//! The change feed: `GET /v1/subscribe` (SUBSCRIBE, `0x0030`), as SSE.
//!
//! # Why SSE and not a WebSocket
//!
//! After the request frame the wire op is server-push only — Brain emits
//! `SUBSCRIBE_EVENT` until the stream is torn down and the client says nothing
//! more. That is precisely what Server-Sent Events are, and SSE needs no
//! protocol upgrade, survives ordinary proxies, and is readable with
//! `curl -N`. A WebSocket would buy bidirectionality the op does not have.
//!
//! # Teardown is the whole design
//!
//! `Subscription::drop` removes the client-side route but **cannot** send
//! UNSUBSCRIBE — `Drop` is not async, and the SDK says so in as many words.
//! So a handler that simply let the handle drop on client disconnect would
//! leave Brain pushing events at a stream id nobody is reading.
//!
//! Hence the shape below: a spawned task OWNS the subscription and forwards
//! events into a bounded channel; the SSE body streams the receiver. When the
//! client goes away axum drops the body, which drops the receiver, which makes
//! the task's `send` fail — and the task then has an async context in which to
//! call `unsubscribe().await` before exiting. Teardown happens on disconnect
//! without anyone having to remember to ask for it.
//!
//! The channel is bounded, so a slow reader backpressures the wire stream
//! rather than letting brain-edge buffer a feed without limit. That is the
//! same budget `max_inflight` expresses, enforced on this side too.
//!
//! # Unsubscribe is disconnect, deliberately
//!
//! There is no `DELETE /v1/subscriptions/{id}`. A stream id is meaningful only
//! on the one pooled connection the subscription lives on, so a teardown
//! request would have to find that connection — and across replicas it would
//! land on a different process entirely. Closing the HTTP response is the
//! portable way to say "stop", and it is the one a browser's `EventSource`
//! already does on `.close()`.
//!
//! What a caller loses that way is where the feed got to, so the stream ends
//! with a `closed` event carrying `final_lsn` from UNSUBSCRIBE's own response.
//! Together with per-event `id:`/`Last-Event-ID` resumption, a dropped feed
//! resumes exactly rather than guessing.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use futures_core::Stream;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::dto::subscribe::{SubscribeQuery, SubscriptionClosedDto, SubscriptionEventDto};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// How many events may sit between Brain and a slow HTTP reader before the
/// forwarding task stops draining and the wire stream backpressures.
const CHANNEL_DEPTH: usize = 256;

/// Comment frames keep an idle feed from being reaped by an intermediary that
/// times out quiet connections. 15s is comfortably under the usual 30–60s.
///
/// Emitted by the forwarding task rather than by `Sse::keep_alive`, which
/// wraps the body in a `KeepAliveStream<_>` and so would erase the item type
/// from the handler's signature — the one place `tools/http_manifest.py` can
/// read which DTO this route streams.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// The SSE body: a `Stream` over the forwarding task's channel.
///
/// Generic in its item so `tools/http_manifest.py` can read the event DTO
/// straight off the handler's return type — `Sse<SseStream<SubscriptionEventDto>>`
/// names the contract in the signature rather than hiding it behind
/// `impl Stream`.
#[derive(Debug)]
pub struct SseStream<T> {
    rx: mpsc::Receiver<Result<Event, Infallible>>,
    // `fn() -> T` rather than `T`: the marker records which DTO this streams
    // without making the stream's `Unpin` depend on it. A bare
    // `PhantomData<T>` would require `T: Unpin` in `poll_next`, which is a
    // bound on a type that is never actually held here.
    _item: std::marker::PhantomData<fn() -> T>,
}

impl<T> SseStream<T> {
    fn new(rx: mpsc::Receiver<Result<Event, Infallible>>) -> Self {
        Self {
            rx,
            _item: std::marker::PhantomData,
        }
    }
}

impl<T> Stream for SseStream<T> {
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Every field is `Unpin`, so the pin carries no obligation here.
        self.get_mut().rx.poll_recv(cx)
    }
}

/// `GET /v1/subscribe` — the caller's change feed, as Server-Sent Events.
///
/// Each event's SSE `id:` is its LSN, so a client reconnecting with
/// `Last-Event-ID` resumes from the next event. The stream ends with a
/// `closed` event carrying `final_lsn`.
pub async fn subscribe(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<SubscribeQuery>,
) -> Result<Sse<SseStream<SubscriptionEventDto>>, ApiError> {
    // An automatic browser reconnect resends the last id it saw; the original
    // `?from_lsn=` is still in the URL and would rewind the feed, so the
    // header wins.
    let resume_from = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());

    let req = query
        .to_request(resume_from)
        .map_err(ApiError::bad_request)?;

    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.subscribe(&req).await;
    state.record("subscribe", &ident, Outcome::of(&out));
    let mut subscription = out.map_err(|e| ApiError::from_brain(&e))?;

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(CHANNEL_DEPTH);

    // The task owns the subscription for exactly as long as someone is
    // reading. `client` is moved in with it so the pooled connection the
    // stream lives on cannot be reclaimed underneath it.
    tokio::spawn(async move {
        let _client = client;
        let mut reason = "ended";

        let mut ticker = tokio::time::interval(KEEPALIVE);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate; a comment before any event is noise.
        ticker.tick().await;

        loop {
            let next = tokio::select! {
                // A quiet feed still has to prove the connection is alive, or
                // an intermediary reaps it and the client sees a silent drop.
                _ = ticker.tick() => {
                    if tx.send(Ok(Event::default().comment("keepalive"))).await.is_err() {
                        reason = "unsubscribed";
                        break;
                    }
                    continue;
                }
                next = subscription.next() => next,
            };
            let Some(next) = next else { break };
            let event = match next {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "subscription stream failed");
                    reason = "error";
                    break;
                }
            };
            let dto = SubscriptionEventDto::from(event);
            let lsn = dto.lsn;
            let frame = match Event::default().id(lsn.to_string()).json_data(&dto) {
                Ok(f) => f,
                Err(e) => {
                    // Serializing our own DTO cannot normally fail; treat it
                    // as fatal for this feed rather than silently skipping an
                    // event and leaving a hole in the LSN sequence.
                    warn!(error = %e, "could not serialize a subscription event");
                    reason = "error";
                    break;
                }
            };
            // The only exit that matters: the receiver is gone, so the HTTP
            // client disconnected. Fall through to the teardown below.
            if tx.send(Ok(frame)).await.is_err() {
                reason = "unsubscribed";
                break;
            }
        }

        // The async teardown `Drop` cannot do. Best-effort: a feed that ended
        // because Brain closed it has nothing left to unsubscribe from, and a
        // dead connection cannot carry the request either.
        let final_lsn = match subscription.unsubscribe().await {
            Ok(resp) => resp.final_lsn,
            Err(e) => {
                debug!(error = %e, "unsubscribe on teardown did not complete");
                0
            }
        };

        // Best-effort: on a client disconnect there is nobody to receive it.
        if let Ok(frame) = Event::default()
            .event("closed")
            .json_data(&SubscriptionClosedDto { final_lsn, reason })
        {
            let _ = tx.send(Ok(frame)).await;
        }
    });

    Ok(Sse::new(SseStream::new(rx)))
}
