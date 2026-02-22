# Changelog

## [0.2.5](https://github.com/block65/wallhack/compare/cli-v0.2.4...cli-v0.2.5) (2026-02-22)


### Features

* **cli:** warn when kernel entropy pool is not yet seeded ([cf0c6ed](https://github.com/block65/wallhack/commit/cf0c6edbb04cfd5391e7130d969fbbf9c84092e7))


### Bug Fixes

* **client:** detect IPv6 support at runtime for default bind address ([551adab](https://github.com/block65/wallhack/commit/551adab70764d96c15be55232f46ead412567006))
* **entry:** display actual bound address instead of parsed input ([c37eb22](https://github.com/block65/wallhack/commit/c37eb22e19ccf3d8a9be269bfb041f548c5329ad))
* single source of truth for SOCAT_VERSION + clippy fixes ([c940fb5](https://github.com/block65/wallhack/commit/c940fb56d37702ea17d3c4218d81109cf56b26c5))
* **transport:** subscribe before spawn to eliminate UDP broadcast race ([1272a15](https://github.com/block65/wallhack/commit/1272a1561f0d3f497c628b367e8f9428593ce09c))


### Performance Improvements

* **bench:** flip VM startup order + reduce retry delays ([ebf74b7](https://github.com/block65/wallhack/commit/ebf74b7c4438f013850ac0afded2a4c6a6a1dbc3))


### Reverts

* restore [::] default for entry listen address ([738f2e0](https://github.com/block65/wallhack/commit/738f2e006c78e8edd9741517ab98540a8d8470a4))

## [0.2.4](https://github.com/block65/wallhack/compare/cli-v0.2.3...cli-v0.2.4) (2026-02-20)

## [0.2.3](https://github.com/block65/wallhack/compare/cli-v0.2.2...cli-v0.2.3) (2026-02-20)


### Features

* **cli:** omit peer in route add when exactly one peer is connected ([aa1aced](https://github.com/block65/wallhack/commit/aa1acede97446093659a48c5fc4a0bdcc296adcc))

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
