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

| Method   | Path              | Body                                       | Returns |
| -------- | ----------------- | ------------------------------------------ | ------- |
| `POST`   | `/v1/memories`    | `{ text, context?, occurred_at? }`         | `{ memory_id, was_deduplicated, salience, kind, created_at_unix_nanos, auto_edges_added }` |
| `POST`   | `/v1/recall`      | `{ query \| cue, max_results?, subject? }` | `{ answer_kind, memories: [{ memory_id, text, similarity_score, confidence, salience, kind, created_at_unix_nanos }] }` |
| `DELETE` | `/v1/memories`    | `{ memory_id, hard? }`                     | `{ memory_id, was_already_forgotten, edges_removed }` |
| `POST`   | `/v1/links`       | `{ source, target, kind, weight? }`        | `{ source, target, kind, weight, created_at_unix_nanos, already_existed }` |
| `DELETE` | `/v1/links`       | `{ source, target, kind }`                 | `{ source, target, kind, removed }` |
| `POST`   | `/v1/plan`        | `{ start:{text\|memory_id}, goal:{…}, max_steps?, strategy? }` | `{ steps: [{ step_index, memory_id, text, transition_kind, confidence, estimated_distance_to_goal }] }` |
| `POST`   | `/v1/reason`      | `{ observation:{text\|memory_id}, depth?, confidence_threshold?, max_inferences? }` | `{ inferences: [{ step_index, claim, supporting_memories, contradicting_memories, confidence, inference_kind }] }` |
| `GET`    | `/v1/whoami`      | —                                          | `{ namespace, agent_id, permissions }` |
| `GET`    | `/v1/capabilities`| —                                          | `{ rerank, llm_extractor, classifier_extractor, pattern_extractor, schema_namespaces, vector_dim }` |
| `GET`    | `/health/live`    | —                                          | `200` (process is up) |
| `GET`    | `/health/ready`   | —                                          | `200` when Brain is reachable, else `503` |

Edge kinds: `caused` · `followed_by` · `derived_from` · `similar_to` · `contradicts` · `supports` · `references` · `part_of`.

Memory ids are decimal strings (they exceed the JS safe-integer range).

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
| `RUST_LOG`                      | `brain_edge=info`| Log filter                                          |

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

This edge exposes the **core data-plane** verbs (`encode` / `recall` / `forget`
/ `whoami`). The full wire protocol (typed-graph ops, subscribe, transactions)
remains available via the native `brain-db-sdk` client connecting to
`brain` directly — the performance/advanced tier. `brain-edge` is the
portable, curl-friendly tier that most integrations use.
