# Changelog

## 0.1.0 (2026-02-18)


### Features

* add Cidr type and extend control protocol for routing ([7ea58a2](https://github.com/block65/wallhack/commit/7ea58a2c6cb665f951f7627a92be6ecea7a0dc02))
* add ping/pong messages to tunnel protocol ([f4320ee](https://github.com/block65/wallhack/commit/f4320ee81beca9f0f20c9c73726251c4ee0428cd))
* **proto:** add auth_token field to ExitNodeHello for PSK auth ([12ac7d6](https://github.com/block65/wallhack/commit/12ac7d68ed5123b68df82c1dc079a272febcba71))
* **proto:** add ControlMessage definition and arc-swap dependency ([aefb441](https://github.com/block65/wallhack/commit/aefb44118c3571009f1d36172fba0129db3fe042))


### Bug Fixes

* add session status handshake to fix nmap showing all ports open ([6515f67](https://github.com/block65/wallhack/commit/6515f6741247d577e2e7594680f4a41d28378566))
* **protobuf:** vendor protoc binary to eliminate system dependency ([09a6613](https://github.com/block65/wallhack/commit/09a6613e3802d071e1df3d0f56dead98792b0513))
* upgrade bytes to 1.11.1 (security) ([40e8962](https://github.com/block65/wallhack/commit/40e8962cb524f0178a5a9a96bd159cbe9e3db695))


### Performance Improvements

* **orchestrator:** use bytes::Bytes for protobuf data fields ([6cdb10b](https://github.com/block65/wallhack/commit/6cdb10b5588449741c75cac608876dbb128d3c3e))
