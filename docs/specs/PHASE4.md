Wallhack Refactoring Spec - Phase 4: Reliability & Performance Modes

Status: Ready for Implementation
Prerequisites: Phase 3 Complete (System works, but is "Honest/Slow")
Scope: crates/wallhack (Host & Agent) & crates/wallhack-netstack

1. Executive Summary

Phase 4 transforms the system from a "One Size Fits All" tool into a specialized engine that adapts to the user's intent. We introduce Traffic Profiles to solve the conflict between Nmap (needs speed/churn) and RDP/SSH (needs stability/persistence).

The Goal:

Profiles: Add --mode scan (Fast/Optimistic) and --mode session (Reliable/Honest).

Reliability: Implement a "Status Handshake" so the Agent can report Connection Refused explicitly.

UX: Detect when the user is scanning in the wrong mode and warn them ("RTFM" feature).

2. Traffic Profiles (Configuration)

Introduce a configuration struct that controls stack behavior.

2.1. The Modes

Feature

Session (Default)

Scan (--scan)

TCP Handshake

Honest: Wait for Agent to connect before sending SYN-ACK.

Optimistic: Send SYN-ACK immediately. Buffer early data.

UDP Timeout

30 Seconds: Keeps RDP/DNS sessions alive during pauses.

500 ms: Aggressively recycles slots to prevent Agent exhaustion.

Use Case

SSH, RDP, VNC, Browsing.

Nmap, Masscan, Dirbuster.

2.2. CLI Update

Update main.rs arguments to accept --scan flag.

Default: TrafficConfig::session()

If flag present: TrafficConfig::scan()

3. The "Status Handshake" (Reliability)

Since smoltcp (in Optimistic mode) accepts connections locally that might fail remotely, we need a way to tear them down gracefully.

3.1. Protobuf Update

Add a SessionStatus message to the protocol.

enum StatusCode {
CONNECTED = 0;
CONNECTION_REFUSED = 1;
TIMEOUT = 2;
UNREACHABLE = 3;
}
message SessionStatus {
StatusCode code = 1;
string error_message = 2;
}

3.2. Host Logic (SessionTask)

Update the connection sequence in src/host/session.rs:

Send: SessionInit (Existing).

Wait: Read SessionStatus from the Agent.

Note: During this wait, smoltcp automatically buffers any data Nmap sends in its internal RX window. We do not need a custom buffer; we simply delay the start of the pipe loop.

Handle:

CONNECTED: Begin tokio::io::copy_bidirectional.

REFUSED / TIMEOUT:

Action: Abort the local TcpStream.

Crucial: This must trigger a TCP RST (Reset) to the local tool, not a FIN. This ensures Nmap correctly marks the port as Closed/Filtered.

3.3. Agent Logic

Update src/agent/mod.rs:

Read: SessionInit.

Action: Attempt TcpStream::connect().

Report: Send SessionStatus result back to Host.

If Ok: Send CONNECTED.

If Err(ConnectionRefused): Send CONNECTION_REFUSED.

4. The Optimistic Engine (Scan Mode)

4.1. Netstack Update

Modify InnerStack in wallhack-netstack to support HandshakeMode.

Logic: Modify the JIT Listener logic.

If Optimistic: When the JIT listener receives a SYN, force an immediate transition to ESTABLISHED (or ensure smoltcp replies instantly without waiting for an "Accept" call to drive it).

Note: smoltcp does this by default if a listener exists. "Honest" mode actually requires delaying the SYN-ACK, often by holding the SYN in a "Pending" queue or creating the listener lazily.

Simplification: If implementing "Honest" blocking in smoltcp is too complex, Phase 4 can rely on the Status Handshake alone to close bad connections quickly, which is often "Good Enough" for Nmap even without perfect pre-ACK timing.

5. UX Heuristic (The "RTFM" Warning)

The ConnectionManager should track the Connection Rate (New flows per second).

Logic:

// Inside the accept loop
let now = Instant::now();
self.rate_limiter.add_event(now);

if self.config.mode == Mode::Session && self.rate_limiter.rate() > 50.0 {
warn_once!("⚠️ High connection rate detected! Scans will be slow in Session mode.");
warn_once!("💡 Tip: Restart with --scan for high-speed enumeration.");
}
