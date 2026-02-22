# Remove Broken JSON Output Format

The `--json` / `--output json` flag produces literal garbage. Remove it until
the feature is properly implemented.

## Scope

`crates/cli/src/output.rs` and the CLI flag definition

---

## The Problem

`crates/cli/src/output.rs`:
```rust
OutputFormat::Json => {
    // let json_output = serde_json::to_string(&message)
    // 	.expect("Failed to serialize status message to JSON");
    println!("{{ json_output }}");
}
```

The implementation was started, commented out, and replaced with a placeholder
that prints the literal string `{ json_output }` to stdout. Any user or script
that passes `--json` gets this string instead of JSON. This is a broken,
user-visible bug.

---

## Fix

Remove the `OutputFormat::Json` variant, the `--json` CLI flag, and all code
paths that reference them. This is cleaner than shipping a stub.

When JSON output is properly implemented (requiring `serde::Serialize` on all
status message types), it can be re-added as a new feature.

**Steps:**
1. Delete `OutputFormat::Json` variant from the enum
2. Remove the `--json` or `--output json` flag from the CLI argument parser
3. Remove the `println!("{{ json_output }}")` arm and any surrounding match
   arm machinery
4. If `serde_json` is only used for this path, remove it from `cli/Cargo.toml`
   dev/build dependencies

## Acceptance criteria

- `just check` passes
- `wallhack --json` (or equivalent) produces a clear "unrecognized argument"
  error rather than silent garbage output
- No `{{ json_output }}` string literal exists in the codebase
