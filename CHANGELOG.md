# Changelog

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

