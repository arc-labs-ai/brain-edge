//! wire_proxy — the transparent Brain wire-protocol proxy (deployment Option X,
//! Shape B: opaque frame-splice).
//!
//! A customer's SDK speaks the **binary wire protocol** to the edge with its own
//! Brain key. The proxy terminates that TCP connection, opens **one** upstream
//! connection to Brain per customer connection, and splices frames between them
//! **unchanged** — forwarding the customer's own HELLO / AUTH so Brain
//! authenticates the connection as that same credential and returns the real
//! WELCOME / AUTH_OK. The proxy never synthesizes a handshake and never resolves
//! a key to an identity, so it stays entirely out of the isolation TCB: isolation
//! lives 100% in Brain (see `tenancy_industry_scale.md` §11).
//!
//! What the edge adds on the path is **metering** and **rate-limiting**, both
//! driven off the 32-byte frame header alone (opcode + stream_id). The single
//! exception is the handshake AUTH frame, whose CBOR payload is parsed **once**
//! to sniff the credential that keys the rate-limiter and the metering label —
//! nothing else is ever decoded. Frames (and their stream_ids — client op streams
//! must stay odd) are forwarded byte-for-byte, so the future
//! `agent → space / context → session` wire rename is invisible here.
//!
//! Failure modes surface as **wire ERROR frames**, not dropped sockets: a
//! rate-limited op is answered with a `RateLimited` error on its own stream, and
//! an unreachable Brain is answered with a `ShardUnavailable` error on the
//! handshake stream — in both cases the customer's SDK sees a clean structured
//! error instead of a reset.

mod rate_limit;

pub use rate_limit::{RateLimitConfig, RateLimiters};

use std::net::SocketAddr;
use std::sync::Arc;

use brain_db_sdk::transport::{read_frame, write_frame};
use brain_db_sdk::wire::cbor::{from_cbor_bytes, to_cbor_bytes};
use brain_db_sdk::wire::frame::{FLAG_EOS, Frame};
use brain_db_sdk::wire::opcode::Opcode;
use brain_db_sdk::wire::types::{
    AuthCredentials, AuthPayload, ErrorCategoryWire, ErrorCodeWire, ErrorResponse,
};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::port::{MeterEvent, MeteringSink, Outcome};

/// Deploy-time configuration for the wire proxy listener.
#[derive(Clone, Debug)]
pub struct WireProxyConfig {
    /// Address the proxy accepts customer wire connections on.
    pub wire_listen_addr: SocketAddr,
    /// The Brain database listener each customer connection is spliced to.
    pub brain_addr: SocketAddr,
    /// Per-credential token-bucket rate limiting (disabled when capacity is 0).
    pub rate: RateLimitConfig,
}

/// Bind the wire-proxy listener and serve customer connections until the process
/// exits. One task per accepted connection; a bad accept is logged and skipped
/// rather than tearing the listener down.
///
/// # Errors
/// Returns the bind error if `wire_listen_addr` can't be bound.
pub async fn serve(config: WireProxyConfig, meter: Arc<dyn MeteringSink>) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.wire_listen_addr).await?;
    info!(
        addr = %config.wire_listen_addr,
        brain = %config.brain_addr,
        "wire proxy listening"
    );
    let limiters = Arc::new(RateLimiters::new(config.rate));

    loop {
        let (sock, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "wire proxy accept failed");
                continue;
            }
        };
        let config = config.clone();
        let meter = Arc::clone(&meter);
        let limiters = Arc::clone(&limiters);
        tokio::spawn(async move {
            debug!(%peer, "wire proxy connection opened");
            handle_conn(sock, config, meter, limiters).await;
            debug!(%peer, "wire proxy connection closed");
        });
    }
}

/// Splice one customer connection to a fresh upstream Brain connection.
///
/// Opens the 1:1 upstream first (so an unreachable Brain is reported as a wire
/// ERROR before any splicing), then runs two directional pumps: whichever ends
/// first (half-close or peer drop) cancels the other.
async fn handle_conn(
    customer: TcpStream,
    config: WireProxyConfig,
    meter: Arc<dyn MeteringSink>,
    limiters: Arc<RateLimiters>,
) {
    let _ = customer.set_nodelay(true);
    let (cust_rd, cust_wr) = customer.into_split();
    // The customer write half is shared: the b→c pump writes Brain's responses,
    // and the c→b pump writes ERROR frames on rate-limit. A mutex serializes them
    // so frames never interleave on the wire.
    let cust_wr = Arc::new(Mutex::new(cust_wr));

    let brain = match TcpStream::connect(config.brain_addr).await {
        Ok(b) => b,
        Err(e) => {
            warn!(brain = %config.brain_addr, error = %e, "wire proxy: brain unreachable");
            // Report on the handshake stream (0) so the SDK surfaces a clean
            // "unavailable" during connect instead of a bare socket reset.
            let mut w = cust_wr.lock().await;
            let _ = write_frame(
                &mut *w,
                &error_frame(
                    0,
                    ErrorCodeWire::ShardUnavailable,
                    ErrorCategoryWire::Unavailable,
                    "brain upstream unreachable",
                ),
            )
            .await;
            let _ = w.shutdown().await;
            return;
        }
    };
    let _ = brain.set_nodelay(true);
    let (brain_rd, brain_wr) = brain.into_split();

    tokio::select! {
        () = pump_customer_to_brain(cust_rd, brain_wr, Arc::clone(&cust_wr), &meter, &limiters) => {}
        () = pump_brain_to_customer(brain_rd, Arc::clone(&cust_wr)) => {}
    }
}

