//! Version reporting.
//!
//! TODO(future): expose as structured data (typed semver + build metadata fields)
//! for JSON/MCP consumers that need to compare or display versions programmatically.

/// Return the canonical version string in semver+build-metadata format.
///
/// Format: `1.2.3+abc1234.20260316T123456.release`
/// With dirty working tree: `1.2.3+abc1234-dirty.20260316T123456.release`
#[must_use]
pub fn version() -> String {
    let sha = &env!("VERGEN_GIT_SHA")[..7];
    let dirty = if env!("VERGEN_GIT_DIRTY") == "true" {
        "-dirty"
    } else {
        ""
    };
    let ts = env!("VERGEN_BUILD_TIMESTAMP");
    // Compact ISO timestamp: 20260316T123456
    let compact_ts = ts.get(..19).unwrap_or(ts).replace('-', "").replace(':', "");
    let profile = env!("WALLHACK_BUILD_PROFILE");
    format!(
        "{}+{}{}.{}.{}",
        env!("CARGO_PKG_VERSION"),
        sha,
        dirty,
        compact_ts,
        profile,
    )
}
