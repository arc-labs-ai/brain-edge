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
| `PUT`  | `/v1/entities/{id}`              | `{ canonical_name, aliases?, attributes? }`                                  | the updated entity — a **wholesale** replace: an omitted `aliases` clears them |
| `POST` | `/v1/entities/{id}/rename`       | `{ new_canonical_name, move_to_alias? }`                                     | the updated entity — keeps the old name as an alias by default, which is how a mention in older text still resolves |
| `POST` | `/v1/entities/{id}/merge`        | `{ merged, confidence?, reason? }`                                           | `{ audit_id, grace_period_seconds }` — folds `merged` into `{id}`; not symmetric |
| `POST` | `/v1/entities/{id}/unmerge`      | —                                                                            | `{ restored_entity_id }` — pass the **absorbed** id, not the survivor's |
| `DELETE` | `/v1/entities/{id}`            | `reason?`                                                                    | `{ tombstoned_at_unix_nanos }` |
| `POST` | `/v1/statements`                 | `{ kind, subject, predicate, object, confidence?, event_at_unix_nanos?, valid_from_unix_nanos?, valid_to_unix_nanos?, extractor_id?, schema_version?, session_id?, evidence? }` | `{ statement_id, auto_superseded, chain_root }` |
| `POST` | `/v1/statements/{id}/supersede`  | `{ statement }`                                                              | `{ new_statement_id, chain_root, version }` — keeps both and the link between them |
| `POST` | `/v1/statements/{id}/retract`    | `reason?`, `message?`                                                        | `{ retracted_at_unix_nanos, will_zero_at_unix_nanos }` — the **content** does not survive |
| `DELETE` | `/v1/statements/{id}`          | `reason?`, `message?`                                                        | `{ tombstoned_at_unix_nanos }` — soft; the row survives for audit |
| `GET`  | `/v1/statements/{id}/history`    | `limit?`, `cursor?`                                                          | `{ items, chain_root, total_versions, next_cursor }` — the supersession chain, oldest first |
| `POST` | `/v1/relations`                  | `{ relation_type, from_entity, to_entity, confidence?, valid_from_unix_nanos?, valid_to_unix_nanos?, extractor_id?, session_id?, evidence?, properties? }` | `{ relation_id }` |
| `POST` | `/v1/relations/{id}/supersede`   | `{ relation }`                                                               | `{ new_relation_id, version }` — `properties` is replaced, not merged |
| `DELETE` | `/v1/relations/{id}`           | `reason?`                                                                    | `{ tombstoned_at_unix_nanos }` — traversals stop crossing it |
| `GET`  | `/v1/graph`                      | `limit?` (≤500, default 200), `cursor?`, `include_statements?`, `include_memories?`, `include_memory_edges?`, `include_tombstoned?` | `{ nodes, edges, next_cursor? }` — a paged node/edge export |

`include_memory_edges` requires `include_memories`; asking for it alone returns
`400` rather than a relayed wire error, because the edges' endpoints wouldn't be
in the page.

Predicates and relation types are always `namespace:name` — `acme:prefers`,
never `prefers`. A bare name is rejected. Supersede rather than delete when
something changed: `DELETE` keeps the row for audit but loses the history of
having believed it, and `retract` destroys the content outright.

### Schema

| Method | Path                  | Body / query                                  | Returns |
| ------ | --------------------- | --------------------------------------------- | ------- |
| `GET`  | `/v1/schema`          | `namespace?`, `version?` (`0`/omitted = active) | `{ namespace, schema_version, schema_document, uploaded_at_unix_nanos, validator_version }` |
| `POST` | `/v1/schema`          | `{ schema_document, dry_run?, allow_breaking? }` | `{ namespace, schema_version, backward_compatible, validation_errors }` |
| `POST` | `/v1/schema/validate` | `{ schema_document }`                          | `{ namespace, would_be_version, validation_errors }` — never touches storage |
| `PUT`  | `/v1/schema`          | `{ schema_document, force_drop_existing }`     | `{ namespace, schema_version, dropped_count, validation_errors }` |
| `GET`  | `/v1/schemas`         | `limit?`, `cursor?`                            | `{ namespace, items, total, next_cursor }` — the version history, as opposed to `/v1/schema`'s one active document |
| `DELETE` | `/v1/schema/{kind}/{name}` | `force?`                                  | `{ namespace, schema_version, target_kind, target_name, dropped, live_rows, validation_errors }` |
| `GET`  | `/v1/extractors`      | —                                              | `{ items, total }` — what reads a new memory and what it may produce |

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

`DELETE /v1/schema/{kind}/{name}` removes one declared `predicate` or
`relation_type` — the only two droppable kinds. `force` is not a confirmation
flag: it consents specifically to orphaning live rows, and `live_rows` reports
how many it did.

### Sessions and spaces

A **space** is the per-request isolation unit; a **session** groups one
conversation's memories inside it. No space verb takes a path parameter,
because all three act on the caller's *own* space — resolved from the
credential, never named in the request.

