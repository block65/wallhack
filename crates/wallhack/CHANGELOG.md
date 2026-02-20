# Changelog

## 0.1.0 (2026-02-20)


### Features

* add Cidr type and extend control protocol for routing ([7ea58a2](https://github.com/block65/wallhack/commit/7ea58a2c6cb665f951f7627a92be6ecea7a0dc02))
* add high connection rate warning for scan mode hint ([a978a3d](https://github.com/block65/wallhack/commit/a978a3d7c62ff535b2965202991f43bb9642a71c))
* add peer registry and route table for per-peer routing ([cf29aee](https://github.com/block65/wallhack/commit/cf29aee2bdd904bab69ea7000dab1f425b34b57f))
* add ping/pong messages to tunnel protocol ([f4320ee](https://github.com/block65/wallhack/commit/f4320ee81beca9f0f20c9c73726251c4ee0428cd))
* add ping/pong to tunnel protocol ([6f5ef6c](https://github.com/block65/wallhack/commit/6f5ef6c0ba6230c8968a3d05b9ff7b8733f8daee))
* add REPL support for exit nodes ([2f52bf3](https://github.com/block65/wallhack/commit/2f52bf3cbd0409d3897c9e6bf6c10965cb2dd194))
* add REST API for headless node control ([ae3c4de](https://github.com/block65/wallhack/commit/ae3c4de801930e9ecd15cccdb9d3bfa4c5335207))
* **bridge:** support persistent control and dedicated data streams for plane separation ([ec55455](https://github.com/block65/wallhack/commit/ec554553679b3abac00c7df151252718af9eddf3))
* enable PSK and peer limit configuration for tunnel auth ([33575e8](https://github.com/block65/wallhack/commit/33575e8804a6bf697865b535286fa251a88e0c2b))
* **entry:** propagate ICMP unreachable errors for UDP scanning ([7875eaf](https://github.com/block65/wallhack/commit/7875eaf743af0d425961de337f6905aa33c057f7))
* **entry:** SYN proxy for accurate TCP port scanning ([5741ad8](https://github.com/block65/wallhack/commit/5741ad83aadca40f000fea39598f20c6d551d6b1))
* **exit:** reap idle UDP/ICMP sessions to prevent unbounded memory growth ([75a90fc](https://github.com/block65/wallhack/commit/75a90fce64f0c37a7428ddbf1cef94741ca33246))
* HTTP CONNECT and SOCKS5 proxy support for WebSockets transport ([ce2f8cf](https://github.com/block65/wallhack/commit/ce2f8cf3ecab8c2e38ecd5e7f8482fd80faf4708))
* implement routing control handlers and wire into servers ([686e352](https://github.com/block65/wallhack/commit/686e352c56ebf34eb2b5678bc76d09d5fefe0f19))
* integrate REST API with NodeApi trait ([e9998b5](https://github.com/block65/wallhack/commit/e9998b554dea60531c2e8430cced66f1551308f4))
* **proto:** add ControlMessage definition and arc-swap dependency ([aefb441](https://github.com/block65/wallhack/commit/aefb44118c3571009f1d36172fba0129db3fe042))
* **server:** enforce mTLS when CA roots are configured ([30b6595](https://github.com/block65/wallhack/commit/30b6595e70cac91294ac451cfbe48929fafa7a11))
* **tls:** support TOFU certificate pinning via SHA-256 fingerprint ([a06a683](https://github.com/block65/wallhack/commit/a06a683c2d1c97187c7b22eb16bbf066b1975057))
* **transport:** rewire connection lifecycle to use persistent control stream and gated handshake ([b550168](https://github.com/block65/wallhack/commit/b5501685928b8a10c16b1165e21682b769570ef0))
* **transport:** update connection types to support control stream task and simplified handshake ([67537bf](https://github.com/block65/wallhack/commit/67537bf4598596cc3650e5aac591408a0623e8c3))
* wire PSK and fingerprint through server and client transports ([2ccc48e](https://github.com/block65/wallhack/commit/2ccc48e7e68ad49cb05f3a19c4f9f6a92317de19))
* **ws:** enable TLS by default with self-signed cert for zero-config security ([1e15455](https://github.com/block65/wallhack/commit/1e154558f3f51bef2027ee6913bc41d7ddab51c1))


### Bug Fixes

* add session status handshake to fix nmap showing all ports open ([6515f67](https://github.com/block65/wallhack/commit/6515f6741247d577e2e7594680f4a41d28378566))
* **api:** propagate route added_at timestamp to REST API ([0a2c5de](https://github.com/block65/wallhack/commit/0a2c5dea7c45fc4a6e11e13cbb4a239f6edbccd2))
* avoid panicking on unimplemented code paths ([d2d562d](https://github.com/block65/wallhack/commit/d2d562de5e48bf1b2169807ecb4d73ff17fc84d3))
* **ci:** restore slim variant with proper feature isolation and fix r… ([e8f0f3b](https://github.com/block65/wallhack/commit/e8f0f3b43d2fe9e7fab24d3ea2d3a9c3c3f8c5a2))
* **ci:** restore slim variant with proper feature isolation and fix release upload race ([7a04c81](https://github.com/block65/wallhack/commit/7a04c81787aca98c03a62b3211c2e61ddf1f4eee))
* **entry:** bring TUN device UP on creation ([9cb264e](https://github.com/block65/wallhack/commit/9cb264ef78d0e6db82ac99d8c53a468e93ed0c34))
* **exit:** loop UDP recv in orchestrator to match TCP behaviour ([2dd3442](https://github.com/block65/wallhack/commit/2dd3442e4038ea8906e10c2f8a4fc0a2958a7f48))
* gate ICMP code behind #[cfg(unix)] for Windows compatibility ([cae65e9](https://github.com/block65/wallhack/commit/cae65e943fde74a970764792bbb33f9d99c85b69))
* **netstack:** drop unmatched TCP segments to prevent false open ports ([d0aa453](https://github.com/block65/wallhack/commit/d0aa453e9f0a33f44871fdd4859715e3ffcbeb5c))
* parallel TCP streams and QUIC stream exhaustion ([347c0b3](https://github.com/block65/wallhack/commit/347c0b30681047443f164ad5fb68a41a30d7c20b))
* prevent timing side-channel in credential validation ([0090b0b](https://github.com/block65/wallhack/commit/0090b0bccd3d41f701bf3b141f04be801f431710))
* **repl:** use exit_id as peer ID and normalize IPv4-mapped addresses ([a349de6](https://github.com/block65/wallhack/commit/a349de6bac82024822a3032892f0c3e3e9295fb3))
* **server:** move test module to end of tls.rs ([eeebd7b](https://github.com/block65/wallhack/commit/eeebd7b21bb2292dc0b816dd51a3f1659ca1517c))
* **tls:** default fingerprint hash prefix to sha256 ([51b130b](https://github.com/block65/wallhack/commit/51b130bfde589801c924bad82160d86cac0f12da))
* **transport:** preserve control_tx lifetime to prevent control stream death ([39894ce](https://github.com/block65/wallhack/commit/39894cec44dfe694879cc3e0c7918b0c8b2cec6a))
* update client run_incoming_data calls for new signature ([5c0b48a](https://github.com/block65/wallhack/commit/5c0b48abc3647d708a1485bc4cbc1191ab703768))
* update WebSocket client and bench scripts for REPL changes ([923a3b6](https://github.com/block65/wallhack/commit/923a3b6ebab5740d7330f546397d49c554395955))
* upgrade bytes to 1.11.1 (security) ([40e8962](https://github.com/block65/wallhack/commit/40e8962cb524f0178a5a9a96bd159cbe9e3db695))


### Performance Improvements

* **control:** replace RwLock with arc-swap for wait-free reads ([53117e1](https://github.com/block65/wallhack/commit/53117e1ddc798e1c199a8210dacd37fd3b964e47))
* **netstack:** replace 1ms sleep poll with epoll-based fd readiness ([37ddd67](https://github.com/block65/wallhack/commit/37ddd675d718555c1d7873e40323ab309a0e1b8d))
* **netstack:** zero-copy peek_all_ingress and reduce poll interval to 1ms ([b33274a](https://github.com/block65/wallhack/commit/b33274a646da555157c3d3fee8638f4cc1f8691b))
* **orchestrator:** use bytes::Bytes for protobuf data fields ([6cdb10b](https://github.com/block65/wallhack/commit/6cdb10b5588449741c75cac608876dbb128d3c3e))
* **transport:** persistent streams with length-delimited framing and buffer reuse ([936a0da](https://github.com/block65/wallhack/commit/936a0da02e435fd178b914cb72bef4f12bbd1c2d))
* **transport:** reduce broadcast channel capacity to prevent OOM ([5875824](https://github.com/block65/wallhack/commit/58758241963fb01449fa219d8cbc9bf36012bb63))


### Reverts

* **transport:** restore broadcast channel capacity to 65536 ([3f86316](https://github.com/block65/wallhack/commit/3f86316762c16801b779181cf1c6179c64638b5a))

## [0.1.2](https://github.com/block65/wallhack/compare/wallhack-v0.1.1...wallhack-v0.1.2) (2026-02-18)


### Bug Fixes

* **ci:** restore slim variant with proper feature isolation and fix r… ([e8f0f3b](https://github.com/block65/wallhack/commit/e8f0f3b43d2fe9e7fab24d3ea2d3a9c3c3f8c5a2))
* **ci:** restore slim variant with proper feature isolation and fix release upload race ([7a04c81](https://github.com/block65/wallhack/commit/7a04c81787aca98c03a62b3211c2e61ddf1f4eee))

## [0.1.1](https://github.com/block65/wallhack/compare/wallhack-v0.1.0...wallhack-v0.1.1) (2026-02-18)

## 0.1.0 (2026-02-18)


### Features

* add Cidr type and extend control protocol for routing ([7ea58a2](https://github.com/block65/wallhack/commit/7ea58a2c6cb665f951f7627a92be6ecea7a0dc02))
* add high connection rate warning for scan mode hint ([a978a3d](https://github.com/block65/wallhack/commit/a978a3d7c62ff535b2965202991f43bb9642a71c))
* add peer registry and route table for per-peer routing ([cf29aee](https://github.com/block65/wallhack/commit/cf29aee2bdd904bab69ea7000dab1f425b34b57f))
* add ping/pong messages to tunnel protocol ([f4320ee](https://github.com/block65/wallhack/commit/f4320ee81beca9f0f20c9c73726251c4ee0428cd))
* add ping/pong to tunnel protocol ([6f5ef6c](https://github.com/block65/wallhack/commit/6f5ef6c0ba6230c8968a3d05b9ff7b8733f8daee))
* add REPL support for exit nodes ([2f52bf3](https://github.com/block65/wallhack/commit/2f52bf3cbd0409d3897c9e6bf6c10965cb2dd194))
* add REST API for headless node control ([ae3c4de](https://github.com/block65/wallhack/commit/ae3c4de801930e9ecd15cccdb9d3bfa4c5335207))
* **bridge:** support persistent control and dedicated data streams for plane separation ([ec55455](https://github.com/block65/wallhack/commit/ec554553679b3abac00c7df151252718af9eddf3))
* enable PSK and peer limit configuration for tunnel auth ([33575e8](https://github.com/block65/wallhack/commit/33575e8804a6bf697865b535286fa251a88e0c2b))
* **entry:** propagate ICMP unreachable errors for UDP scanning ([7875eaf](https://github.com/block65/wallhack/commit/7875eaf743af0d425961de337f6905aa33c057f7))
* **entry:** SYN proxy for accurate TCP port scanning ([5741ad8](https://github.com/block65/wallhack/commit/5741ad83aadca40f000fea39598f20c6d551d6b1))
* **exit:** reap idle UDP/ICMP sessions to prevent unbounded memory growth ([75a90fc](https://github.com/block65/wallhack/commit/75a90fce64f0c37a7428ddbf1cef94741ca33246))
* implement routing control handlers and wire into servers ([686e352](https://github.com/block65/wallhack/commit/686e352c56ebf34eb2b5678bc76d09d5fefe0f19))
* integrate REST API with NodeApi trait ([e9998b5](https://github.com/block65/wallhack/commit/e9998b554dea60531c2e8430cced66f1551308f4))
* **proto:** add ControlMessage definition and arc-swap dependency ([aefb441](https://github.com/block65/wallhack/commit/aefb44118c3571009f1d36172fba0129db3fe042))
* **tls:** support TOFU certificate pinning via SHA-256 fingerprint ([a06a683](https://github.com/block65/wallhack/commit/a06a683c2d1c97187c7b22eb16bbf066b1975057))
* **transport:** rewire connection lifecycle to use persistent control stream and gated handshake ([b550168](https://github.com/block65/wallhack/commit/b5501685928b8a10c16b1165e21682b769570ef0))
* **transport:** update connection types to support control stream task and simplified handshake ([67537bf](https://github.com/block65/wallhack/commit/67537bf4598596cc3650e5aac591408a0623e8c3))
* wire PSK and fingerprint through server and client transports ([2ccc48e](https://github.com/block65/wallhack/commit/2ccc48e7e68ad49cb05f3a19c4f9f6a92317de19))
* **ws:** enable TLS by default with self-signed cert for zero-config security ([1e15455](https://github.com/block65/wallhack/commit/1e154558f3f51bef2027ee6913bc41d7ddab51c1))


### Bug Fixes

* add session status handshake to fix nmap showing all ports open ([6515f67](https://github.com/block65/wallhack/commit/6515f6741247d577e2e7594680f4a41d28378566))
* avoid panicking on unimplemented code paths ([d2d562d](https://github.com/block65/wallhack/commit/d2d562de5e48bf1b2169807ecb4d73ff17fc84d3))
* **entry:** bring TUN device UP on creation ([9cb264e](https://github.com/block65/wallhack/commit/9cb264ef78d0e6db82ac99d8c53a468e93ed0c34))
* **exit:** loop UDP recv in orchestrator to match TCP behaviour ([2dd3442](https://github.com/block65/wallhack/commit/2dd3442e4038ea8906e10c2f8a4fc0a2958a7f48))
* gate ICMP code behind #[cfg(unix)] for Windows compatibility ([cae65e9](https://github.com/block65/wallhack/commit/cae65e943fde74a970764792bbb33f9d99c85b69))
* **netstack:** drop unmatched TCP segments to prevent false open ports ([d0aa453](https://github.com/block65/wallhack/commit/d0aa453e9f0a33f44871fdd4859715e3ffcbeb5c))
* parallel TCP streams and QUIC stream exhaustion ([347c0b3](https://github.com/block65/wallhack/commit/347c0b30681047443f164ad5fb68a41a30d7c20b))
* prevent timing side-channel in credential validation ([0090b0b](https://github.com/block65/wallhack/commit/0090b0bccd3d41f701bf3b141f04be801f431710))
* **repl:** use exit_id as peer ID and normalize IPv4-mapped addresses ([a349de6](https://github.com/block65/wallhack/commit/a349de6bac82024822a3032892f0c3e3e9295fb3))
* **tls:** default fingerprint hash prefix to sha256 ([51b130b](https://github.com/block65/wallhack/commit/51b130bfde589801c924bad82160d86cac0f12da))
* **transport:** preserve control_tx lifetime to prevent control stream death ([39894ce](https://github.com/block65/wallhack/commit/39894cec44dfe694879cc3e0c7918b0c8b2cec6a))
* update client run_incoming_data calls for new signature ([5c0b48a](https://github.com/block65/wallhack/commit/5c0b48abc3647d708a1485bc4cbc1191ab703768))
* update WebSocket client and bench scripts for REPL changes ([923a3b6](https://github.com/block65/wallhack/commit/923a3b6ebab5740d7330f546397d49c554395955))
* upgrade bytes to 1.11.1 (security) ([40e8962](https://github.com/block65/wallhack/commit/40e8962cb524f0178a5a9a96bd159cbe9e3db695))


### Performance Improvements

* **control:** replace RwLock with arc-swap for wait-free reads ([53117e1](https://github.com/block65/wallhack/commit/53117e1ddc798e1c199a8210dacd37fd3b964e47))
* **netstack:** replace 1ms sleep poll with epoll-based fd readiness ([37ddd67](https://github.com/block65/wallhack/commit/37ddd675d718555c1d7873e40323ab309a0e1b8d))
* **netstack:** zero-copy peek_all_ingress and reduce poll interval to 1ms ([b33274a](https://github.com/block65/wallhack/commit/b33274a646da555157c3d3fee8638f4cc1f8691b))
* **orchestrator:** use bytes::Bytes for protobuf data fields ([6cdb10b](https://github.com/block65/wallhack/commit/6cdb10b5588449741c75cac608876dbb128d3c3e))
* **transport:** persistent streams with length-delimited framing and buffer reuse ([936a0da](https://github.com/block65/wallhack/commit/936a0da02e435fd178b914cb72bef4f12bbd1c2d))
* **transport:** reduce broadcast channel capacity to prevent OOM ([5875824](https://github.com/block65/wallhack/commit/58758241963fb01449fa219d8cbc9bf36012bb63))


### Reverts

* **transport:** restore broadcast channel capacity to 65536 ([3f86316](https://github.com/block65/wallhack/commit/3f86316762c16801b779181cf1c6179c64638b5a))
