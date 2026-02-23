# Changelog

## 0.1.0 (2026-02-18)


### Features

* **entry:** SYN proxy for accurate TCP port scanning ([5741ad8](https://github.com/block65/wallhack-core/commit/5741ad83aadca40f000fea39598f20c6d551d6b1))


### Bug Fixes

* **wallhack-core-netstack:** drop unmatched TCP segments to prevent false open ports ([d0aa453](https://github.com/block65/wallhack-core/commit/d0aa453e9f0a33f44871fdd4859715e3ffcbeb5c))
* **wallhack-core-netstack:** wake poll loop after UDP send_to to flush egress ([bdaf357](https://github.com/block65/wallhack-core/commit/bdaf3578a9e37c96e6a6adf15af05fef756871a2))
* parallel TCP streams and QUIC stream exhaustion ([347c0b3](https://github.com/block65/wallhack-core/commit/347c0b30681047443f164ad5fb68a41a30d7c20b))


### Performance Improvements

* **wallhack-core-netstack:** increase TCP socket buffers from 64 KiB to 256 KiB ([6d975b5](https://github.com/block65/wallhack-core/commit/6d975b5e6e1dca1225ed0eb84547241269524d40))
* **wallhack-core-netstack:** replace 1ms sleep poll with epoll-based fd readiness ([37ddd67](https://github.com/block65/wallhack-core/commit/37ddd675d718555c1d7873e40323ab309a0e1b8d))
* **wallhack-core-netstack:** zero-copy peek_all_ingress and reduce poll interval to 1ms ([b33274a](https://github.com/block65/wallhack-core/commit/b33274a646da555157c3d3fee8638f4cc1f8691b))
* replace std::sync::Mutex with parking_lot in wallhack-core-netstack ([b5fa48e](https://github.com/block65/wallhack-core/commit/b5fa48e0b1314e3f34d2619b12990607e9a2808b))