/// Brain → customer: a pure relay. Every response/event/heartbeat frame is
/// forwarded verbatim; the proxy neither meters nor inspects this direction.
async fn pump_brain_to_customer(mut brain_rd: OwnedReadHalf, cust_wr: Arc<Mutex<OwnedWriteHalf>>) {
    let mut buf = Vec::new();
    loop {
        match read_frame(&mut brain_rd, &mut buf).await {
            Ok(frame) => {
                let mut w = cust_wr.lock().await;
                if write_frame(&mut *w, &frame).await.is_err() {
                    return;
                }
            }
            // Closed / corrupt / EOF: end this direction, which cancels the other.
            Err(_) => return,
        }
    }
}

/// Customer → Brain: sniff AUTH once, meter + rate-limit op frames off the
/// header, then forward every frame unchanged.
async fn pump_customer_to_brain(
    mut cust_rd: OwnedReadHalf,
    mut brain_wr: OwnedWriteHalf,
    cust_wr: Arc<Mutex<OwnedWriteHalf>>,
    meter: &Arc<dyn MeteringSink>,
    limiters: &Arc<RateLimiters>,
) {
    let mut buf = Vec::new();
    // The sniffed credential (rate-limit bucket key) and its non-secret metering
    // label. Set once, when the AUTH frame passes through.
    let mut credential: Option<String> = None;
    let mut label: Option<String> = None;

    loop {
        let frame = match read_frame(&mut cust_rd, &mut buf).await {
            Ok(f) => f,
            Err(_) => return,
        };

        // Handshake sniff — the ONLY place a payload is decoded, and only the AUTH
        // frame, to key the rate-limiter + metering label on the customer's own
        // credential. Every other frame is handled header-only.
        if credential.is_none() && frame.opcode == Opcode::Auth.as_u16() {
            if let Some(token) = sniff_credential(&frame.payload) {
                label = Some(fingerprint(&token));
                credential = Some(token);
            }
        }

        // Meter + rate-limit real ops only; handshake/keepalive frames pass
        // through untouched and uncounted.
        if !is_control_frame(frame.opcode) {
            if let Some(cred) = &credential {
                if !limiters.allow(cred) {
                    // Answer on the op's own stream and do NOT forward — the
                    // socket stays up and the SDK sees a structured error.
                    let ef = error_frame(
                        frame.stream_id,
                        ErrorCodeWire::RateLimited,
                        ErrorCategoryWire::ResourceExhausted,
                        "rate limit exceeded",
                    );
                    {
                        let mut w = cust_wr.lock().await;
                        if write_frame(&mut *w, &ef).await.is_err() {
                            return;
                        }
                    }
                    record(meter, label.as_deref(), frame.opcode, Outcome::Err);
                    continue;
                }
            }
            // Opaque proxy: we can't cheaply correlate the eventual per-stream
            // response, so this meters admission, not success.
            record(meter, label.as_deref(), frame.opcode, Outcome::Ok);
        }

        // Forward byte-for-byte (stream_id and payload untouched). Frame::encode
        // reproduces identical bytes, so re-encoding after decode is loss-free.
        if write_frame(&mut brain_wr, &frame).await.is_err() {
            return;
        }
    }
}

/// Record one metered op via the injected sink.
fn record(meter: &Arc<dyn MeteringSink>, tenant: Option<&str>, opcode: u16, outcome: Outcome) {
    meter.record(&MeterEvent {
        tenant,
        op: op_name(opcode),
        outcome,
    });
}

