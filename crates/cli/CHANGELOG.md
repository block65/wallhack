# Changelog

## [0.2.2](https://github.com/block65/wallhack/compare/cli-v0.2.1...cli-v0.2.2) (2026-02-20)


### Bug Fixes

* **cli:** clarify exit node startup messages ([fe833f3](https://github.com/block65/wallhack/commit/fe833f3d6445dc7bd41e6bec91ad395644de2df9))
* **cli:** eliminate duplicate error/info output in interactive mode ([34b3217](https://github.com/block65/wallhack/commit/34b32172103e69031ed65fd560519ecdf276f102))

## [0.2.1](https://github.com/block65/wallhack/compare/cli-v0.2.0...cli-v0.2.1) (2026-02-20)


### Features

* **cli:** default to port 6565 when no port given for --connect and --listen ([3343d10](https://github.com/block65/wallhack/commit/3343d1094b9eaadb7c815496333d4ee134df5b2b))

## [0.2.0](https://github.com/block65/wallhack/compare/cli-v0.1.2...cli-v0.2.0) (2026-02-19)


### Features

* **cli:** auto-generate REST API secret and require auth by default ([9402dba](https://github.com/block65/wallhack/commit/9402dba3db9b65a0209fc65bb981b66e15602f16))

## [0.1.2](https://github.com/block65/wallhack/compare/cli-v0.1.1...cli-v0.1.2) (2026-02-18)


### Bug Fixes

* **ci:** restore slim variant with proper feature isolation and fix r… ([e8f0f3b](https://github.com/block65/wallhack/commit/e8f0f3b43d2fe9e7fab24d3ea2d3a9c3c3f8c5a2))
* **ci:** restore slim variant with proper feature isolation and fix release upload race ([7a04c81](https://github.com/block65/wallhack/commit/7a04c81787aca98c03a62b3211c2e61ddf1f4eee))

## [0.1.1](https://github.com/block65/wallhack/compare/cli-v0.1.0...cli-v0.1.1) (2026-02-18)


### Bug Fixes

* gate build_server_config behind quic/websocket features for slim build ([2edf4b5](https://github.com/block65/wallhack/commit/2edf4b58732e466dbc74127d728694b3e8e9a861))

## 0.1.0 (2026-02-18)


### Bug Fixes

* **release:** track whole repo so all fix/feat commits trigger releases ([219e5e1](https://github.com/block65/wallhack/commit/219e5e1229b71fdb0f06f146a4abac4657aa06f9))
* **release:** use cargo-workspace plugin with all crates listed ([329d2a4](https://github.com/block65/wallhack/commit/329d2a4f881d3064ff4cad25acd0ac6e8ae2dd2c))
