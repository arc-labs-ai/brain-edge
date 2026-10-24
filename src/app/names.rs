//! Display names for the entity ids a read references.
//!
//! A statement is `(subject, predicate, object)` where subject and object are
//! entity ids, and a relation is `(from_entity, to_entity)`. Returning only the
//! ids leaves every consumer to resolve them itself just to print a label — a
//! second call per id, on a read it already made. Clients hold names for
//! whatever graph page they happen to have loaded and show raw UUIDs for the
//! rest, which is what this exists to stop.
//!
//! Ported from the cloud gateway, which has carried it since the dashboard
//! needed it, so a self-hosted edge and the hosted API answer the same shape.
//! The gateway's forked data plane is where the two drifted apart; the names
//! were on one side and not the other, and nothing compared them.
//!
//! What a caller gets is deliberately best-effort: a name is an convenience on
//! top of the id, so an id that cannot be resolved is simply left unnamed
//! rather than failing the read it was decorating.

use std::collections::HashMap;
use std::sync::Arc;

use brain_db_sdk::BrainClient;
use brain_db_sdk::wire::types::{EntityGetRequest, WireUuid};
use tokio::task::JoinSet;

/// How many distinct ids one response will resolve.
///
/// A bound, not a tuning knob: name resolution is one engine call per unique
/// id, so an unbounded page would turn a single list into hundreds of round
/// trips. Past the cap the remaining ids come back unnamed, which is the same
/// outcome as an unresolvable one and already handled.
const NAME_RESOLVE_CAP: usize = 128;

/// What the space's own entity is called in a response.
///
/// Every space has a self-entity named `space:<32 hex>`, which is an internal
/// handle, not something to show a person.
pub const SELF_ENTITY_LABEL: &str = "This space (self)";

/// The name to show for an entity, given its canonical name.
#[must_use]
pub fn display_entity_name(name: String) -> String {
    match name.strip_prefix("space:") {
        Some(hex) if hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            SELF_ENTITY_LABEL.to_string()
        }
        _ => name,
    }
}

/// Resolve display names for `ids`, concurrently.
///
/// Deduplicated first, because a page of statements about one subject would
/// otherwise look it up once per row. The lookups run together rather than in
/// sequence: they are independent, they share one multiplexed connection, and
/// serially a 40-id page would cost 40 round trips end to end.
///
/// An id that does not resolve — tombstoned, merged away, or simply not
/// readable by this caller — is absent from the map, and the field it would
/// have filled stays `null`.
pub async fn entity_names(
    client: &Arc<BrainClient>,
    ids: impl IntoIterator<Item = WireUuid>,
) -> HashMap<WireUuid, String> {
    let mut unique: Vec<WireUuid> = ids.into_iter().collect();
    unique.sort_unstable();
    unique.dedup();
    unique.truncate(NAME_RESOLVE_CAP);
    if unique.is_empty() {
        return HashMap::new();
    }

    let mut lookups = JoinSet::new();
    for entity_id in unique {
        let client = Arc::clone(client);
        lookups.spawn(async move {
            let name = client
                .get_entity(&EntityGetRequest {
                    entity_id,
                    act_as: None,
                })
                .await
                .ok()
                .map(|r| display_entity_name(r.entity.canonical_name));
            (entity_id, name)
        });
    }

    let mut out = HashMap::new();
    while let Some(joined) = lookups.join_next().await {
        // A panicked lookup task is reported as a join error. It leaves that
        // one id unnamed, like any other failure to resolve, rather than
        // taking down the read.
        if let Ok((id, Some(name))) = joined {
            out.insert(id, name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_self_entity_gets_a_readable_label() {
        assert_eq!(
            display_entity_name(format!("space:{}", "ab".repeat(16))),
            SELF_ENTITY_LABEL
        );
    }

    #[test]
    fn an_ordinary_name_is_passed_through() {
        assert_eq!(display_entity_name("Ada Lovelace".into()), "Ada Lovelace");
    }

    #[test]
    fn a_near_miss_on_the_self_pattern_is_not_relabelled() {
        // Right prefix, wrong shape: these are names someone could genuinely
        // have chosen, and relabelling them would hide real data.
        for name in [
            "space:short",
            "space:",
            &format!("space:{}", "ab".repeat(15)),
            &format!("space:{}", "zz".repeat(16)),
        ] {
            assert_eq!(display_entity_name(name.to_string()), name);
        }
    }
}
