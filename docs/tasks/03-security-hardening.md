# Security Hardening

Fix three distinct security issues that are independently correctible without
architectural changes.

## Scope

`crates/wallhack/src/api/auth.rs`, `crates/wallhack/src/server/tls.rs`,
`crates/wallhack/src/tls/verifiers.rs`, `crates/wallhack/src/control/client.rs`,
`crates/wallhack/src/control/server.rs` (tests)

---

## Items

### 1. Deduplicate `SkipServerVerification`

`SkipServerVerification` (skip all TLS certificate verification) is implemented
three separate times:

- `crates/wallhack/src/tls/verifiers.rs` — the canonical version; correctly
  delegates signature verification to the `CryptoProvider`
- `crates/wallhack/src/control/client.rs:140` — inline version that hardcodes
  a list of signature schemes and returns `assertion()` for all signatures
  **without actually verifying them**
- `crates/wallhack/src/control/server.rs` (test module) — another inline copy

The control client version is weaker because it doesn't actually verify
TLS 1.2/1.3 signatures — it always returns `HandshakeSignatureValid::assertion()`
regardless of whether the signature is correct. This means certificate
*forgery* would succeed even if you did care about the cert content.

**Fix:**
- Delete the two inline copies
- Use `crate::tls::verifiers::SkipServerVerification::new()` everywhere
- Ensure the control client and test module import from `tls::verifiers`

### 2. Empty CA roots file silently disables mTLS

**File:** `crates/wallhack/src/server/tls.rs`

When `ca_roots` is configured but the PEM file exists and is empty (deleted,
corrupted, or accidentally truncated), the root store is empty and client auth
is silently not enforced. The server accepts any client certificate or none.

**Fix:** After parsing the CA roots file, assert the store is non-empty:
```rust
if roots.is_empty() {
    return Err(Error::EmptyCaStore);
}
```
Add `EmptyCaStore` to the `Error` enum with a message like:
`"ca_roots is configured but the file contained no valid certificates"`.

### 3. API auth — constant-time username check and failed auth logging

**File:** `crates/wallhack/src/api/auth.rs`

Two issues:

**a) Username timing leak.** Username comparison currently short-circuits on
mismatch before password comparison starts, leaking whether a username exists
via response time. Fix: compare both username and password in constant time,
then AND the results:

```rust
use subtle::ConstantTimeEq;

let username_ok = username.as_bytes().ct_eq(provided_username.as_bytes());
let password_ok = password.as_bytes().ct_eq(provided_password.as_bytes());
// u8 AND: both must be 1
let authed = (username_ok & password_ok).unwrap_u8() == 1;
```

**b) No logging of failed auth attempts.** Failed authentication is a security
event. At minimum log it at `warn!` level with the source IP, without logging
the credential values:

```rust
if !authed {
    tracing::warn!(remote = %source_ip, "authentication failed");
    return Err(AuthError::Unauthorized);
}
```

The source IP should be threaded in from the request context (it is available
via the axum extractor chain).

**Note:** Rate limiting (token bucket per source IP) is left for a future task.
The constant-time comparison and logging are the correctness-critical fixes here.

## Acceptance criteria

- `just check` passes
- Only one `SkipServerVerification` struct exists in the codebase
- Configuring an empty CA roots file returns an error at startup, not silent
  mTLS bypass
- Failed API auth writes a `warn!` log event
- Username and password comparisons use `subtle::ConstantTimeEq`
