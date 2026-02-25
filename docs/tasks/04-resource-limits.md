# Network Resource Limits

Add hard bounds to data structures that currently grow without limit in
response to network traffic. Each of these is a DoS vector: an attacker
controlling source IPs or ports can exhaust memory or file descriptors.

## Scope

`crates/wallhack/src/entry/manager.rs`,
`crates/wallhack/src/entry/actor.rs`,
`crates/entry-stack/src/async_stack/`

---

## Items

### 1. UDP session map — cap at a configurable maximum

**File:** `crates/wallhack/src/entry/manager.rs:62`

`udp_sessions: HashMap<(IpEndpoint, u16), UdpSession>` grows without bound.
Each unique (source IP, source port, destination port) tuple adds a new entry.
An attacker can exhaust RAM by rotating source ports.

**Fix:**

```rust
const MAX_UDP_SESSIONS: usize = 100_000;

// In the UDP recv branch, before inserting:
if self.udp_sessions.len() >= MAX_UDP_SESSIONS {
    tracing::warn!("UDP session table full, dropping packet");
    self.metrics.inc_packets_dropped(1); // add this counter
    continue;
}
self.udp_sessions.entry(key).or_insert_with(|| { ... });
```

Expose `MAX_UDP_SESSIONS` as a `ConnectionManager` constructor argument so
it can be tuned per-deployment. Default: 100 000.

### 2. `recent_connections` rate-tracking vec — bounded ring buffer

**File:** `crates/wallhack/src/entry/manager.rs:64,142`

The rate-detection vec (`Vec<Instant>`) grows by one entry per TCP connection
accepted, then is pruned by time. Under a SYN flood, it grows faster than it
shrinks.

**Fix:** Replace with a fixed-size `VecDeque` of at most
`HIGH_RATE_THRESHOLD as usize * RATE_WINDOW.as_secs() as usize * 2` entries
(e.g. 500 for the current thresholds). Discard the oldest when full:

```rust
const MAX_RATE_SAMPLES: usize = 500;

self.recent_connections.push_back(now);
if self.recent_connections.len() > MAX_RATE_SAMPLES {
    self.recent_connections.pop_front();
}
```

Or use a simple atomic counter with periodic reset — whichever is simpler.

### 3. TUN device pending packet queue — add a watermark

**File:** `crates/wallhack/src/entry/actor.rs:120`

`SmoltcpTunDevice.pending: VecDeque<Vec<u8>>` accumulates injected packets
(e.g. SYN re-injections, ICMP responses). If the poll loop stalls, this queue
grows unbounded.

**Fix:** Cap at a reasonable size (e.g. `MTU * 64` packets). When full, drop
the oldest packet (head) and log at trace level:

```rust
const MAX_PENDING_PACKETS: usize = 64;

pub fn inject_pending(&mut self, packet: Vec<u8>) {
    if self.pending.len() >= MAX_PENDING_PACKETS {
        tracing::trace!("pending queue full, dropping oldest packet");
        self.pending.pop_front();
    }
    self.pending.push_back(packet);
}
```

### 4. JIT TCP listen socket cap

**File:** `crates/entry-stack/src/async_stack/mod.rs`, JIT bind path

Each SYN to an unbound port creates a new smoltcp listen socket. An attacker
sending SYNs to 65 535 different ports creates 65 535 sockets, exhausting
smoltcp's `SocketSet`.

**Fix:** Add a `max_jit_sockets: usize` field to `StackConfig` (default: 256).
Track the current JIT socket count with an atomic or by querying the
`SocketSet`. When at limit, skip `jit_bind_port` and let smoltcp RST the
connection naturally:

```rust
if current_jit_count >= config.max_jit_sockets {
    tracing::debug!(port = dst_port, "JIT socket limit reached, RST-ing");
    // do not bind; smoltcp RSTs the SYN
} else {
    let _ = jit_bind_port(...);
}
```

## Acceptance criteria

- Under a UDP flood from many source IPs, RSS stabilises rather than climbing
  indefinitely
- JIT socket count does not exceed the configured limit
- All new limits are constructor parameters or `StackConfig` fields, not only
  constants, so they can be tested and tuned
- `just check` passes
