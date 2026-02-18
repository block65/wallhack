# Changelog

## [0.1.1](https://github.com/block65/wallhack/compare/exit_adapter-v0.1.0...exit_adapter-v0.1.1) (2026-02-18)


### Bug Fixes

* reorder cfg(unix) import to satisfy rustfmt ([e574113](https://github.com/block65/wallhack/commit/e574113002b9f04f083d0cc4ed4f11a11473bfe3))

## 0.1.0 (2026-02-18)


### Features

* **exit:** reap idle UDP/ICMP sessions to prevent unbounded memory growth ([75a90fc](https://github.com/block65/wallhack/commit/75a90fce64f0c37a7428ddbf1cef94741ca33246))


### Bug Fixes

* **exit-adapter:** gate ICMP session behind #[cfg(unix)] ([98e2ce2](https://github.com/block65/wallhack/commit/98e2ce29a57869c6108357c319995d405ddb980b))
* gate ICMP code behind #[cfg(unix)] for Windows compatibility ([cae65e9](https://github.com/block65/wallhack/commit/cae65e943fde74a970764792bbb33f9d99c85b69))


### Performance Improvements

* **orchestrator:** use bytes::Bytes for protobuf data fields ([6cdb10b](https://github.com/block65/wallhack/commit/6cdb10b5588449741c75cac608876dbb128d3c3e))
