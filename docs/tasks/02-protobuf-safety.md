# Protobuf Boundary Safety

Fix silent data corruption at the protobuf deserialization boundary. Both
issues allow a malformed or adversarially-crafted protobuf message to produce
wrong data with no error, no log, and no indication to the caller.

## Scope

`crates/protobuf/src/`

---

## Items

### 1. `vec_to_sized_array` — return `Result` instead of silently truncating

**File:** `crates/protobuf/src/helpers.rs`

Current behaviour: if the input slice is longer than `N`, the excess bytes are
silently dropped. Used to convert raw byte fields into fixed-size IP address
arrays — a 17-byte "IPv4 address" produces a wrong address, not a parse error.

```rust
// before
pub fn vec_to_sized_array<const N: usize>(vec: &[u8]) -> [u8; N] {
    let mut arr = [0u8; N];
    let len_to_copy = std::cmp::min(vec.len(), N);
    arr[..len_to_copy].copy_from_slice(&vec[..len_to_copy]);
    arr
}

// after — add a LengthMismatch error to ConversionError, or use a local one
pub fn vec_to_sized_array<const N: usize>(vec: &[u8]) -> Result<[u8; N], ConversionError> {
    if vec.len() != N {
        return Err(ConversionError::InvalidLength { expected: N, got: vec.len() });
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(vec);
    Ok(arr)
}
```

Update `ConversionError` to include the `InvalidLength` variant. Update all
~4 call sites (all in `socket_set.rs`) to propagate with `?`.

### 2. Port truncation — `u16::try_from` instead of silent cast

**File:** `crates/protobuf/src/socket_set.rs`

The file-level `#![allow(clippy::cast_possible_truncation)]` exists to
suppress warnings on `src.port as u16` where `port: u32`. A port value above
65535 silently truncates rather than erroring.

```rust
// before (with the file-level allow suppressing the warning)
port: src.port as u16,

// after — inside the existing TryFrom impls
port: u16::try_from(src.port).map_err(|_| ConversionError::InvalidPort)?,
```

- Remove the file-level `#![allow(clippy::cast_possible_truncation)]`
- Add `InvalidPort` variant to `ConversionError` if not already present
- Apply in every `TryFrom` impl in the file (IPv4 and IPv6 variants)

## Acceptance criteria

- `just check` passes with no file-level allows in `socket_set.rs`
- Passing a 5-byte vec to `vec_to_sized_array::<4>` returns `Err`, not `Ok`
  with truncated data
- Passing `port: 70_000u32` in a protobuf message returns a conversion error
- New unit tests for both error cases in `#[cfg(test)]` modules
