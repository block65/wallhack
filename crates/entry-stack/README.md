# wallhack-entry-stack

A high-performance, userspace TCP/IP stack designed for transparent proxying, network tunneling, and security research.

## What is it?

`wallhack-entry-stack` is a deterministic, asynchronous network stack built on top of `smoltcp`. It provides a standard async I/O interface (Streams, Listeners, and Sockets) that operates entirely in userspace over virtual L3 (IP) devices. 

Unlike the standard library's networking primitives which rely on the host operating system's kernel stack, this crate implements the full TCP/IP state machine internally. This allows an application to manage network traffic without being constrained by host kernel configurations, firewall rules, or socket limits.

## Why does it exist?

Traditional networking applications are bound by the "Host OS Bottleneck." This crate was developed to solve three specific challenges:

### 1. Transparent Proxying & AnyIP
In a tunneling or VPN context, you often need to accept traffic destined for *any* IP address or *any* port. Standard OS stacks require explicit binding to specific addresses or complex `iptables/nftables` redirection. `wallhack-entry-stack` supports "AnyIP" and Just-In-Time (JIT) binding, allowing the application to dynamically spawn listeners as traffic arrives for previously unknown destinations.

### 2. High-Fidelity Scanning Support
Security tools like `nmap` often detect the presence of proxies or VPNs by observing subtle differences in how a TCP implementation responds to probes (e.g., specific flag combinations or sequence number patterns). By using a dedicated userspace stack, this crate provides a consistent and configurable "network identity" that is completely decoupled from the host OS.

### 3. Deterministic Network Testing
Testing complex network protocols is notoriously difficult with standard sockets due to non-deterministic timing and OS-level buffering. Because this stack is decoupled from the hardware, it allows for:
* **PCAP Replay**: Feeding raw network captures into the stack to reproduce bugs deterministically.
* **Virtual Topologies**: Simulating entire networks within a single process without needing root privileges or complex container setups.

## Why not just use `smoltcp`?

`smoltcp` is a fantastic, low-level, and `#![no_std]` capable TCP/IP implementation. However, it is fundamentally synchronous and requires the developer to manually manage the `SocketSet`, advance the state machine via `Interface::poll` at exact timestamps, and implement their own logic for waking async tasks.

`wallhack-entry-stack` provides the "missing middle" required for modern applications:

1.  **Tokio Integration**: It bridges `smoltcp`'s synchronous buffers to the `AsyncRead` and `AsyncWrite` ecosystem, allowing you to use standard tools like `copy_bidirectional`.
2.  **Autonomous Poll Loop**: It manages a background task that intelligently polls the stack based on both hardware events (ingress) and internal protocol timers (retransmissions, ACK delays).
3.  **Dynamic Connection Management**: It implements the JIT (Just-In-Time) binding logic and `TcpListenerAny` abstractions required for AnyIP/transparent proxying—features that are not part of `smoltcp`'s core protocol logic.
4.  **Thread Safety**: It provides the `Arc<Mutex<...>>` architecture necessary to share a single network interface safely across multiple Tokio tasks and threads.

## Key Capabilities

* **Userspace TCP/IP**: Full L3/L4 implementation excluding the kernel.
* **Async/Await Integration**: Built-in support for the Tokio runtime.
* **JIT Port Binding**: Automatically respond to connection attempts on any port.
* **AnyIP Support**: Accept traffic for any destination IP address.
* **High Throughput**: Optimized for modern link speeds with configurable buffer management.

## Constraints & Requirements

### 1. L3 (IP) Medium Only
This crate is designed specifically for **Layer 3 (TUN)** devices. It operates on raw IPv4/IPv6 packets and does not implement Ethernet (Layer 2) framing or ARP/Neighbor Discovery. If used with a TAP device or physical Ethernet interface, an external framing layer is required.

### 2. Global Mutex Bottleneck
To maintain the safety and consistency of the network state, the entire stack is protected by a single global `Mutex`. 
* **Impact**: While individual I/O operations are asynchronous, the underlying state machine is progressed sequentially. This makes the stack throughput-bound to a single CPU core.
* **Use Case**: This is ideal for high-throughput tunneling of several heavy streams, but may become a bottleneck for tens of thousands of concurrent, high-frequency small-packet streams.

### 3. Resource Requirements
By default, the stack is tuned for high-performance networking (e.g., ~2Gbps links):
* **Memory**: Each TCP socket allocates 512KiB by default (256KiB RX + 256KiB TX buffers).
* **Socket Limits**: The stack currently relies on host memory as its natural limit. Users should configure `StackConfig` buffer sizes according to their expected concurrent connection count and available RAM.

### 4. Async Runtime
The `async` features of this crate are designed specifically for the **Tokio** runtime. The background poll loop assumes a multi-threaded executor is available to drive the state machine while the application performs I/O.
