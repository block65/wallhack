
# Changelog

## [0.8.2](https://github.com/block65/wallhack/compare/wallhack-cli-v0.8.1...wallhack-cli-v0.8.2) (2026-03-16)

### Bug Fixes

* **build:** strip unused vergen cargo/rustc features to reduce deps ([db7f1c8](https://github.com/block65/wallhack/commit/db7f1c883c9fef108e1c3c187edbdc39951057ba))
* **build:** update bloat thresholds to reflect post-relay baseline ([c5c2be9](https://github.com/block65/wallhack/commit/c5c2be90b66ebe0c8997f708ea0954ec8cc31e5a))
* **build:** feature-gate serde_json out of slim build ([170744f](https://github.com/block65/wallhack/commit/170744f4a4de0288c71498a2cf6da7c73be86979))
* **build:** disable tracing default features, cap at debug level ([a0ad162](https://github.com/block65/wallhack/commit/a0ad162dd172987ae9c9c087be3ff73801b605a7))

2 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.8.1...wallhack-cli-v0.8.2)


## [0.8.1](https://github.com/block65/wallhack/compare/wallhack-cli-v0.8.0...wallhack-cli-v0.8.1) (2026-03-16)

### Bug Fixes

* **relay:** correct data plane wiring and fix multiple peer management bugs ([b2f157f](https://github.com/block65/wallhack/commit/b2f157f6869afd8af5899f357478a477cfa2068f))
* **relay:** remove unused bridge_channels to fix slim dead-code error ([7a3b309](https://github.com/block65/wallhack/commit/7a3b30919a1fc421e51c8a556cb12774ab4212c9))
* **version:** restore semver+build-metadata format with seconds (#77) ([65111e5](https://github.com/block65/wallhack/commit/65111e559234401f23eabd0789bec0300e8d3c80))

1 other change — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.8.0...wallhack-cli-v0.8.1)


## [0.8.0](https://github.com/block65/wallhack/compare/wallhack-cli-v0.7.0...wallhack-cli-v0.8.0) (2026-03-16)

### Features

* **cli:** \--role alias for --fixed-role, git SHA in version, consolidate reconnect logs ([dd04dd8](https://github.com/block65/wallhack/commit/dd04dd88fb320d8732ee1a520fad42b790462509))
* **peers:** auto-ping on connect, fix display state, relay role, TUN cleanup, --json output ([5b68528](https://github.com/block65/wallhack/commit/5b685287618d3daceca40dced2847f2758eeaa9d))

### Bug Fixes

* **cli:** remove --fixed-role alias, rename FixedRole* error variants (#70) ([ac97d4a](https://github.com/block65/wallhack/commit/ac97d4ad9bbcc8104cbcc1554c2703f03ea2fe6f))
* **peers:** serde_json for --json output, drop side field, remote_addr display label ([69bf610](https://github.com/block65/wallhack/commit/69bf610ba807694fcdacd57dd6b009cf63c9c62b))
* **relay:** retain control_tx across relay session lifetime, register bridged peers ([d328144](https://github.com/block65/wallhack/commit/d328144045ea815cc83017d69a0791a4935da30b))

3 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.7.0...wallhack-cli-v0.8.0)


## [0.7.0](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.3...wallhack-cli-v0.7.0) (2026-03-15)

### Features

* **entry:** forward ICMP echo requests through tunnel ([9599dc2](https://github.com/block65/wallhack/commit/9599dc2674eb663eada0895e37ebed6d7b67908a))

### Bug Fixes

* **cli:** deduplicate consecutive identical log lines with repeat summary ([c95ff38](https://github.com/block65/wallhack/commit/c95ff3840fe4f9346066f0b88701745a956a2959))
* **cli:** wire vsock IPC listener into daemon REPL, refactor ctl stream connect ([f2ef974](https://github.com/block65/wallhack/commit/f2ef974487b2c477240d7d319c06b7726e3df29a))
* **core:** suppress unused variable warning in find_by_addr ([48aec0a](https://github.com/block65/wallhack/commit/48aec0aa321fb28cd0f1d5506d05895d8053913c))
* **core:** client handshake must carry routes and hint from local_handshake ([ca7de2c](https://github.com/block65/wallhack/commit/ca7de2c7e434c1a80e0b8821cead82ca71694949))
* **core:** wire route_updates broadcast channel into server handlers ([a3d3c72](https://github.com/block65/wallhack/commit/a3d3c72e01e971c4d281e7f9a4bedad7a3a62982))
* **daemon:** exponential backoff on reconnect and log dedup ([d89e532](https://github.com/block65/wallhack/commit/d89e532617038a0acbcc2eb91fbccfd607c8c4b6))
* **daemon:** peer names, role in peers, status capabilities, log quality ([1ed1c82](https://github.com/block65/wallhack/commit/1ed1c82d219cc41c1e04ffc020f33a4a30039a20))
* **daemon:** consistent log format, reconnect attempt counter, backoff naming ([b65080d](https://github.com/block65/wallhack/commit/b65080df2d1bb4dbc92045f60a44d54f79ec68d2))
* **daemon:** TCP relay logging, PSK dedup, version display with build ID ([5c78ab1](https://github.com/block65/wallhack/commit/5c78ab1a64cd33697230fbc2a11c9a5997d55d0f))
* **daemon:** consolidate version display into single canonical format ([5be96b4](https://github.com/block65/wallhack/commit/5be96b4b0e0665f84f70831eaedf89a6cd870fc6))
* **daemon:** single version source — global.version used everywhere ([991c74b](https://github.com/block65/wallhack/commit/991c74b3cea92e3caf47edad662282b1fde42ffb))
* **daemon:** initial ping + 30s heartbeat in entry connection loop ([51d9e46](https://github.com/block65/wallhack/commit/51d9e4621fab254cd77d760d1809ee880426d7a6))
* **daemon:** include build timestamp in version string ([1734d77](https://github.com/block65/wallhack/commit/1734d771dc5e65da6ef4af07208dd5f8f034a57d))
* **daemon:** 13e route announcement — exit advertises local CIDRs, entry auto-installs ([d363d90](https://github.com/block65/wallhack/commit/d363d900d2026107ecc70d69c69a65d190e8b6c2))
* **deps:** update website/package.json version ranges to match pnpm-lock.yaml ([cef8469](https://github.com/block65/wallhack/commit/cef84696259338554ae574d33e807f885664f87a))
* **entry-stack:** prune stale JIT listen sockets, ProbeResult enum for SYN probe ([c80e363](https://github.com/block65/wallhack/commit/c80e363ce1094cc66bad6fb5ec2699fa0cf12d30))
* **exit:** bind TCP to unspecified instead of hardcoded IP ([eca1c5d](https://github.com/block65/wallhack/commit/eca1c5d360fdf3326f1cc9937f13d16c12cb3d12))
* **ipc:** vsock IPC client support — IpcStream enum, feature-gated vsock variant ([b4bfff7](https://github.com/block65/wallhack/commit/b4bfff7cfb8c2348e25cda123dd8f9aeda7d6af4))
* **logging:** demote per-flow probe log to debug, fix Some() leak in route listener ([66c496c](https://github.com/block65/wallhack/commit/66c496c2eb90c582d56e43584bf16a20bb2b1835))
* **mcp:** wire tool router, fix range setup for MCP control ([f53e5e9](https://github.com/block65/wallhack/commit/f53e5e99a15f46ebfcc1fc199e80d849f1407214))
* **mcp:** consistent lowercase in status/peer display, clearer connect logs ([c90e35a](https://github.com/block65/wallhack/commit/c90e35ae587109788edfd5160ad52a4e92d7bad7))
* **mcp:** rename tools to drop wallhack_ prefix, add version to ServerInfo, unify built dep ([947706b](https://github.com/block65/wallhack/commit/947706b724eb4aa40b47b34bad74697874233a7a))
* **mcp:** disconnect_peer accepts name prefix or remote address ([018d813](https://github.com/block65/wallhack/commit/018d81331abf8e49943919b64078df926417733e))
* **mcp:** show auto-managed routes with (auto) tag in routes output ([8694a99](https://github.com/block65/wallhack/commit/8694a99c057195f281f0892debea0740c8ebb91c))

13 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.3...wallhack-cli-v0.7.0)


## [0.6.3](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.2...wallhack-cli-v0.6.3) (2026-03-15)

### Bug Fixes

* implement role transitions and hint-reactive auto mode (13g) ([0742241](https://github.com/block65/wallhack/commit/0742241e4ea5c88e9f0cd344c145e7940c6f2037))
* **core:** erase transport generics before async spawn to reduce binary size ([6fadf85](https://github.com/block65/wallhack/commit/6fadf853f5f09f671bf153d434ab9dcb97019bec))
* **core:** add #[must_use] to erase() methods ([4d6079d](https://github.com/block65/wallhack/commit/4d6079d7f98b5260a2690ff699d1b22cc6e753b3))
* **core:** remove unnecessary mut bindings in tests ([8019c27](https://github.com/block65/wallhack/commit/8019c27381bd0d3a1c88b8736b6ee5b1d41ff24d))
* **daemon:** fix clippy lints from type-erasure refactor ([7fa7a1e](https://github.com/block65/wallhack/commit/7fa7a1e5c55dd2803c07d52b4401ebe32fe6f59a))

1 other change — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.2...wallhack-cli-v0.6.3)


## [0.6.2](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.1...wallhack-cli-v0.6.2) (2026-03-07)

### Bug Fixes

* **entry-stack:** flush smoltcp immediately in poll_write to reduce tcp_downstream latency ([1634163](https://github.com/block65/wallhack/commit/1634163dd2efeaafbbd14421a6b08668851046d1))
* **entry-stack:** drain smoltcp egress in burst to double tcp_downstream throughput ([b0892db](https://github.com/block65/wallhack/commit/b0892dbaff84089da7eedd9a0a0d42ed23667af5))
* **entry-stack:** scale drain_egress rounds with socket count to fix parallel regression ([f6bd02e](https://github.com/block65/wallhack/commit/f6bd02e1dde90edc5a26fc60f198a8a0d886a469))


## [0.6.1](https://github.com/block65/wallhack/compare/wallhack-cli-v0.6.0...wallhack-cli-v0.6.1) (2026-03-07)

### Bug Fixes

* replace broadcast channels with mpsc on the data path ([65aeb57](https://github.com/block65/wallhack/commit/65aeb579039cd6ff822fb4fd965c19c97b47a005))
* increase recv buffer to 64 KiB in exit adapter ([bcbb01c](https://github.com/block65/wallhack/commit/bcbb01c813ee6c65aa5deb0986e54df1076aff10))


## [0.6.0](https://github.com/block65/wallhack/compare/wallhack-cli-v0.5.0...wallhack-cli-v0.6.0) (2026-03-07)

### Features

* **cli:** add color to notification prefixes in REPL ([1ab1def](https://github.com/block65/wallhack/commit/1ab1def832d06f94ee8311fb0325b3f489de851d))

2 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.5.0...wallhack-cli-v0.6.0)


## [0.5.0](https://github.com/block65/wallhack/compare/wallhack-cli-v0.4.0...wallhack-cli-v0.5.0) (2026-03-07)

### Features

* **negotiate:** support role hints for auto-negotiation ([a8a03d4](https://github.com/block65/wallhack/commit/a8a03d431ba66c2f08a88a4eba5ef63cc78d3244))

### Bug Fixes

* **core:** remove unused ip_as_octets feature flag from wallhack-core ([da319d2](https://github.com/block65/wallhack/commit/da319d2198fbd25ce8bb066a311c6a48475bf83e))
* **transport:** remove unused trait_alias feature flag ([76e2d7d](https://github.com/block65/wallhack/commit/76e2d7d96ef9dd3afa09df4301b0e5e5a4046ed1))

7 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.4.0...wallhack-cli-v0.5.0)


## [0.4.0](https://github.com/block65/wallhack/compare/wallhack-cli-v0.3.1...wallhack-cli-v0.4.0) (2026-03-01)

### Features

* **cli:** top-level --connect/--listen flags for zero-config mode ([2d9f9c8](https://github.com/block65/wallhack/commit/2d9f9c8498b56eb48d97208388a4d4ef1e2a235c))
* **core:** plumb local_handshake through client config and wire clients ([68eef7b](https://github.com/block65/wallhack/commit/68eef7bee5c753d4bf78e9665508f24eb52540f7))
* **core:** negotiate() pure function for auto-role derivation ([afecba2](https://github.com/block65/wallhack/commit/afecba274ce03f07f0d40e484c23b2471d29ed1e))
* **daemon:** TUN capability detection via kernel probe ([765e680](https://github.com/block65/wallhack/commit/765e68004fca1a57eb5a10f1db069a2c90f628b9))
* **daemon:** advertise accurate capabilities in all mode handshakes ([ec2115c](https://github.com/block65/wallhack/commit/ec2115c9bd9de96dacc00c2501eb4492e2f7b39c))
* **daemon:** auto-negotiation mode ([d31540c](https://github.com/block65/wallhack/commit/d31540c99ad5ba75a0b3cafe74cf202eca13ef90))

### Bug Fixes

* **release:** make open-pr idempotent and fix stale push on chore commits ([90f2077](https://github.com/block65/wallhack/commit/90f2077390e887f61edb719a3b508dbb44cacbbe))

_4 other changes — [view diff](https://github.com/block65/wallhack/compare/wallhack-cli-v0.3.1...wallhack-cli-v0.4.0)_


## [0.3.1](https://github.com/block65/wallhack/compare/wallhack-cli-v0.3.0...wallhack-cli-v0.3.1) (2026-02-28)

### Bug Fixes

* **ci:** match release-please body format, fix cross install, add changelog links ([9732585](https://github.com/block65/wallhack/commit/9732585ef304210cafb9962ffd4504cae1df1192))
* **ci:** exclude ci-scoped commits from release bump logic ([a39de7a](https://github.com/block65/wallhack/commit/a39de7a692ebc5de724c6c95ebc57c96e896779c))
* **wire:** reject malformed protobuf at deserialisation boundary ([2591896](https://github.com/block65/wallhack/commit/2591896ff7e955c1052c243aa219dd6165683fad))


## [0.3.0](https://github.com/block65/wallhack/releases/tag/wallhack-cli-v0.3.0) - 2026-02-28

### Added

- add relay reconnect on source peer disconnect
- add Indeterminate as fourth NodeRole variant

### Fixed

- correct markdown link syntax in README
