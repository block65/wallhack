//! Memory budget integration tests.
//!
//! Measures heap allocations for all major runtime components so we can set
//! hard budgets and catch regressions before they ship to constrained hardware
//! (Raspberry Pi, t4g.nano 512 MB, etc.).
//!
//! Integration tests compile as their own binary so `#[global_allocator]` is
//! isolated from the rest of the test suite.
//!
//! Run with:
//! ```sh
//! cargo test -p wallhack --test memory_budget -- --nocapture --test-threads=1
//! ```

#![allow(
    unsafe_code,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::used_underscore_binding
)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::broadcast;
use wallhack_wire::data::{EntryNodeInstruction, ExitNodeResponse, TunnelMessage};

// ---------------------------------------------------------------------------
// Tracking allocator
// ---------------------------------------------------------------------------

struct TrackingAllocator {
    allocated: AtomicUsize,
    peak: AtomicUsize,
}

impl TrackingAllocator {
    const fn new() -> Self {
        Self {
            allocated: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    fn current(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }

    fn reset_peak(&self) {
        self.peak
            .store(self.allocated.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let prev = self.allocated.fetch_add(layout.size(), Ordering::Relaxed);
            let current = prev + layout.size();
            let mut old_peak = self.peak.load(Ordering::Relaxed);
            while current > old_peak {
                match self.peak.compare_exchange_weak(
                    old_peak,
                    current,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => old_peak = actual,
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.allocated.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator::new();

/// Serialize tests so allocations don't interleave.
static SERIAL: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run `f`, return its result and the net heap delta in bytes.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    std::thread::sleep(std::time::Duration::from_millis(10));
    let before = ALLOC.current();
    let result = f();
    let after = ALLOC.current();
    (result, after.saturating_sub(before))
}

/// Run `f`, return its result, net heap delta, and peak delta.
fn measure_peak<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    std::thread::sleep(std::time::Duration::from_millis(10));
    ALLOC.reset_peak();
    let before = ALLOC.current();
    let result = f();
    let after = ALLOC.current();
    let peak = ALLOC.peak();
    (
        result,
        after.saturating_sub(before),
        peak.saturating_sub(before),
    )
}

#[allow(clippy::cast_precision_loss)]
fn fmt_bytes(n: usize) -> String {
    if n >= 1_048_576 {
        format!("{:.2} MB", n as f64 / 1_048_576.0)
    } else if n >= 1_024 {
        format!("{:.1} KB", n as f64 / 1_024.0)
    } else {
        format!("{n} B")
    }
}

/// Build an `ExitNodeResponse` carrying `payload_size` bytes of UDP data.
fn make_response_with_payload(payload_size: usize) -> ExitNodeResponse {
    ExitNodeResponse {
        response: Some(
            wallhack_wire::data::exit_node_response::Response::UdpResponse(
                wallhack_wire::data::UdpResponse {
                    response: Some(wallhack_wire::data::udp_response::Response::DataRecv(
                        wallhack_wire::data::UdpDataRecvResponse {
                            data: bytes::Bytes::from(vec![0xABu8; payload_size]),
                        },
                    )),
                },
            ),
        ),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Hardware budget constants
//
// These represent the TOTAL heap budget for the wallhack process on each
// target.  Individual component budgets are derived from these.
// ---------------------------------------------------------------------------

/// t4g.nano / RPi Zero: 512 MB total, ~400 MB usable after OS.
/// Wallhack should stay well under half of usable memory.
const BUDGET_CONSTRAINED: usize = 64 * 1024 * 1024; // 64 MB

/// RPi 4 / small VPS: 1-4 GB.
const BUDGET_MODERATE: usize = 256 * 1024 * 1024; // 256 MB

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Protobuf struct sizes — deterministic, zero-cost to check.
/// If someone adds fields to the proto, this catches the stack-size increase.
#[test]
fn struct_sizes() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let sizes = [
        (
            "EntryNodeInstruction",
            std::mem::size_of::<EntryNodeInstruction>(),
            256,
        ),
        (
            "ExitNodeResponse",
            std::mem::size_of::<ExitNodeResponse>(),
            256,
        ),
        ("TunnelMessage", std::mem::size_of::<TunnelMessage>(), 512),
    ];

    println!();
    println!("[memory] === struct sizes (stack) ===");
    for (name, size, budget) in &sizes {
        println!("[memory]   {name:<30} {size:>4} B   (budget: {budget} B)");
        assert!(
            *size <= *budget,
            "{name} grew to {size} bytes, budget is {budget}",
        );
    }
}

/// Cost of a single broadcast channel at various capacities.
///
/// This is the dominant per-connection cost. The ring buffer is allocated
/// up-front at channel creation, even if no messages have been sent yet.
#[test]
fn broadcast_channel_scaling() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === broadcast<ExitNodeResponse> empty channel cost ===");
    println!(
        "[memory]   {:>10}  {:>10}  {:>14}",
        "capacity", "1 chan", "connection (×2)"
    );
    println!(
        "[memory]   {:>10}  {:>10}  {:>14}",
        "--------", "------", "--------------"
    );

    let capacities: &[usize] = &[64, 256, 512, 1024, 2048, 4096, 8192, 16384, 65536];
    let mut results: Vec<(usize, usize)> = Vec::new();

    for &cap in capacities {
        let (_tx, cost) = measure(|| broadcast::channel::<ExitNodeResponse>(cap));
        let pair = cost * 2;
        println!(
            "[memory]   {cap:>10}  {:>10}  {:>14}",
            fmt_bytes(cost),
            fmt_bytes(pair),
        );
        results.push((cap, cost));
    }

    // Budget checks against both targets.
    let (_, cost_65536) = *results.iter().find(|(c, _)| *c == 65536).unwrap();
    let pair_65536 = cost_65536 * 2;

    let constrained_quarter = BUDGET_CONSTRAINED / 4;
    if pair_65536 >= constrained_quarter {
        println!(
            "[memory]   WARNING: connection pair at 65536 ({}) exceeds constrained budget ({}, 1/4 of {})",
            fmt_bytes(pair_65536),
            fmt_bytes(constrained_quarter),
            fmt_bytes(BUDGET_CONSTRAINED),
        );
    }

    // Hard assert: must fit moderate budget.
    assert!(
        pair_65536 < BUDGET_MODERATE / 4,
        "Connection pair at capacity 65536 costs {}, exceeds moderate budget {}",
        fmt_bytes(pair_65536),
        fmt_bytes(BUDGET_MODERATE / 4),
    );
}

/// Cost of one full "connection" — the channels + control mpsc that each
/// QUIC/WS connection allocates.
///
/// This is what `QuicServer::accept` / `QuicClient::connect` creates.
#[test]
fn per_connection_overhead() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === per-connection overhead ===");

    // Current production capacity
    let cap = 65536usize;

    let (_state, retained, peak) = measure_peak(|| {
        let (instr_tx, _) = broadcast::channel::<EntryNodeInstruction>(cap);
        let (resp_tx, _) = broadcast::channel::<ExitNodeResponse>(cap);
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<u8>(64);
        // Subscribe one receiver per channel (the orchestrator does this)
        let _instr_rx = instr_tx.subscribe();
        let _resp_rx = resp_tx.subscribe();
        (instr_tx, resp_tx, ctrl_tx, ctrl_rx)
    });

    println!(
        "[memory]   capacity {cap}: retained {} / peak {}",
        fmt_bytes(retained),
        fmt_bytes(peak)
    );

    // Same at reduced capacity
    let cap_reduced = 1024usize;
    let (_state, retained_r, peak_r) = measure_peak(|| {
        let (instr_tx, _) = broadcast::channel::<EntryNodeInstruction>(cap_reduced);
        let (resp_tx, _) = broadcast::channel::<ExitNodeResponse>(cap_reduced);
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<u8>(64);
        let _instr_rx = instr_tx.subscribe();
        let _resp_rx = resp_tx.subscribe();
        (instr_tx, resp_tx, ctrl_tx, ctrl_rx)
    });

    println!(
        "[memory]   capacity {cap_reduced}: retained {} / peak {}",
        fmt_bytes(retained_r),
        fmt_bytes(peak_r)
    );

    let savings = retained.saturating_sub(retained_r);
    println!(
        "[memory]   savings from {cap} -> {cap_reduced}: {}",
        fmt_bytes(savings)
    );

    // A single connection at production capacity must stay under budget.
    assert!(
        retained < BUDGET_CONSTRAINED / 2,
        "Single connection at capacity {cap} costs {}, budget is {} (half constrained)",
        fmt_bytes(retained),
        fmt_bytes(BUDGET_CONSTRAINED / 2),
    );
}

/// Measure what it costs to fill a channel with default (empty) messages.
///
/// broadcast internally clones messages into a ring buffer.  Even "empty"
/// protobuf messages carry heap allocations from the Box<dyn> in oneof
/// wrappers.
#[test]
fn channel_filled_default() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === filled channel cost (default messages) ===");

    for &cap in &[1024_usize, 2048, 4096] {
        let ((_tx, _rx), retained, peak) = measure_peak(|| {
            let (tx, rx) = broadcast::channel::<ExitNodeResponse>(cap);
            for _ in 0..cap {
                tx.send(ExitNodeResponse::default()).unwrap();
            }
            (tx, rx)
        });

        println!(
            "[memory]   capacity {cap:>5} filled default: retained {} / peak {}",
            fmt_bytes(retained),
            fmt_bytes(peak),
        );
    }
}

/// Measure cost with realistic MTU-sized payloads.
///
/// This is the worst case: ring buffer full of 1400-byte UDP responses.
#[test]
fn channel_filled_mtu_payloads() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let payload_size = 1400; // typical MTU

    println!();
    println!("[memory] === filled channel cost ({payload_size}B payloads) ===");

    for &cap in &[1024_usize, 2048, 4096] {
        let ((_tx, _rx), retained, peak) = measure_peak(|| {
            let (tx, rx) = broadcast::channel::<ExitNodeResponse>(cap);
            for _ in 0..cap {
                tx.send(make_response_with_payload(payload_size)).unwrap();
            }
            (tx, rx)
        });

        println!(
            "[memory]   capacity {cap:>5} filled {payload_size}B: retained {} / peak {}",
            fmt_bytes(retained),
            fmt_bytes(peak),
        );

        // Hard budget: a full channel must not exceed the constrained target.
        assert!(
            retained < BUDGET_CONSTRAINED,
            "Channel({cap}) filled with {payload_size}B payloads costs {}, budget is {}",
            fmt_bytes(retained),
            fmt_bytes(BUDGET_CONSTRAINED),
        );
    }
}

/// Simulate a burst: fill the channel, then drain it, and measure peak.
///
/// This shows the high-water mark during a traffic spike — the number that
/// matters for OOM risk.
#[test]
fn burst_peak_memory() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let cap = 2048usize;
    let payload_size = 1400;

    println!();
    println!("[memory] === burst simulation (cap={cap}, {payload_size}B payloads) ===");

    let (_state, retained, peak) = measure_peak(|| {
        let (tx, _guard_rx) = broadcast::channel::<ExitNodeResponse>(cap);
        let mut rx = tx.subscribe();

        // Fill to capacity
        for _ in 0..cap {
            tx.send(make_response_with_payload(payload_size)).unwrap();
        }

        // Drain all
        for _ in 0..cap {
            let _ = rx.try_recv();
        }

        (tx, _guard_rx, rx)
    });

    println!(
        "[memory]   after fill+drain: retained {} / peak {}",
        fmt_bytes(retained),
        fmt_bytes(peak)
    );

    // After draining, retained should be close to the empty channel cost.
    // The ring buffer stays allocated but payload Bytes are freed.
    let (_empty_chan, empty_cost) = measure(|| broadcast::channel::<ExitNodeResponse>(cap));
    println!(
        "[memory]   empty channel baseline: {}",
        fmt_bytes(empty_cost)
    );
    println!(
        "[memory]   payload overhead after drain: {}",
        fmt_bytes(retained.saturating_sub(empty_cost))
    );
}

/// tokio mpsc channel costs (used for control messages, UDP responses, etc.)
#[test]
fn mpsc_channel_costs() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === tokio mpsc channel costs ===");

