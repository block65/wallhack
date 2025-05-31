---
applyTo: "**/*.rs"
---

- Use tabs! Visible as 2 spaces. 80 char width limit
- Write idiomatic 2024 edition code
- Prefer ("{var}") over ("{}", var)
- Use question mark operator over unwrap
- Use `tracing` crate with `tracing::` prefix
- Use `thiserror:error` crate for `Error` Enums
- Prefer span instrumentation over log prefixes
- Prefer `tracing::instrument` derive over spans
- clippy pedantic
