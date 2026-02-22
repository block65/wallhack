# Type System Improvements

Small, independent improvements to the type system that reduce ambiguity,
prevent misuse, and improve error handling ergonomics. These are individually
low-risk and can be done in any order or split across multiple PRs.

## Scope

`crates/wallhack/src/types.rs`,
`crates/wallhack/src/control/metrics.rs`,
`crates/wallhack/src/transport/bridge.rs`,
`crates/wallhack/src/exit/orchestrator.rs`,
and call sites throughout `crates/cli/`

---

## Items

### 1. Newtype wrappers for domain primitives

`PeerId`, `PeerName`, and `Psk` are raw `String`s throughout the codebase.
The compiler cannot distinguish a peer ID from a PSK from a peer name. Introduce
newtypes:

```rust
/// A peer's stable identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);  // or Arc<str> if cloning is hot

/// A peer's human-readable display name.
#[derive(Debug, Clone)]
pub struct PeerName(pub String);

/// A pre-shared key for tunnel authentication.
/// Zeroed on drop.
#[derive(Debug, Clone, zeroize::ZeroizeOnDrop)]
pub struct Psk(String);
```

Update all `String` usages at config and connection boundaries to use the
appropriate newtype. The `ZeroizeOnDrop` on `Psk` ensures the secret is wiped
from memory when the value is dropped.

This is a mechanical change (find all `psk: Option<String>`, `peer_id: String`
etc.) but touches many call sites — plan for a wide diff.

### 2. `TryFrom<ProtoNodeRole>` error — proper enum, not `String`

**File:** `crates/wallhack/src/types.rs`

```rust
// before
impl TryFrom<ProtoNodeRole> for NodeRole {
    type Error = String;
    fn try_from(role: ProtoNodeRole) -> Result<Self, Self::Error> {
        match role {
            ProtoNodeRole::RoleUnknown => Err("unknown node role".to_string()),
            ...
        }
    }
}

// after
#[derive(Debug, thiserror::Error)]
pub enum NodeRoleError {
    #[error("unknown node role value: {0:?}")]
    Unknown(ProtoNodeRole),
}

impl TryFrom<ProtoNodeRole> for NodeRole {
    type Error = NodeRoleError;
    fn try_from(role: ProtoNodeRole) -> Result<Self, Self::Error> {
        match role {
            ProtoNodeRole::RoleUnknown => Err(NodeRoleError::Unknown(role)),
            ...
        }
    }
}
```

### 3. `Metrics` — make fields private

**File:** `crates/wallhack/src/control/metrics.rs`

All six `AtomicU64` fields are `pub`. Callers can call `store(0)` or
`fetch_add(u64::MAX)` directly, bypassing the inc/dec methods that are the
correct API surface.

```rust
// before
pub struct Metrics {
    pub bytes_in: AtomicU64,
    ...
}

// after
pub struct Metrics {
    bytes_in: AtomicU64,
    ...
}
```

The existing `inc_*`/`dec_*`/`snapshot()` methods are the public API.
Any direct field access in tests should be rewritten to go through the methods.

### 4. `run_control_loop` — parameter struct

**File:** `crates/wallhack/src/transport/bridge.rs`

`run_control_loop` has 7 parameters, 4 of which are `Option<&Tx>` channel
handles. Group the channel handles into a struct:

```rust
pub struct ControlLoopHandles<'a> {
    pub instructions_tx: Option<&'a broadcast::Sender<EntryNodeInstruction>>,
    pub responses_rx: Option<broadcast::Receiver<ExitNodeResponse>>,
    pub control_request_tx: Option<&'a mpsc::Sender<ControlRequest>>,
    pub control_response_tx: Option<&'a mpsc::Sender<ControlResponse>>,
}
```

Update `run_control_loop` and both call sites (`run_control_stream_initiator`,
`run_control_stream_acceptor`) to use the struct. This also makes it
immediately obvious when a handle is intentionally omitted (the field is `None`)
vs accidentally left out (the field doesn't compile).

**Note:** After task 07 (broadcast→mpsc), the types in this struct change.
Coordinate or do this after 07.

### 5. `ControlLoopExit::Disconnect` — replace `String` with enum

**File:** `crates/wallhack/src/transport/bridge.rs`

```rust
// before
pub enum ControlLoopExit {
    Disconnect(String),
    ...
}

// after
#[derive(Debug)]
pub enum DisconnectReason {
    PeerClosed,
    IdleTimeout,
    AuthFailure,
    ProtocolError(String),
    IoError(std::io::Error),
}

pub enum ControlLoopExit {
    Disconnect(DisconnectReason),
    ...
}
```

Update all match arms. Callers that currently parse the string can now match
structurally.

### 6. `ExitNodeResponse` construction boilerplate

**File:** `crates/wallhack/src/exit/orchestrator.rs`

The struct literal:
```rust
ExitNodeResponse {
    pair: Some(pair),
    response: Some(exit_node_response::Response::UdpResponse(...)),
}
```

is repeated many times. Add a constructor:
```rust
impl ExitNodeResponse {
    pub fn with_pair(pair: SocketAddressPair, response: exit_node_response::Response) -> Self {
        Self { pair: Some(pair), response: Some(response) }
    }
}
```

---

## Notes

- Items 1–3 are independent and can be done in any order
- Item 4 should wait until after task 07 to avoid double-touching bridge.rs
- Item 5 should be done alongside or after item 4
- Item 6 is a quick local refactor inside orchestrator.rs

## Acceptance criteria

- `just check` passes
- No public `AtomicU64` fields on `Metrics`
- `PeerId`, `PeerName`, `Psk` newtypes exist; raw `String` used for peer
  identity at no more than the CLI parsing boundary
- `run_control_loop` parameter count ≤ 4 (struct takes the rest)
- `ControlLoopExit::Disconnect` carries `DisconnectReason`, not `String`
