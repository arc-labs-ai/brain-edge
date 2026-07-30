# brain-edge

An **HTTP/JSON edge for the [Brain](https://github.com/arc-labs-ai/brain-db) memory
database** — the open-source, self-hostable front door.

Brain speaks a binary wire protocol (CBOR over TCP). `brain-edge` accepts the
**same HTTP/JSON `/v1/*` API the Arc cloud hosts** and translates it to the wire
protocol using the Brain SDK. Run it next to `brain`, point any HTTP
client at it, and your self-hosted experience matches the cloud — only the base
URL differs.

```
your app ──HTTP/JSON──▶ brain-edge ──wire/CBOR──▶ brain
   (curl / Brain HTTP SDK)         (this)          (the database)
```

`brain-edge` does **not** re-implement the wire protocol — it's a thin
[axum](https://github.com/tokio-rs/axum) server on top of the `brain-db-sdk`
Rust client, so there's one wire implementation, verified against Brain's
conformance corpus.

## API

Auth: `Authorization: Bearer <key>` (or `X-API-Key`). The key is forwarded to
Brain as its wire credential — Brain resolves `(namespace, agent, permissions)`
from it, so your key's scoping is honored end to end.

### Memory, reasoning, identity

| Method   | Path              | Body / query                               | Returns |
| -------- | ----------------- | ------------------------------------------ | ------- |
| `POST`   | `/v1/memories`    | `{ text, context?, occurred_at? }`         | `{ memory_id, was_deduplicated, salience, kind, created_at_unix_nanos, auto_edges_added }` |
| `GET`    | `/v1/memories`    | `limit?` (≤100, default 50), `cursor?`, `dir?` (`desc` default \| `asc`), `include_tombstoned?` | `{ items: [...], next_cursor? }` — `next_cursor` is omitted on the last page |
| `DELETE` | `/v1/memories`    | `{ memory_id, hard? }`                     | `{ memory_id, was_already_forgotten, edges_removed }` |
| `GET`    | `/v1/memories/{id}/inspect` | —                                | `{ found, memory_id, text, artifact }` — the per-stage write-pipeline output (embedding, extraction, HyPE questions, extracted graph) |
| `POST`   | `/v1/recall`      | `{ query \| cue, max_results?, subject? }` | `{ answer_kind, memories: [{ memory_id, text, similarity_score, confidence, salience, kind, created_at_unix_nanos }] }` |
| `POST`   | `/v1/links`       | `{ source, target, kind, weight? }`        | `{ source, target, kind, weight, created_at_unix_nanos, already_existed }` |
| `DELETE` | `/v1/links`       | `{ source, target, kind }`                 | `{ source, target, kind, removed }` |
| `POST`   | `/v1/plan`        | `{ start:{text\|memory_id}, goal:{…}, max_steps?, strategy? }` | `{ steps: [{ step_index, memory_id, text, transition_kind, confidence, estimated_distance_to_goal }] }` |
| `POST`   | `/v1/reason`      | `{ observation:{text\|memory_id}, depth?, confidence_threshold?, max_inferences? }` | `{ inferences: [{ step_index, claim, supporting_memories, contradicting_memories, confidence, inference_kind }] }` |
| `GET`    | `/v1/whoami`      | —                                          | `{ namespace, space_id, permissions }` |
| `GET`    | `/v1/capabilities`| —                                          | `{ rerank, llm_extractor, classifier_extractor, pattern_extractor, schema_namespaces, vector_dim }` |
| `GET`    | `/health/live`    | —                                          | `200` (process is up) |
| `GET`    | `/health/ready`   | —                                          | `200` when Brain is reachable, else `503` |

Edge kinds: `caused` · `followed_by` · `derived_from` · `similar_to` · `contradicts` · `supports` · `references` · `part_of`.

Memory ids are decimal strings (they exceed the JS safe-integer range).

### Typed graph

The entity/relation/statement layer Brain builds from your memories. Entity,
relation and statement ids are **UUID strings**.

| Method | Path                             | Body / query                                                                 | Returns |
| ------ | -------------------------------- | ---------------------------------------------------------------------------- | ------- |
| `POST` | `/v1/entities`                   | `{ entity_type_id, canonical_name, aliases? }`                               | `{ entity_id }` |
| `GET`  | `/v1/entities`                   | `type_id?`, `prefix?`, `mention_count_min?`, `include_tombstoned?`, `include_merged?`, `limit?` (≤1000, default 100) | `{ entities: [...], count }` |
| `POST` | `/v1/entities/resolve`           | `{ candidate_name, resolution_context?, type_hint?, allow_create? }`         | `{ outcome (resolved\|created\|ambiguous\|not_found), tier, confidence, entity_id, candidate_ids }` |
| `GET`  | `/v1/entities/{id}`              | —                                                                            | `{ entity_id, entity_type_id, canonical_name, aliases, mention_count, created_at_unix_nanos, … }` |
| `POST` | `/v1/entities/{id}/traverse`     | `{ direction? (outgoing\|incoming\|both), relation_types?, max_depth? (≤5, default 3), max_nodes? (≤1000, default 100), include_superseded? }` | `{ paths: [{ steps: [...] }], total_paths, truncated }` |
| `GET`  | `/v1/entities/{id}/relations`    | `direction?` (`from`\|`to`), `type?`, `include_superseded?`, `include_tombstoned?`, `limit?` | `{ relations: [...], count }` |
| `GET`  | `/v1/relations/{id}`             | `follow_supersession?` (default `true`)                                      | the relation, or the head of its supersession chain |
| `GET`  | `/v1/statements`                 | `subject?`, `predicate?`, `kind?`, `min_confidence?`, `only_current?` (default `true`), `include_tombstoned?`, `limit?` | `{ statements: [...], count }` |
| `GET`  | `/v1/statements/{id}`            | `follow_supersession?` (default `true`)                                      | the statement, or the head of its supersession chain |
| `GET`  | `/v1/graph`                      | `limit?` (≤500, default 200), `cursor?`, `include_statements?`, `include_memories?`, `include_memory_edges?`, `include_tombstoned?` | `{ nodes, edges, next_cursor? }` — a paged node/edge export |

`include_memory_edges` requires `include_memories`; asking for it alone returns
`400` rather than a relayed wire error, because the edges' endpoints wouldn't be
in the page.

### Schema

| Method | Path                  | Body / query                                  | Returns |
| ------ | --------------------- | --------------------------------------------- | ------- |
| `GET`  | `/v1/schema`          | `namespace?`, `version?` (`0`/omitted = active) | `{ namespace, schema_version, schema_document, uploaded_at_unix_nanos, validator_version }` |
| `POST` | `/v1/schema`          | `{ schema_document, dry_run?, allow_breaking? }` | `{ namespace, schema_version, backward_compatible, validation_errors }` |
| `POST` | `/v1/schema/validate` | `{ schema_document }`                          | `{ namespace, would_be_version, validation_errors }` — never touches storage |
| `PUT`  | `/v1/schema`          | `{ schema_document, force_drop_existing }`     | `{ namespace, schema_version, dropped_count, validation_errors }` |

`POST` **merges** into the active schema — additive and versioned. A
`schema_version` of `0` in the reply means the upload was rejected: either
validation failed, or `dry_run` was set.

⚠️ **`PUT` is destructive.** It drops every declared row in the namespace before
the new document lands; `dropped_count` is how many went. Entities whose type
disappears survive as orphans — still readable as plain memories, no longer
enriched from the typed-graph tables. `force_drop_existing` has no default and
must be `true`; the edge returns `400` naming the field otherwise, so an empty
body can't reach an irreversible operation. `PUT` rather than `POST` because the
method difference is the clearest available signal that this one is not additive.

Listing schema *versions* is not exposed: it's a streaming verb, and the edge
either flattens streaming verbs or omits them rather than half-exposing them.
It's operator introspection against the admin listener, not a data-plane call.

## Configuration (env)

| Var                             | Default          | Meaning                                            |
| ------------------------------- | ---------------- | -------------------------------------------------- |
| `BRAIN_EDGE_LISTEN`             | `0.0.0.0:8080`   | HTTP listen address                                |
| `BRAIN_ADDR`                    | `127.0.0.1:7878` | Brain address — `ip:port` or a DNS `host:port` (e.g. `brain:7878`), resolved once at startup |
| `BRAIN_EDGE_POOL_SIZE`          | `4`              | Connection-pool width per credential (must be ≥ 1) |
| `BRAIN_EDGE_MAX_CREDENTIALS`    | `256`            | Cap on distinct cached credential pools (LRU-evicted) |
| `BRAIN_EDGE_IDLE_TTL_SECS`      | `900`            | Idle credential pools are swept after this many seconds |
| `BRAIN_EDGE_REQUEST_TIMEOUT_SECS` | `30`           | Per-request timeout; a stalled request returns `408` |
| `BRAIN_EDGE_MAX_BODY_BYTES`     | `1048576`        | Max request body size (1 MiB); larger returns `413` |
| `BRAIN_EDGE_WIRE_LISTEN`        | unset            | Wire-proxy listen address — `ip:port` or `host:port`. **Unset = the wire proxy is off**, HTTP only |
| `BRAIN_EDGE_WIRE_RATE_CAPACITY` | `0`              | Per-credential token-bucket burst on the wire proxy (`0` = no limit) |
| `BRAIN_EDGE_WIRE_RATE_REFILL_PER_SEC` | `0`        | Per-credential refill rate, ops/sec (`0` = no limit) |
| `RUST_LOG`                      | `brain_edge=info`| Log filter                                          |

## What a failing request looks like

Every response carries an **`x-request-id`** header. A caller-supplied one is
echoed, so an id assigned by a load balancer upstream survives the hop;
otherwise the edge mints one. It is set on every response, including the ones
generated before a handler runs — the `413` from the body cap and the `408` from
the timeout.

Errors are always the same envelope:

```json
{ "error": { "code": "bad_request", "message": "k must be 1..=100" } }
```

**A `4xx` message describes your request** and is safe to act on. **A `5xx`
message does not**: statuses in that range mean the edge or the engine behind it
is in trouble, and the detail — an OS error, a shard's internal state, a
frame-codec fault — describes the edge's conversation with its database rather
than anything the caller can fix. That detail is written to the log alongside the
request id instead, so quoting the `x-request-id` from a failed call is enough
for an operator to find it.

A panicking handler returns a `500` in this same envelope rather than dropping
the connection, and does not take the process down.

**The wire proxy** is opt-in and separate from the HTTP surface. With
`BRAIN_EDGE_WIRE_LISTEN` set, the edge also accepts raw Brain **wire/CBOR**
connections on that port and splices frames to `BRAIN_ADDR` **byte-for-byte** —
the customer's own HELLO/AUTH included, so Brain authenticates the connection as
that same credential and isolation stays entirely in Brain. What the edge adds on
the path is metering and rate limiting, driven off the 32-byte frame header. A
native `brain-db-sdk` client therefore gets the full wire protocol through the
same process, and the same limits, as the HTTP callers. See
[`examples/wire_proxy_smoke.rs`](./examples/wire_proxy_smoke.rs).

Malformed numeric values (e.g. `BRAIN_EDGE_POOL_SIZE=abc`) or a zero where a
positive is required fail startup loudly rather than silently falling back to
the default. On `SIGTERM`/`SIGINT` the server drains in-flight requests before
exiting.

## Run it

```bash
# next to a running brain (wire on :7878)
BRAIN_ADDR=127.0.0.1:7878 cargo run

# then, from any HTTP client:
curl -s localhost:8080/v1/whoami -H "Authorization: Bearer $BRAIN_KEY"
curl -s -X POST localhost:8080/v1/memories \
  -H "Authorization: Bearer $BRAIN_KEY" -H 'content-type: application/json' \
  -d '{"text":"the sky is teal today"}'
curl -s -X POST localhost:8080/v1/recall \
  -H "Authorization: Bearer $BRAIN_KEY" -H 'content-type: application/json' \
  -d '{"query":"what color is the sky?"}'
```

Schema changes are worth dry-running first — `/v1/schema/validate` reports the
same diagnostics as an upload without persisting anything:

```bash
curl -s -X POST localhost:8080/v1/schema/validate \
  -H "Authorization: Bearer $BRAIN_KEY" -H 'content-type: application/json' \
  -d '{"schema_document":"entity Person { name: string }"}'

# clean? then merge it (additive, versioned)
curl -s -X POST localhost:8080/v1/schema \
  -H "Authorization: Bearer $BRAIN_KEY" -H 'content-type: application/json' \
  -d '{"schema_document":"entity Person { name: string }"}'
```

## Self-host the whole stack

See [`docker-compose.yml`](./docker-compose.yml): it runs `brain` +
`brain-edge`. `docker compose up`, then point the Brain HTTP SDK (or curl) at
`http://localhost:8080`. That's the same code you'd run against the Arc cloud —
just a different URL.

## Self-hosting with Docker

The [`Dockerfile`](./Dockerfile) + [`docker-compose.yml`](./docker-compose.yml)
are a self-host **starter** — read them and adapt to your host before relying on
them.

```bash
# from this directory (brain-edge/)
docker compose up
# then, from any HTTP client:
curl -s localhost:8080/v1/whoami -H "Authorization: Bearer $BRAIN_KEY"
```

**Build context is the parent, on purpose.** `brain-edge` depends on a sibling
crate via a path dependency — `brain-db-sdk = { path = "../brain-sdk/rust" }`.
A Docker context scoped to `brain-edge/` alone can't see `../brain-sdk`, so the
Dockerfile is built with the **arc-labs parent** as the context
(`context: ..` in compose). It `COPY`s both `brain-edge/` and `brain-sdk/`,
preserving their relative layout so the path dep resolves. To build by hand:

```bash
cd ..                       # the arc-labs parent
docker build -f brain-edge/Dockerfile -t brain-edge:latest .
```

**Env vars:** the full set is in [Configuration (env)](#configuration-env)
above (the single source, defined in [`src/config/mod.rs`](./src/config/mod.rs)).
Under compose, `BRAIN_ADDR` is the Brain **service DNS name** `brain:7878` —
resolved at startup, so no hard-coded IP is needed.

**Caveat — you must provide the `brain` image yourself.** Brain is
Linux-only (glommio/io_uring) and lives in a **separate repo** (`~/Desktop/brain`),
so this compose references it as an `image: brain:latest` placeholder
rather than building it. Build it there first:

```bash
cd ~/Desktop/brain && docker build -t brain:latest .
```

The server also needs `seccomp:unconfined` (io_uring), a bind-mount for the
embedding-model dir plus `BRAIN_EMBED_MODEL_DIR`, and a valid LLM api key — the
rerank / classifier / llm tiers **hard-fail at boot** if their models or key are
missing. See the comments in `docker-compose.yml`.

## Scope

The edge covers the **request/response data plane**: memory (encode, recall,
forget, list, inspect), reasoning (plan, reason), the memory graph, the typed
graph (entities, relations, statements, export), and schema. That is the surface
most integrations need, and it's reachable with nothing but curl.

**What it deliberately does not expose:**

- **Open-ended streams** — `SUBSCRIBE` / `UNSUBSCRIBE`, and the schema *version*
  listing. The rule is flatten it or omit it, never half-expose it: where a verb
  streams but terminates (traverse, list, graph export) the edge folds the frames
  into one JSON body; where it doesn't, it's left off.
- **Transactions** — `TXN_BEGIN` / `TXN_COMMIT` / `TXN_ABORT` are scoped to a
  connection, and HTTP requests don't own one. Handing out a transaction id over
  a pooled connection would be a correctness lie, not a convenience.

For those, and for anything latency-sensitive, use the native `brain-db-sdk`
client — either against `brain` directly, or through this edge's opt-in
[wire proxy](#configuration-env), which gives you the full protocol while still
passing through the edge's metering and rate limiting.
