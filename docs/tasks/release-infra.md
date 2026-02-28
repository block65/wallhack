# Release Infrastructure Replacement (REJECTED DESIGN — see TASK.md)

> **This document describes an approach that was considered and rejected.**
> The implemented solution uses a custom Python script + Bash orchestration layer
> instead of cargo-dist. See `.github/scripts/` for the actual implementation.

## Problem

The current release-please setup is flaky:
- Crashes with "Cannot read properties of undefined (reading 'pullRequest')"
  when there are 0 in-scope candidates (i.e. when only internal crates change)
- Requires maintaining two JSON config files that leak internal crate structure
- Binary build workflow is hand-rolled and fragile

## Goal

Replace with **cargo-dist** (build/release/asset attachment) + a small custom
GitHub Action (conventional commit → version bump → tag). No third-party
versioning bot. No CHANGELOG.md.

## What to remove

- `.release-please-manifest.json`
- `release-please-config.json`
- The release-please step in `.github/workflows/release.yml`
- `.github/workflows/build-release.yml` (replaced by cargo-dist CI)

## What to add

### cargo-dist

- `dist-workspace.toml` — points at the CLI binary as the primary artifact
- Generates GitHub release with binary assets for all targets
- Handles checksums and installers
- Used by `uv`, `ruff`, `just`, `mise` — well-established

### Custom version bump action (~50 lines of bash)

On push to main:
1. Read commits since last tag
2. Determine bump: `feat:` → minor, `fix:` → patch, `feat!:` / `BREAKING CHANGE` → major
3. If no `feat:` or `fix:` found — do nothing (no crash, no noise)
4. Bump workspace version in `Cargo.toml` + `Cargo.lock`
5. Commit and push tag
6. Tag triggers cargo-dist

## Notes

- All crates share one workspace version (single source of truth)
- The CLI binary is the only release artifact — internal crates are not published
- No CHANGELOG.md
- Conventional commits drive version bumps exactly as they do today
- Zero extra manual steps vs current workflow