    for &cap in &[16_usize, 64, 256, 1024] {
        let ((_tx, _rx), cost) = measure(|| tokio::sync::mpsc::channel::<ExitNodeResponse>(cap));
        println!(
            "[memory]   mpsc<ExitNodeResponse>({cap:>4}): {}",
            fmt_bytes(cost)
        );
    }
}

/// Tokio runtime overhead — measured by creating a current-thread runtime.
///
/// A multi-thread runtime allocates more (worker threads, queues, etc.).
#[test]
fn tokio_runtime_overhead() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === tokio runtime overhead ===");

    let (_rt, cost_ct) = measure(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    });
    println!("[memory]   current_thread runtime: {}", fmt_bytes(cost_ct));

    let (_rt, cost_mt) = measure(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    });
    println!("[memory]   multi_thread(2) runtime: {}", fmt_bytes(cost_mt));
}

/// Composite: simulate an exit node's memory profile.
///
/// Exit node holds:
///   - 2 broadcast channels (instructions in, responses out)
///   - 1 mpsc control channel
///   - 1 subscriber each
///   - The orchestrator itself (no adapter — that needs real sockets)
///
/// This doesn't include tokio runtime, the adapter's DashMap, or QUIC
/// connection state, but it covers the per-connection channel memory that
/// we control.
#[test]
fn exit_node_connection_profile() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    println!();
    println!("[memory] === exit node connection profile ===");
    println!("[memory]   (channels + subscribers, no adapter/runtime)");

    let targets: &[(usize, &str)] = &[
        (512, "ultra-constrained"),
        (1024, "constrained (RPi/t4g.nano)"),
        (2048, "moderate"),
        (4096, "comfortable"),
        (65536, "current production"),
    ];

    for &(cap, label) in targets {
        let (_, cost) = measure(|| {
            // What QuicServer::accept or QuicClient::connect allocates
            let (instr_tx, _) = broadcast::channel::<EntryNodeInstruction>(cap);
            let (resp_tx, _) = broadcast::channel::<ExitNodeResponse>(cap);
            let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<u8>(64);
            let _instr_rx = instr_tx.subscribe();
            let _resp_rx = resp_tx.subscribe();
            (instr_tx, resp_tx, ctrl_tx, ctrl_rx, _instr_rx, _resp_rx)
        });

        let fits_constrained = if cost < BUDGET_CONSTRAINED {
            "OK"
        } else {
            "OVER"
        };
        let fits_moderate = if cost < BUDGET_MODERATE { "OK" } else { "OVER" };

        println!(
            "[memory]   cap={cap:<5} ({label:<30}): {:>10}  [constrained: {fits_constrained}, moderate: {fits_moderate}]",
            fmt_bytes(cost),
        );

        // Every configuration must fit the moderate budget.
        assert!(
            cost < BUDGET_MODERATE,
            "Exit node profile at cap={cap} costs {}, exceeds moderate budget {}",
            fmt_bytes(cost),
            fmt_bytes(BUDGET_MODERATE),
        );
    }
}

