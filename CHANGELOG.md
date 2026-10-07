# Changelog

## [0.1.1](https://github.com/arc-labs-ai/brain-edge/compare/v0.1.0...v0.1.1) (2026-10-07)


### Fixes

* **ci:** grant attestations:write so provenance can be written ([0e09c62](https://github.com/arc-labs-ai/brain-edge/commit/0e09c62132e35ce7947f9ed34802fd023bcded53))


### Build and tooling

* add a workflow_dispatch hatch to the release workflow ([e74d995](https://github.com/arc-labs-ai/brain-edge/commit/e74d9951c2223047862ea5ca466812b414555d58))
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