| Method   | Path                | Body / query        | Returns |
| -------- | ------------------- | ------------------- | ------- |
| `POST`   | `/v1/sessions`      | `{ session_id, title? }` | `{ session_id, created, created_at_unix_nanos }` — idempotent on the id you choose |
| `GET`    | `/v1/sessions`      | `limit?`            | `{ space_id, sessions, count }` |
| `DELETE` | `/v1/sessions/{id}` | `hard?`             | `{ session_id, erased, memories_removed }` — soft by default, which leaves the memories in place |
| `POST`   | `/v1/spaces`        | —                   | `{ space, created, created_at_unix_nanos }` |
| `GET`    | `/v1/spaces`        | `limit?`            | `{ spaces, count }` |
| `DELETE` | `/v1/spaces`        | `hard?`             | `{ space, erased, memories_removed }` |

⚠️ **`DELETE /v1/spaces` erases the caller's whole workspace.** There is no id
to get wrong and no id to get right — it acts on whichever space the credential
resolves to. `whoami` is the way to check which one that is first.

### Query introspection

| Method | Path                 | Body / query                  | Returns |
| ------ | -------------------- | ----------------------------- | ------- |
| `POST` | `/v1/query/explain`  | `{ text, max_results? }`      | the plan and its estimated cost, **without running the query** |
| `POST` | `/v1/query/trace`    | `{ text, max_results? }`      | what each stage actually cost — note this **runs** the query; it is not the free counterpart to `explain` |

### Procedural, direct vectors, change feed

| Method | Path                            | Body / query                                     | Returns |
| ------ | ------------------------------- | ------------------------------------------------ | ------- |
| `POST` | `/v1/procedural/materialize`    | `top_k?`, `min_confidence?`, `categories?`       | the workspace's learned standing instructions as a system-prompt block, with the statement ids behind each line |
| `POST` | `/v1/memories/vector`           | `{ vector, text?, context?, occurred_at? }`      | same shape as `POST /v1/memories` — for a caller that computed its own embedding |
| `GET`  | `/v1/subscribe`                 | `spaces?`, `kinds?`                              | **Server-Sent Events**: one `SubscriptionEventDto` per change as it lands |

`POST /v1/memories/vector` bypasses the edge's own embedding step, so the
vector's dimension must match what `/v1/capabilities` reports as `vector_dim`;
a mismatch is a `400`.

### BYOK LLM credential

The provider key Brain uses for extraction and HyPE on write. Per namespace,
and write-only: nothing here returns the key itself.

| Method   | Path                          | Body / query                        | Returns |
| -------- | ----------------------------- | ----------------------------------- | ------- |
| `PUT`    | `/v1/llm-credential`          | `{ provider, api_key, model? }`     | `{ provider, model, fingerprint, registered_at_unix_nanos }` — a display fingerprint, never the key |
| `GET`    | `/v1/llm-credential`          | —                                   | the same shape, or `404` when the namespace has none |
| `DELETE` | `/v1/llm-credential`          | —                                   | `{ provider, deleted }` |
| `POST`   | `/v1/llm-credential/test`     | `{ provider, api_key, model? }`     | `{ ok, provider, model, detail? }` — checks a candidate key **without storing it** |
| `POST`   | `/v1/llm-credential/verify`   | —                                   | the same shape, for the key already stored |

⚠️ `PUT` and `/test` carry a **raw provider key in the request body**. Deleting
the credential stops every write in the namespace, because extraction and HyPE
are mandatory rather than optional enrichment.

### LLM broker (proxy plane)

A pass-through to the namespace's own provider, so an agent can reach a model
with the key Brain already holds instead of being given one. Not part of the
`/v1` data plane — these keep the provider's own request and response shapes.

| Method | Path                                             | Body / query            | Returns |
| ------ | ------------------------------------------------ | ----------------------- | ------- |
| `GET`  | `/llm/ns/{namespace}/resolve`                    | —                       | `{ provider, model, source }` — which provider a namespace resolves to |
| `POST` | `/llm/ns/{namespace}/openai/v1/chat/completions` | the provider's own body | the provider's own response, streamed through unchanged |
| `POST` | `/llm/ns/{namespace}/anthropic/v1/messages`      | the provider's own body | the provider's own response, streamed through unchanged |

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
| `BRAIN_EDGE_PLANE`              | `both`           | Which plane this process serves — `both`, `data` (memory verbs only; **no key material is reachable in this process**), or `credential` (BYOK management + broker only). An unrecognised value is a hard error, not a silent fallback to `both` |
| `BRAIN_EDGE_LLM_CREDENTIAL_FILE` | unset           | Path to a durable BYOK credential file. **Unset = the store is in-memory**, so registered keys do not survive a restart |
| `BRAIN_EDGE_LLM_KEKS`           | unset            | Key-encryption keys as `id:hex,id:hex,…`. Unset = BYOK is not configured. Never held on a config struct — read once into the key ring so key material cannot reach a `Debug` print or a startup log |
| `BRAIN_EDGE_LLM_KEK_CURRENT`    | unset            | Which KEK id new seals use. Optional with exactly one key, where the id is unambiguous; **required with several** — guessing which key to seal under is not a safe default |
| `BRAIN_EDGE_LLM_PROXY_TOKEN`    | unset            | Bearer the LLM broker plane requires on `/llm/ns/...`, i.e. Brain's own identity to this edge |
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
same process, and the same limits, as the HTTP callers.

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

**Build it from this directory.** `brain-db-sdk` resolves from crates.io, so the
context is just this crate:

```bash
docker build -t brain-edge:latest .
```

This used to require the arc-labs parent as the context, because `brain-db-sdk`
was a path dependency on a sibling checkout that a context scoped here could not
see. That is no longer the case.

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