/// Summary: print a budget report for quick reference.
#[test]
fn zz_budget_report() {
    let _lock = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Collect key measurements
    let (_, cost_chan_1024) = measure(|| broadcast::channel::<ExitNodeResponse>(1024));
    let (_, cost_chan_65536) = measure(|| broadcast::channel::<ExitNodeResponse>(65536));
    let (_, cost_conn_1024) = measure(|| {
        let (a, _) = broadcast::channel::<EntryNodeInstruction>(1024);
        let (b, _) = broadcast::channel::<ExitNodeResponse>(1024);
        let _ = a.subscribe();
        let _ = b.subscribe();
        (a, b)
    });
    let (_, cost_conn_65536) = measure(|| {
        let (a, _) = broadcast::channel::<EntryNodeInstruction>(65536);
        let (b, _) = broadcast::channel::<ExitNodeResponse>(65536);
        let _ = a.subscribe();
        let _ = b.subscribe();
        (a, b)
    });

    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    MEMORY BUDGET REPORT                     ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Target hardware budgets:                                    ║");
    println!(
        "║   constrained (RPi Zero/t4g.nano):      {:>10}          ║",
        fmt_bytes(BUDGET_CONSTRAINED)
    );
    println!(
        "║   moderate    (RPi 4/small VPS):         {:>10}          ║",
        fmt_bytes(BUDGET_MODERATE)
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Per-connection channel cost (idle):                         ║");
    println!(
        "║   capacity  1024: {:>10}                                ║",
        fmt_bytes(cost_conn_1024)
    );
    println!(
        "║   capacity 65536: {:>10}                                ║",
        fmt_bytes(cost_conn_65536)
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Single broadcast channel (idle):                            ║");
    println!(
        "║   capacity  1024: {:>10}                                ║",
        fmt_bytes(cost_chan_1024)
    );
    println!(
        "║   capacity 65536: {:>10}                                ║",
        fmt_bytes(cost_chan_65536)
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Protobuf message sizes (stack):                             ║");
    println!(
        "║   EntryNodeInstruction: {:>4} B                             ║",
        std::mem::size_of::<EntryNodeInstruction>()
    );
    println!(
        "║   ExitNodeResponse:     {:>4} B                             ║",
        std::mem::size_of::<ExitNodeResponse>()
    );
    println!(
        "║   TunnelMessage:        {:>4} B                             ║",
        std::mem::size_of::<TunnelMessage>()
    );
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ NOTE: TCP data path (copy_bidirectional) uses ~16 KB per    ║");
    println!("║ connection regardless of channel capacity. Channel capacity ║");
    println!("║ only affects the instruction/response broadcast path.       ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
}
