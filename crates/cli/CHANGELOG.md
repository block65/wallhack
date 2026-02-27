# Changelog

## [0.2.7](https://github.com/block65/wallhack/compare/wallhack-cli-v0.2.6...wallhack-cli-v0.2.7) (2026-02-25)


### Features

* add peer event notifications over IPC ([8e7cceb](https://github.com/block65/wallhack/commit/8e7ccebf36d0d2342fc3e9f9d0350f1b21c10ed3))
* **cli:** add --name/-n flag to entry and relay; share generate_node_name() ([8c849a6](https://github.com/block65/wallhack/commit/8c849a6745fdc946bf9af5bb8c5d86baccfd1b6a))
* **cli:** auto-generate REST API secret and require auth by default ([9402dba](https://github.com/block65/wallhack/commit/9402dba3db9b65a0209fc65bb981b66e15602f16))
* **cli:** default to port 6565 when no port given for --connect and --listen ([3343d10](https://github.com/block65/wallhack/commit/3343d1094b9eaadb7c815496333d4ee134df5b2b))
* **cli:** omit peer in route add when exactly one peer is connected ([aa1aced](https://github.com/block65/wallhack/commit/aa1acede97446093659a48c5fc4a0bdcc296adcc))
* **cli:** warn when kernel entropy pool is not yet seeded ([cf0c6ed](https://github.com/block65/wallhack/commit/cf0c6edbb04cfd5391e7130d969fbbf9c84092e7))
* **entry:** startup header, REPL unification, DoneGuard output sync ([0e3f68a](https://github.com/block65/wallhack/commit/0e3f68a33116bfd3d94da1f06ea3838fda4036fe))
* **exit:** startup header, REPL unification, drop state:, DoneGuard sync ([531f58c](https://github.com/block65/wallhack/commit/531f58ce46e31a4cb6cf7c93ee94b49934235cf6))
* ping improvements + peer prefix matching + status→info rename ([dc2c540](https://github.com/block65/wallhack/commit/dc2c540a23a6732e377d1f79d99124f7b6bc6f55))
* **repl-common:** add PrintMsg/DoneGuard, uptime(), and unified print_help() ([960fe7f](https://github.com/block65/wallhack/commit/960fe7ff5bfdf8dd8e952f11ea9cca492f685680))


### Bug Fixes

* **ci:** restore slim variant with proper feature isolation and fix r… ([e8f0f3b](https://github.com/block65/wallhack/commit/e8f0f3b43d2fe9e7fab24d3ea2d3a9c3c3f8c5a2))
* **ci:** restore slim variant with proper feature isolation and fix release upload race ([7a04c81](https://github.com/block65/wallhack/commit/7a04c81787aca98c03a62b3211c2e61ddf1f4eee))
* **cli:** bundle entry node shared state into EntryResources struct ([d2b8e43](https://github.com/block65/wallhack/commit/d2b8e430007ff2561f5b099918464fe30c19b50b))
* **cli:** clarify exit node startup messages ([fe833f3](https://github.com/block65/wallhack/commit/fe833f3d6445dc7bd41e6bec91ad395644de2df9))
* **cli:** eliminate duplicate error/info output in interactive mode ([34b3217](https://github.com/block65/wallhack/commit/34b32172103e69031ed65fd560519ecdf276f102))
* **client:** detect IPv6 support at runtime for default bind address ([551adab](https://github.com/block65/wallhack/commit/551adab70764d96c15be55232f46ead412567006))
* **cli:** REPL in entry connect mode; peer name IP; debug log corruption ([2320f7d](https://github.com/block65/wallhack/commit/2320f7db8d71ed6f801a5dfd7ff42b59918a14f8))
* **cli:** use PKG_NAME in version output instead of hardcoded string ([61a9175](https://github.com/block65/wallhack/commit/61a9175bc1acf7e45bf124b7489b766ed42d8601))
* **entry:** display actual bound address instead of parsed input ([c37eb22](https://github.com/block65/wallhack/commit/c37eb22e19ccf3d8a9be269bfb041f548c5329ad))
* gate build_server_config behind quic/websocket features for slim build ([2edf4b5](https://github.com/block65/wallhack/commit/2edf4b58732e466dbc74127d728694b3e8e9a861))
* merge identical match arms in exit repl input loop ([7f603a1](https://github.com/block65/wallhack/commit/7f603a1d56ae8952c4c5f7340bd60d28a5ee2d41))
* **output:** skip ANSI colour codes when output is not a terminal ([e336149](https://github.com/block65/wallhack/commit/e3361499629950caa479edcaea6d99aaeda2b06b))
* **relay:** add startup header, tighten DNS logging, demote retry to warn ([63a89ac](https://github.com/block65/wallhack/commit/63a89ac8adf698e7e0d2d0f3674c5f8a2cb3429f))
* **release:** track whole repo so all fix/feat commits trigger releases ([219e5e1](https://github.com/block65/wallhack/commit/219e5e1229b71fdb0f06f146a4abac4657aa06f9))
* **release:** use cargo-workspace plugin with all crates listed ([329d2a4](https://github.com/block65/wallhack/commit/329d2a4f881d3064ff4cad25acd0ac6e8ae2dd2c))
* single source of truth for SOCAT_VERSION + clippy fixes ([c940fb5](https://github.com/block65/wallhack/commit/c940fb56d37702ea17d3c4218d81109cf56b26c5))
* **startup:** reduce noise, wire --verbose to version, initialise colour early ([e5c4f07](https://github.com/block65/wallhack/commit/e5c4f07f80c661f3f903cda7fdf7ce8c5db77543))
* **transport:** subscribe before spawn to eliminate UDP broadcast race ([1272a15](https://github.com/block65/wallhack/commit/1272a1561f0d3f497c628b367e8f9428593ce09c))
* **version:** split print_version into short and verbose ([2144278](https://github.com/block65/wallhack/commit/2144278d98d194ab40779e547783f2fdce34e84e))


### Performance Improvements

* **bench:** flip VM startup order + reduce retry delays ([ebf74b7](https://github.com/block65/wallhack/commit/ebf74b7c4438f013850ac0afded2a4c6a6a1dbc3))


### Reverts

* restore [::] default for entry listen address ([738f2e0](https://github.com/block65/wallhack/commit/738f2e006c78e8edd9741517ab98540a8d8470a4))
