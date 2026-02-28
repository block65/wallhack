#!/usr/bin/env python3
"""Tests for prepare-release.py using real commit messages from this repo."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path

# Import module from sibling file
spec = importlib.util.spec_from_file_location(
    "prepare_release", Path(__file__).parent / "prepare-release.py"
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

parse_commits = mod.parse_commits
determine_bump = mod.determine_bump
bump_version = mod.bump_version
version_gt = mod.version_gt
generate_changelog = mod.generate_changelog
read_current_version = mod.read_current_version


# Real commit messages from wallhack-cli-v0.2.9..HEAD
POST_V029 = (
    "feat(core): add Indeterminate as fourth NodeRole variant\x00"
    "fix: correct markdown link syntax in README\x00"
    "refactor(core): make ControlChannels and ConnectionParams idiomatic methods\x00"
    "refactor(core): add SocketAddrExt, From impls, and AsyncProto traits\x00"
    "feat(daemon): add relay reconnect on source peer disconnect\x00"
    "refactor(psk): replace free functions with HandshakeExt trait\x00"
)

# Commits between v0.2.8 and v0.2.9 (fix + feat mix)
V028_TO_V029 = (
    "feat(daemon): integrate handshake exchange and PSK validation\x00"
    "feat(core): wire handshake exchange into client/server transport\x00"
    "feat(core): add PSK proof, HMAC module, and rename bridge to protocol\x00"
    "feat(wire): replace ExitNodeHello with bidirectional Handshake proto\x00"
    "docs: add capability handshake and zero-config design specs\x00"
    "chore: update standards submodule\x00"
    "fix(website): update rollup and devalue to resolve vulnerabilities\x00"
)

# Only non-release commits
CHORE_ONLY = (
    "chore: update standards submodule\x00"
    "docs: tighten AI disclosure wording\x00"
    "refactor(psk): replace free functions with HandshakeExt trait\x00"
)

BREAKING_BANG = "feat(wire)!: replace ExitNodeHello with Handshake proto\x00"

BREAKING_FOOTER = (
    "feat(wire): replace ExitNodeHello with Handshake proto\n\n"
    "BREAKING CHANGE: ExitNodeHello is removed\x00"
)

FIX_ONLY = "fix: correct markdown link syntax in README\x00"

NONCONVENTIONAL_BREAKING = (
    "some random commit subject\n\n"
    "BREAKING CHANGE: removed the old API\x00"
)


def test_parse_feat_and_fix():
    commits = parse_commits(POST_V029)
    types = [c["type"] for c in commits]
    assert "feat" in types
    assert "fix" in types
    assert "refactor" in types
    assert len(commits) == 6


def test_bump_minor_on_feat():
    commits = parse_commits(POST_V029)
    assert determine_bump(commits) == "minor"


def test_bump_minor_on_feat_mix():
    commits = parse_commits(V028_TO_V029)
    assert determine_bump(commits) == "minor"


def test_no_bump_on_chore_only():
    commits = parse_commits(CHORE_ONLY)
    assert determine_bump(commits) is None


def test_bump_major_on_bang():
    commits = parse_commits(BREAKING_BANG)
    assert determine_bump(commits) == "major"
    assert commits[0]["breaking"] is True


def test_bump_major_on_footer():
    commits = parse_commits(BREAKING_FOOTER)
    assert determine_bump(commits) == "major"
    assert commits[0]["breaking"] is True


def test_bump_patch_on_fix():
    commits = parse_commits(FIX_ONLY)
    assert determine_bump(commits) == "patch"


def test_bump_version_patch():
    assert bump_version("0.2.9", "patch") == "0.2.10"


def test_bump_version_minor():
    assert bump_version("0.2.9", "minor") == "0.3.0"


def test_bump_version_major():
    assert bump_version("0.2.9", "major") == "1.0.0"


def test_bump_version_major_from_zero():
    assert bump_version("0.2.9", "major") == "1.0.0"


def test_changelog_has_sections():
    commits = parse_commits(POST_V029)
    cl = generate_changelog("0.3.0", commits)
    assert "## [0.3.0]" in cl
    assert "### Added" in cl
    assert "### Fixed" in cl
    assert "add Indeterminate as fourth NodeRole variant" in cl
    assert "correct markdown link syntax in README" in cl


def test_changelog_breaking():
    commits = parse_commits(BREAKING_BANG)
    cl = generate_changelog("1.0.0", commits)
    assert "### Breaking Changes" in cl
    assert "### Added" in cl


def test_nonconventional_breaking_footer():
    commits = parse_commits(NONCONVENTIONAL_BREAKING)
    assert len(commits) == 1
    assert commits[0]["breaking"] is True
    assert determine_bump(commits) == "major"


def test_version_gt():
    assert version_gt("0.3.0", "0.2.9") is True
    assert version_gt("0.2.9", "0.2.9") is False
    assert version_gt("0.2.8", "0.2.9") is False
    assert version_gt("1.0.0", "0.99.99") is True
    assert version_gt("0.2.10", "0.2.9") is True


def test_empty_input():
    commits = parse_commits("")
    assert commits == []
    assert determine_bump(commits) is None


def test_read_current_version_package_section():
    """Only reads version from [package], not [dependencies]."""
    cargo = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
    cargo.write(
        '[dependencies]\nfoo = { version = "9.9.9" }\n\n'
        '[package]\nname = "test"\nversion = "1.2.3"\n\n'
        '[dev-dependencies]\nbar = { version = "8.8.8" }\n'
    )
    cargo.close()
    assert read_current_version(cargo.name) == "1.2.3"
    Path(cargo.name).unlink()


def test_read_current_version_ignores_dep_version():
    """version in [dependencies] must not be returned."""
    cargo = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
    cargo.write(
        '[dependencies]\nversion = "9.9.9"\n\n'
        '[package]\nversion = "0.1.0"\n'
    )
    cargo.close()
    assert read_current_version(cargo.name) == "0.1.0"
    Path(cargo.name).unlink()


def test_update_version():
    """update-version rewrites only [package] version."""
    cargo = tempfile.NamedTemporaryFile(mode="w", suffix=".toml", delete=False)
    cargo.write(
        '[package]\nname = "test"\nversion = "0.2.9"\n\n'
        '[dependencies]\nfoo = { version = "1.0.0" }\n'
    )
    cargo.close()
    # Simulate the subcommand
    import argparse
    args = argparse.Namespace(version_file=cargo.name, version="0.3.0")
    mod.cmd_update_version(args)
    content = Path(cargo.name).read_text()
    assert 'version = "0.3.0"' in content
    assert 'version = "1.0.0"' in content  # dependency unchanged
    Path(cargo.name).unlink()


def test_update_changelog_new_file():
    """update-changelog creates CHANGELOG.md if missing."""
    with tempfile.TemporaryDirectory() as d:
        section = Path(d) / "section.md"
        section.write_text("## [1.0.0] - 2026-01-01\n\n### Added\n\n- foo\n")
        cl = Path(d) / "CHANGELOG.md"
        import argparse
        args = argparse.Namespace(section_file=str(section), changelog=str(cl))
        mod.cmd_update_changelog(args)
        content = cl.read_text()
        assert "# Changelog" in content
        assert "## [1.0.0]" in content


def test_update_changelog_existing():
    """update-changelog inserts after the title in an existing file."""
    with tempfile.TemporaryDirectory() as d:
        section = Path(d) / "section.md"
        section.write_text("## [2.0.0] - 2026-02-01\n\n### Added\n\n- bar\n")
        cl = Path(d) / "CHANGELOG.md"
        cl.write_text("# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Added\n\n- foo\n")
        import argparse
        args = argparse.Namespace(section_file=str(section), changelog=str(cl))
        mod.cmd_update_changelog(args)
        content = cl.read_text()
        # New section appears before old
        assert content.index("2.0.0") < content.index("1.0.0")


def test_special_chars_in_description():
    """Commit descriptions with shell metacharacters pass through unchanged."""
    msg = "feat: add $(echo hello) and `whoami` support\x00fix: expand $USER profile\x00"
    commits = parse_commits(msg)
    assert len(commits) == 2
    assert commits[0]["description"] == "add $(echo hello) and `whoami` support"
    assert commits[1]["description"] == "expand $USER profile"
    cl = generate_changelog("1.0.0", commits)
    assert "$(echo hello)" in cl
    assert "`whoami`" in cl


def test_emoji_in_description():
    """Emoji in commit descriptions survive parsing and changelog generation."""
    msg = (
        "feat: 🚀 add turbo mode\x00"
        "fix: 🐛 squash the null pointer bug\x00"
        "feat!: 💥 redesign API\x00"
    )
    commits = parse_commits(msg)
    assert len(commits) == 3
    assert commits[0]["description"] == "🚀 add turbo mode"
    assert commits[1]["description"] == "🐛 squash the null pointer bug"
    assert commits[2]["description"] == "💥 redesign API"
    assert commits[2]["breaking"] is True
    cl = generate_changelog("1.0.0", commits)
    assert "🚀 add turbo mode" in cl
    assert "🐛 squash the null pointer bug" in cl
    assert "💥 redesign API" in cl


if __name__ == "__main__":
    failures = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"  PASS  {name}")
            except Exception as e:
                print(f"  FAIL  {name}: {e}")
                failures += 1
    sys.exit(1 if failures else 0)
