# Changelog

## [0.4.0](https://github.com/arc-labs-ai/brain-edge/compare/v0.3.0...v0.4.0) (2026-10-29)


### ⚠ BREAKING CHANGES

* close the self-host parity gaps, and follow the wire's renames

### Fixes

* let a caller read the 413, and 404 an inspect of a memory that is not there ([8d351ba](https://github.com/arc-labs-ai/brain-edge/commit/8d351ba509ef5d13ead2249218148ae55dd0b874))
* **manifest:** describe the broker routes instead of failing on them ([f60e3c6](https://github.com/arc-labs-ai/brain-edge/commit/f60e3c60d6e23bbbf1e770978d057516fc3f3a3b))
* scope the change feed to the caller's own space ([5e38957](https://github.com/arc-labs-ai/brain-edge/commit/5e3895731bc1181a26158083c04a905ddae55e2e))
* send the caller's namespace on SCHEMA_LIST and SCHEMA_DROP ([9f100ba](https://github.com/arc-labs-ai/brain-edge/commit/9f100ba3448dd9188a11f65042e3f3dd60a9ef3e))
* **subscribe:** relay the HyPE stage's failure reason ([be27be4](https://github.com/arc-labs-ai/brain-edge/commit/be27be48e4a256140d7c386c34f6d46651bf9db9))


### Features

* close the self-host parity gaps, and follow the wire's renames ([80bebe2](https://github.com/arc-labs-ai/brain-edge/commit/80bebe21564f356f91041144d70121ac2273cad9))
* close the self-host/cloud parity gaps ([098b37a](https://github.com/arc-labs-ai/brain-edge/commit/098b37a39c94e19a0fa2fc3d19acb8f3aa494bdd))
* **error:** add forbidden and unavailable constructors ([8386dda](https://github.com/arc-labs-ai/brain-edge/commit/8386dda31963a954a900e5fbd827480210840b72))
* **port:** give the metering sink latency, identity and recall signals ([ee357ec](https://github.com/arc-labs-ai/brain-edge/commit/ee357ecdf0dea98ddb466923cbccd902bdea7e7f))
* **port:** hand the metering sink RECALL's trace, serialized once ([f2637a7](https://github.com/arc-labs-ai/brain-edge/commit/f2637a7a9a7139136f3af769f3a15c054ced0bac))
* **port:** let the metering sink refuse an op before it runs ([43bffb9](https://github.com/arc-labs-ai/brain-edge/commit/43bffb9eadf306ddaf223ca83bb4da00c4674d82))
* **port:** resolve a tenant identity and stamp it on every request ([50bc02a](https://github.com/arc-labs-ai/brain-edge/commit/50bc02a288b33d2069892056f05cce535a1d078d))
* serve query-plan introspection ([8a6aaa0](https://github.com/arc-labs-ai/brain-edge/commit/8a6aaa0b5fab19c51eb2e80deb347b1f6d055cb0))
* serve schema history, extractor list and the memory extras ([2f1daf3](https://github.com/arc-labs-ai/brain-edge/commit/2f1daf369bef0de5978ec305b8e9b7a2e147f301))
* serve the change feed as SSE, and teach the manifest to describe it ([6917581](https://github.com/arc-labs-ai/brain-edge/commit/6917581f2c535fee26e1be3e380fbaf2f8ab3f00))
* serve the entity authoring verbs ([b469641](https://github.com/arc-labs-ai/brain-edge/commit/b469641e1b1be5e8cfd4815522c6bf84fb546af7))
* serve the relation authoring verbs, and expose the edge chain ([f6e35a8](https://github.com/arc-labs-ai/brain-edge/commit/f6e35a831fc18755a181c91137f9c94e861295b4))
* serve the space and session lifecycle verbs ([96324d5](https://github.com/arc-labs-ai/brain-edge/commit/96324d5fdfbbc71488f83392c7f8fd318fd4d768))
* serve the statement authoring verbs, and fix two misaligned reads ([5c12da6](https://github.com/arc-labs-ai/brain-edge/commit/5c12da67fe4f0db162d46759fb62c829e0c09e27))


### Refactoring

* **dto:** one recall constructor ([0c4c392](https://github.com/arc-labs-ai/brain-edge/commit/0c4c3920e747230b72fbb5fab50bd2648c80d222))
* let the category mapping produce inspect's 404 ([a03d1c7](https://github.com/arc-labs-ai/brain-edge/commit/a03d1c7c922876a95d7019461eda53bcc42b227c))


### Tests and verification

* cover the request/response mapping for every route added ([8fb0775](https://github.com/arc-labs-ai/brain-edge/commit/8fb07754387dffcfa82b1d25a03f59e63151179b))
* **port:** pin what the self-host defaults promise ([c08faf1](https://github.com/arc-labs-ai/brain-edge/commit/c08faf1908472f59d43ca9fd8375d9fcfb3a58ff))


### Build and tooling

* **deps:** require brain-db-sdk 0.4.0 and drop the local patch ([5a004bd](https://github.com/arc-labs-ai/brain-edge/commit/5a004bde9da4a816296421d6552580c0c58ba5b2))

## [0.3.0](https://github.com/arc-labs-ai/brain-edge/compare/v0.2.1...v0.3.0) (2026-10-21)


### Features

* **memory:** serve memory edges on inspect, off the published SDK 0.3.0 ([9cfe596](https://github.com/arc-labs-ai/brain-edge/commit/9cfe596118b4f2d34183d9cc55faec8629e1f497))

## [0.2.1](https://github.com/arc-labs-ai/brain-edge/compare/v0.2.0...v0.2.1) (2026-10-21)


### Build and tooling

* **deps:** clear six security advisories and take brain-db-sdk 0.2.1 ([94e3a10](https://github.com/arc-labs-ai/brain-edge/commit/94e3a10f2bd3d4f624514b6f4002d0533390b608))
* **deps:** clear six security advisories and take brain-db-sdk 0.2.1 ([c39eea4](https://github.com/arc-labs-ai/brain-edge/commit/c39eea49748557534a90cdcaa71096ff6e3a5608))

## [0.2.0](https://github.com/arc-labs-ai/brain-edge/compare/v0.1.0...v0.2.0) (2026-10-20)


### Fixes

* **ci:** grant attestations:write so provenance can be written ([0e09c62](https://github.com/arc-labs-ai/brain-edge/commit/0e09c62132e35ce7947f9ed34802fd023bcded53))


### Features

* **byok:** seal tenant provider keys in the edge and broker them upstream ([96ad50d](https://github.com/arc-labs-ai/brain-edge/commit/96ad50d8c4a851f74b806c00eea3a5881b21759d))


### Tests and verification

* **ports:** bring the BYOK routes inside the port guarantees ([e456dcb](https://github.com/arc-labs-ai/brain-edge/commit/e456dcbe86cf7feff229672377062d3b19cdb300))


### Build and tooling

* add a workflow_dispatch hatch to the release workflow ([e74d995](https://github.com/arc-labs-ai/brain-edge/commit/e74d9951c2223047862ea5ca466812b414555d58))
* build each architecture on a native runner instead of QEMU ([2fba206](https://github.com/arc-labs-ai/brain-edge/commit/2fba2061727fb2d44890937156a9632431b36039))
* gate dev PRs and keep one CI run per commit ([d267c55](https://github.com/arc-labs-ai/brain-edge/commit/d267c55d5e9d7bd62d7da031b8828aa24e665843))
* pin runners to ubuntu-26.04 instead of the floating label ([1d8a383](https://github.com/arc-labs-ai/brain-edge/commit/1d8a383f3cf79c7f273dddbafe019a89f620f81e))

## 0.1.0 (2026-10-07)


### Fixes

* bound the rate limiter's memory, and deepen brain-edge's lints ([f7a67ed](https://github.com/arc-labs-ai/brain-edge/commit/f7a67edf6f5da41c66a6a3fb57b98458614f5cc3))
* clear the four pre-existing clippy -D warnings errors ([7391b11](https://github.com/arc-labs-ai/brain-edge/commit/7391b11a13cbea4bad35d6852f0dd2c7533880b6))
* **edge:** follow SDK wire additions and reconnect dead pool members ([5449cc1](https://github.com/arc-labs-ai/brain-edge/commit/5449cc11d51dcfcd969f8d93f205b92b9fd20b04))
* **edge:** satisfy clippy 1.99 (assert_is_empty, double_must_use) ([61dcda2](https://github.com/arc-labs-ai/brain-edge/commit/61dcda2cbb7a4a1c2e3c497e535621f9fdb74243))
* **tools:** the manifest was silently dropping every enum DTO ([0248d7c](https://github.com/arc-labs-ai/brain-edge/commit/0248d7c70c1fddba4781eefb85fc3bb1258cd3a4))


### Features

* **edge:** expose namespace-wide recall scope on POST /v1/recall ([6a07d21](https://github.com/arc-labs-ai/brain-edge/commit/6a07d217b1ece8677005541fbb68fca558099478))
* **edge:** Option X transparent wire-passthrough proxy ([8cf51d9](https://github.com/arc-labs-ai/brain-edge/commit/8cf51d92a0929a7eecb9c80540775f04392ce702))
* **entity:** surface the merge-redirect chain on GET /v1/entities/{id} ([97e00ce](https://github.com/arc-labs-ai/brain-edge/commit/97e00ce9b46d6ce7235f1271221b82279a89f0c8))
* expose the schema surface over HTTP, including the destructive replace ([c0fb7b9](https://github.com/arc-labs-ai/brain-edge/commit/c0fb7b9da20037e6f5a89725f5ad3bdaf2c1a61b))
* fixed edge http layer contract ([4679586](https://github.com/arc-labs-ai/brain-edge/commit/467958661c0251e3096eb5da931e233077fea24e))
* production hardening — panic recovery, correlation ids, no 5xx leakage ([a639717](https://github.com/arc-labs-ai/brain-edge/commit/a639717298463938b19a1f62f670da2ccc097428))
* **tenancy:** rename HTTP tier to space/session, track SDK contract ([61bddce](https://github.com/arc-labs-ai/brain-edge/commit/61bddce33295be1c3612fd4dc4d7e7afb44e664b))


### Tests and verification

* **edge:** complete handler happy-path coverage for all 25 /v1 routes ([e0047a4](https://github.com/arc-labs-ai/brain-edge/commit/e0047a41a6b2f24b1c1bdf00127301bd55fb9fe4))
* **edge:** dto-conversion unit tests + handler happy-path harness (mock Brain) incl. relation to/from branch ([8c178f3](https://github.com/arc-labs-ai/brain-edge/commit/8c178f317bc961cf5bfacf9adce61d37a7ed2728))
* guard the two ports that turn this edge into a product ([f6468c2](https://github.com/arc-labs-ai/brain-edge/commit/f6468c278cb0e0dde411754577cb853e4bc0b379))


### Build and tooling

* **edge:** add the Apache-2.0 LICENSE file ([3682bc2](https://github.com/arc-labs-ai/brain-edge/commit/3682bc2eb50e6ccc0ce6b62e3609b1ad3083a194))
* **edge:** depend on the published brain-db-sdk, not a sibling checkout ([74f458c](https://github.com/arc-labs-ai/brain-edge/commit/74f458c65270681db87a274a2b357c06e8f2e36b))
* **edge:** don't publish a floating `0` tag for a 0.x release ([dfb37bd](https://github.com/arc-labs-ai/brain-edge/commit/dfb37bd0e94f81cb1278b6ff5276dea764a351e0))
* **edge:** drop the sibling checkouts and move off the Node 20 runtime ([c7f67d6](https://github.com/arc-labs-ai/brain-edge/commit/c7f67d6c5ba593c47cce6fd3c02d50c0544da8e6))
* **edge:** make the crate publishable to crates.io ([03351ef](https://github.com/arc-labs-ai/brain-edge/commit/03351ef697bd4648a68847a376bf12b1f317f380))


### Documentation

* **edge:** ListEntitiesQuery.type_id is required, not optional ([6c669e4](https://github.com/arc-labs-ai/brain-edge/commit/6c669e449aa1a1ddd77cbf302392c214d84c9820))
* whoami returns space_id, not agent_id ([c2fbe8c](https://github.com/arc-labs-ai/brain-edge/commit/c2fbe8cce8b532452d217c487f2c0260d1bc23f6))


### Chores

* **release:** tag under a PAT and cut 0.1.0, not 0.2.0 ([212b425](https://github.com/arc-labs-ai/brain-edge/commit/212b42547e005d8596efdd0f6d821a6f5a7e6e2a))