/// Parse the AUTH payload — the single sanctioned payload decode — to recover the
/// credential the connection presents. Token creds yield the raw token (the same
/// bytes Brain keys identity on); mTLS yields the asserted subject. Returns `None`
/// if the payload doesn't decode (a malformed AUTH the proxy simply forwards for
/// Brain to reject).
fn sniff_credential(payload: &[u8]) -> Option<String> {
    let auth: AuthPayload = from_cbor_bytes(payload).ok()?;
    match auth.credentials {
        AuthCredentials::Token(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        AuthCredentials::Mtls(claim) => Some(claim.asserted_subject),
    }
}

/// A stable, non-secret label for a credential, so the raw Brain key never lands
/// in metering records or logs. Not a security primitive — just a de-identifier.
fn fingerprint(credential: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    credential.hash(&mut h);
    format!("cred-{:016x}", h.finish())
}

/// Build a terminal (EOS) wire ERROR frame on `stream_id`.
fn error_frame(
    stream_id: u32,
    code: ErrorCodeWire,
    category: ErrorCategoryWire,
    message: &str,
) -> Frame {
    let payload = to_cbor_bytes(&ErrorResponse {
        code,
        category,
        message: message.to_string(),
        details: None,
        retry_after_ms: None,
    });
    Frame::new(Opcode::Error.as_u16(), FLAG_EOS, stream_id, payload)
}

/// Handshake and keepalive frames the proxy neither meters nor rate-limits.
fn is_control_frame(opcode: u16) -> bool {
    opcode == Opcode::Hello.as_u16()
        || opcode == Opcode::Auth.as_u16()
        || opcode == Opcode::Bye.as_u16()
        || opcode == Opcode::Ping.as_u16()
        || opcode == Opcode::ClientPong.as_u16()
}

/// A coarse verb name for a request opcode, for the metering label. Unknown
/// opcodes (including any future addition) fall back to `"op"` — the proxy never
/// needs to know the full verb set to forward a frame.
fn op_name(opcode: u16) -> &'static str {
    match opcode {
        o if o == Opcode::EncodeReq.as_u16() => "encode",
        o if o == Opcode::EncodeVectorDirectReq.as_u16() => "encode_vector",
        o if o == Opcode::RecallReq.as_u16() => "recall",
        o if o == Opcode::PlanReq.as_u16() => "plan",
        o if o == Opcode::ReasonReq.as_u16() => "reason",
        o if o == Opcode::ForgetReq.as_u16() => "forget",
        o if o == Opcode::LinkReq.as_u16() => "link",
        o if o == Opcode::UnlinkReq.as_u16() => "unlink",
        o if o == Opcode::MemoryListReq.as_u16() => "memory_list",
        o if o == Opcode::MemoryInspectReq.as_u16() => "memory_inspect",
        o if o == Opcode::SubscribeReq.as_u16() => "subscribe",
        o if o == Opcode::UnsubscribeReq.as_u16() => "unsubscribe",
        o if o == Opcode::TxnBegin.as_u16() => "txn_begin",
        o if o == Opcode::TxnCommit.as_u16() => "txn_commit",
        o if o == Opcode::TxnAbort.as_u16() => "txn_abort",
        o if o == Opcode::GetCapabilitiesReq.as_u16() => "get_capabilities",
        _ => "op",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_db_sdk::wire::types::{AuthMethod, MtlsClaim};

    #[test]
    fn sniffs_a_token_credential() {
        let payload = to_cbor_bytes(&AuthPayload {
            method: AuthMethod::Token,
            credentials: AuthCredentials::Token(b"secret-key".to_vec()),
        });
        assert_eq!(sniff_credential(&payload).as_deref(), Some("secret-key"));
    }

    #[test]
    fn sniffs_an_mtls_subject() {
        let payload = to_cbor_bytes(&AuthPayload {
            method: AuthMethod::Mtls,
            credentials: AuthCredentials::Mtls(MtlsClaim {
                cert_fingerprint: [7u8; 32],
                asserted_subject: "spiffe://acme/edge".into(),
            }),
        });
        assert_eq!(
            sniff_credential(&payload).as_deref(),
            Some("spiffe://acme/edge")
        );
    }

    #[test]
    fn malformed_auth_payload_sniffs_to_none() {
        assert_eq!(sniff_credential(b"not-cbor-at-all"), None);
    }

    #[test]
    fn fingerprint_is_stable_and_hides_the_raw_key() {
        let a = fingerprint("super-secret");
        let b = fingerprint("super-secret");
        assert_eq!(a, b, "same credential → same label");
        assert!(!a.contains("super-secret"), "raw key is never exposed");
        assert_ne!(fingerprint("other"), a);
    }

    #[test]
    fn control_frames_are_not_metered() {
        for op in [
            Opcode::Hello,
            Opcode::Auth,
            Opcode::Bye,
            Opcode::Ping,
            Opcode::ClientPong,
        ] {
            assert!(is_control_frame(op.as_u16()), "{op:?} is control");
        }
        assert!(!is_control_frame(Opcode::EncodeReq.as_u16()));
        assert!(!is_control_frame(Opcode::RecallReq.as_u16()));
    }

    #[test]
    fn op_names_cover_the_common_verbs_and_default() {
        assert_eq!(op_name(Opcode::EncodeReq.as_u16()), "encode");
        assert_eq!(op_name(Opcode::RecallReq.as_u16()), "recall");
        assert_eq!(op_name(0xFFFF), "op");
    }

    #[test]
    fn error_frame_round_trips_as_a_terminal_error() {
        let f = error_frame(
            5,
            ErrorCodeWire::RateLimited,
            ErrorCategoryWire::ResourceExhausted,
            "rate limit exceeded",
        );
        assert_eq!(f.opcode, Opcode::Error.as_u16());
        assert_eq!(f.stream_id, 5);
        assert!(f.flags & FLAG_EOS != 0, "error frame terminates the stream");
        let decoded: ErrorResponse = from_cbor_bytes(&f.payload).expect("decode error payload");
        assert_eq!(decoded.code, ErrorCodeWire::RateLimited);
        assert_eq!(decoded.message, "rate limit exceeded");
    }
}
