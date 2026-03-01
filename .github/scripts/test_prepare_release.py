#!/usr/bin/env python3
"""Tests for prepare-release.py using real commit messages from this repo."""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import tempfile
import textwrap
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
generate_pr_body = mod.generate_pr_body
read_current_version = mod.read_current_version
cmd_has_releasable = mod.cmd_has_releasable


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

REPO_URL = "https://github.com/block65/wallhack"


def commits_json(*messages: str) -> str:
    """Build a JSON commits array with deterministic short SHAs for behavioral tests."""
    return json.dumps([
        {"sha": f"sha{i+1:04d}abc", "message": m}
        for i, m in enumerate(messages)
    ])


def sha_link(n: int, repo_url: str = REPO_URL) -> str:
    """Return a markdown commit link for the nth synthetic commit (1-based)."""
    full = f"sha{n:04d}abc"
    return f"([{full[:7]}]({repo_url}/commit/{full}))"


def _cl(raw: str, version: str = "0.3.0", compare_url: str = "", date: str = "2026-03-01") -> str:
    commits = parse_commits(raw)
    url = compare_url or f"{REPO_URL}/compare/wallhack-cli-v{version}~1...wallhack-cli-v{version}"
    return generate_changelog(version, commits, url=url, repo_url=REPO_URL, date=date)


# ---------------------------------------------------------------------------
# Real release fixtures — actual commits from the wallhack repo
# ---------------------------------------------------------------------------

# Commits between wallhack-cli-v0.2.9 and wallhack-cli-v0.3.0
# (chore: release commit excluded — same as release.sh behaviour)
V029_TO_V030_COMMITS = json.dumps([
    {"sha": "1b7d18776287361bc518f9a64f3a1b0a256f8ff8", "message": "fix(ci): push git tag before draft release, add changelog comparison links"},
    {"sha": "8cb108eff4ba306cf43a526aa633439b472be88d", "message": "ci: replace release-please with custom release pipeline"},
    {"sha": "22bcc63757145f64ddd5735960b7b3292691accd", "message": "feat(core): add Indeterminate as fourth NodeRole variant"},
    {"sha": "d68b976b5b80051340acf2a7ece6653736fe3fef", "message": "fix: correct markdown link syntax in README"},
    {"sha": "9c32aadaababd6f5f36beb422f4de0f438e5c205", "message": "refactor(core): make ControlChannels and ConnectionParams idiomatic methods"},
    {"sha": "a5be110e2f73d0d19d2ed9d2d3bf94956e69b522", "message": "refactor(core): add SocketAddrExt, From impls, and AsyncProto traits"},
    {"sha": "fd9381a791df209fc5cf07f3f5fde9dcb0c6369c", "message": "feat(daemon): add relay reconnect on source peer disconnect"},
    {"sha": "4635ca134b9a8450feb7d3e0c8c7113a385a2d77", "message": "refactor(psk): replace free functions with HandshakeExt trait"},
])

V030_COMPARE_URL = f"{REPO_URL}/compare/wallhack-cli-v0.2.9...wallhack-cli-v0.3.0"
V030_RELEASE_DATE = "2026-02-28"

# Commits between wallhack-cli-v0.2.8 and wallhack-cli-v0.2.9
V028_TO_V029 = commits_json(
    "feat(daemon): integrate handshake exchange and PSK validation",
    "feat(core): wire handshake exchange into client/server transport",
    "feat(core): add PSK proof, HMAC module, and rename bridge to protocol",
    "feat(wire): replace ExitNodeHello with bidirectional Handshake proto",
    "docs: add capability handshake and zero-config design specs",
    "chore: update standards submodule",
    "fix(website): update rollup and devalue to resolve vulnerabilities",
)

CHORE_ONLY = commits_json(
    "chore: update standards submodule",
    "docs: tighten AI disclosure wording",
    "refactor(psk): replace free functions with HandshakeExt trait",
)

BREAKING_BANG = commits_json("feat(wire)!: replace ExitNodeHello with Handshake proto")

BREAKING_FOOTER = commits_json(
    "feat(wire): replace ExitNodeHello with Handshake proto\n\nBREAKING CHANGE: ExitNodeHello is removed"
)

FIX_ONLY = commits_json("fix: correct markdown link syntax in README")

NONCONVENTIONAL_BREAKING = commits_json(
    "some random commit subject\n\nBREAKING CHANGE: removed the old API"
)


# ---------------------------------------------------------------------------
# Bump logic
# ---------------------------------------------------------------------------

def test_parse_feat_and_fix():
    commits = parse_commits(V029_TO_V030_COMMITS)
    types = [c["type"] for c in commits]
    assert "feat" in types
    assert "fix" in types
    assert "refactor" in types
    assert len(commits) == 8


def test_bump_minor_on_feat():
    commits = parse_commits(V029_TO_V030_COMMITS)
    assert determine_bump(commits) == "minor"


def test_bump_minor_on_feat_mix():
    commits = parse_commits(V028_TO_V029)
    assert determine_bump(commits) == "minor"


def test_no_bump_on_chore_only():
    commits = parse_commits(CHORE_ONLY)
    assert determine_bump(commits) is None


def test_no_bump_on_infra_scopes_only():
    """ci-, release-, and website-scoped commits alone don't trigger a release."""
    commits = parse_commits(commits_json(
        "fix(ci): correct workflow syntax",
        "fix(release): make open-pr idempotent",
        "feat(website): add dark mode",
    ))
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
    commits = parse_commits("[]")
    assert commits == []
    assert determine_bump(commits) is None


# ---------------------------------------------------------------------------
# Changelog — real release fixture
# ---------------------------------------------------------------------------

def test_changelog_real_v030_release():
    """Full changelog output for the actual v0.3.0 release."""
    commits = parse_commits(V029_TO_V030_COMMITS)
    cl = generate_changelog(
        "0.3.0", commits,
        url=V030_COMPARE_URL,
        repo_url=REPO_URL,
        date=V030_RELEASE_DATE,
    )
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({V030_COMPARE_URL}) ({V030_RELEASE_DATE})

        ### Features

        * **core:** add Indeterminate as fourth NodeRole variant ([22bcc63]({REPO_URL}/commit/22bcc63757145f64ddd5735960b7b3292691accd))
        * **daemon:** add relay reconnect on source peer disconnect ([fd9381a]({REPO_URL}/commit/fd9381a791df209fc5cf07f3f5fde9dcb0c6369c))

        ### Bug Fixes

        * correct markdown link syntax in README ([d68b976]({REPO_URL}/commit/d68b976b5b80051340acf2a7ece6653736fe3fef))

        5 other changes — [view diff]({V030_COMPARE_URL})
    """)


# ---------------------------------------------------------------------------
# Changelog — behavioral tests (synthetic commits)
# ---------------------------------------------------------------------------

def test_changelog_feat_and_fix():
    cl = _cl(commits_json("feat: add turbo mode", "fix: correct null pointer"))
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0) (2026-03-01)

        ### Features

        * add turbo mode {sha_link(1)}

        ### Bug Fixes

        * correct null pointer {sha_link(2)}
    """)


def test_changelog_breaking():
    cl = _cl(BREAKING_BANG, version="1.0.0")
    assert cl == textwrap.dedent(f"""\
        ## [1.0.0]({REPO_URL}/compare/wallhack-cli-v1.0.0~1...wallhack-cli-v1.0.0) (2026-03-01)

        ### Breaking Changes

        * **wire:** replace ExitNodeHello with Handshake proto {sha_link(1)}

        ### Features

        * **wire:** replace ExitNodeHello with Handshake proto {sha_link(1)}
    """)


def test_changelog_chore_silently_dropped():
    cl = _cl(commits_json("chore: update standards submodule"))
    assert cl == f"## [0.3.0]({REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0) (2026-03-01)\n"


def test_changelog_infra_scopes_not_in_sections():
    """ci- and website-scoped commits are counted as other, not shown in sections."""
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    cl = _cl(commits_json(
        "ci: add release workflow",
        "fix(ci): correct matrix syntax",
        "fix(website): update rollup",
        "feat(website): add dark mode",
    ))
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({compare}) (2026-03-01)

        4 other changes — [view diff]({compare})
    """)


def test_changelog_other_count_singular():
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    cl = _cl(commits_json("docs: update README"))
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({compare}) (2026-03-01)

        1 other change — [view diff]({compare})
    """)


def test_changelog_other_count_plural():
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    cl = _cl(commits_json("docs: update README", "refactor: simplify parser"))
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({compare}) (2026-03-01)

        2 other changes — [view diff]({compare})
    """)


def test_changelog_section_order():
    """Sections appear in order: Breaking Changes, Features, Bug Fixes."""
    cl = _cl(commits_json("fix: correct thing", "feat: add thing", "feat!: redesign API"), version="1.0.0")
    assert cl == textwrap.dedent(f"""\
        ## [1.0.0]({REPO_URL}/compare/wallhack-cli-v1.0.0~1...wallhack-cli-v1.0.0) (2026-03-01)

        ### Breaking Changes

        * redesign API {sha_link(3)}

        ### Features

        * add thing {sha_link(2)}
        * redesign API {sha_link(3)}

        ### Bug Fixes

        * correct thing {sha_link(1)}
    """)


def test_changelog_mixed_sections_and_other():
    """feat + fix + docs + chore: docs counted as other, chore dropped."""
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    cl = _cl(commits_json("feat: add thing", "fix: correct thing", "docs: update README", "chore: bump deps"))
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({compare}) (2026-03-01)

        ### Features

        * add thing {sha_link(1)}

        ### Bug Fixes

        * correct thing {sha_link(2)}

        1 other change — [view diff]({compare})
    """)


def test_pr_body_real_v030_release():
    """PR body for v0.3.0 lists other commits in full instead of a count."""
    commits = parse_commits(V029_TO_V030_COMMITS)
    body = generate_pr_body(
        "0.3.0", commits,
        url=V030_COMPARE_URL,
        repo_url=REPO_URL,
        date=V030_RELEASE_DATE,
    )
    assert body == textwrap.dedent(f"""\
        ## [0.3.0]({V030_COMPARE_URL}) ({V030_RELEASE_DATE})

        ### Features

        * **core:** add Indeterminate as fourth NodeRole variant ([22bcc63]({REPO_URL}/commit/22bcc63757145f64ddd5735960b7b3292691accd))
        * **daemon:** add relay reconnect on source peer disconnect ([fd9381a]({REPO_URL}/commit/fd9381a791df209fc5cf07f3f5fde9dcb0c6369c))

        ### Bug Fixes

        * correct markdown link syntax in README ([d68b976]({REPO_URL}/commit/d68b976b5b80051340acf2a7ece6653736fe3fef))

        ### Other Changes

        * **ci:** push git tag before draft release, add changelog comparison links ([1b7d187]({REPO_URL}/commit/1b7d18776287361bc518f9a64f3a1b0a256f8ff8))
        * replace release-please with custom release pipeline ([8cb108e]({REPO_URL}/commit/8cb108eff4ba306cf43a526aa633439b472be88d))
        * **core:** make ControlChannels and ConnectionParams idiomatic methods ([9c32aad]({REPO_URL}/commit/9c32aadaababd6f5f36beb422f4de0f438e5c205))
        * **core:** add SocketAddrExt, From impls, and AsyncProto traits ([a5be110]({REPO_URL}/commit/a5be110e2f73d0d19d2ed9d2d3bf94956e69b522))
        * **psk:** replace free functions with HandshakeExt trait ([4635ca1]({REPO_URL}/commit/4635ca134b9a8450feb7d3e0c8c7113a385a2d77))
    """)


def test_pr_body_vs_changelog_other_section():
    """Changelog shows count; PR body shows full list."""
    raw = commits_json("feat: add thing", "fix(ci): infra fix", "refactor: tidy up")
    commits = parse_commits(raw)
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    cl = generate_changelog("0.3.0", commits, url=compare, repo_url=REPO_URL, date="2026-03-01")
    body = generate_pr_body("0.3.0", commits, url=compare, repo_url=REPO_URL, date="2026-03-01")
    assert "2 other changes" in cl
    assert "### Other Changes" in body
    assert "**ci:** infra fix" in body
    assert "tidy up" in body
    assert "### Other Changes" not in cl


def test_changelog_entries_grouped_by_scope():
    """Within each section, entries are sorted by scope (unscoped last)."""
    cl = _cl(commits_json(
        "fix: unscoped fix",
        "fix(wire): wire fix",
        "fix(core): core fix",
        "feat(daemon): daemon feat",
        "feat: unscoped feat",
        "feat(core): core feat",
    ))
    compare = f"{REPO_URL}/compare/wallhack-cli-v0.3.0~1...wallhack-cli-v0.3.0"
    assert cl == textwrap.dedent(f"""\
        ## [0.3.0]({compare}) (2026-03-01)

        ### Features

        * unscoped feat {sha_link(5)}
        * **core:** core feat {sha_link(6)}
        * **daemon:** daemon feat {sha_link(4)}

        ### Bug Fixes

        * unscoped fix {sha_link(1)}
        * **core:** core fix {sha_link(3)}
        * **wire:** wire fix {sha_link(2)}
    """)


def test_markdown_sanitisation_in_description():
    """Descriptions are sanitised to prevent markdown structure breakage.

    - [text](url) anywhere → escaped (renders as literal text, not a link)
    - # or ## at start → escaped (prevents heading breakout from bullet)
    - - at start → escaped (prevents nested bullet double-indent)
    - **bold**, _italic_, `code`, * mid-sentence → pass through unchanged
    """
    raw = commits_json(
        "feat: sneaky [link](https://example.com) mid-sentence",
        "feat: ## heading attempt",
        "fix: - hyphen at start",
        "fix(math): 1 * 3 = 99 this is now **correct**",
    )
    cl = _cl(raw)
    # links escaped
    assert r"\[link\](https://example.com)" in cl
    # heading escaped
    assert r"\## heading attempt" in cl
    # leading hyphen escaped
    assert r"\- hyphen at start" in cl
    # intentional formatting preserved
    assert "1 * 3 = 99 this is now **correct**" in cl


def test_special_chars_in_description():
    """Commit descriptions with shell metacharacters pass through unchanged."""
    raw = commits_json(
        "feat: add $(echo hello) and `whoami` support",
        "fix: expand $USER profile",
    )
    commits = parse_commits(raw)
    assert commits[0]["description"] == "add $(echo hello) and `whoami` support"
    assert commits[1]["description"] == "expand $USER profile"
    cl = _cl(raw)
    assert "$(echo hello)" in cl
    assert "`whoami`" in cl


def test_emoji_in_description():
    """Emoji in commit descriptions survive parsing and changelog generation."""
    raw = commits_json(
        "feat: 🚀 add turbo mode",
        "fix: 🐛 squash the null pointer bug",
        "feat!: 💥 redesign API",
    )
    commits = parse_commits(raw)
    assert commits[0]["description"] == "🚀 add turbo mode"
    assert commits[1]["description"] == "🐛 squash the null pointer bug"
    assert commits[2]["description"] == "💥 redesign API"
    assert commits[2]["breaking"] is True
    cl = _cl(raw, version="1.0.0")
    assert "🚀 add turbo mode" in cl
    assert "🐛 squash the null pointer bug" in cl
    assert "💥 redesign API" in cl


# ---------------------------------------------------------------------------
# Version file helpers
# ---------------------------------------------------------------------------

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
    args = argparse.Namespace(version_file=cargo.name, version="0.3.0")
    mod.cmd_update_version(args)
    content = Path(cargo.name).read_text()
    assert 'version = "0.3.0"' in content
    assert 'version = "1.0.0"' in content  # dependency unchanged
    Path(cargo.name).unlink()


def test_has_releasable_with_feat():
    import io
    try:
        sys.stdin = io.StringIO("feat: add turbo mode\nchore: bump deps\n")
        cmd_has_releasable(argparse.Namespace())
        assert False, "expected SystemExit(0)"
    except SystemExit as e:
        assert e.code == 0
    finally:
        sys.stdin = sys.__stdin__


def test_has_releasable_chore_only():
    import io
    try:
        sys.stdin = io.StringIO("chore: update standards submodule\ndocs: tighten wording\n")
        cmd_has_releasable(argparse.Namespace())
        assert False, "expected SystemExit(1)"
    except SystemExit as e:
        assert e.code == 1
    finally:
        sys.stdin = sys.__stdin__


def test_has_releasable_infra_scopes_only():
    import io
    try:
        sys.stdin = io.StringIO("fix(ci): correct workflow\nfeat(website): dark mode\n")
        cmd_has_releasable(argparse.Namespace())
        assert False, "expected SystemExit(1)"
    except SystemExit as e:
        assert e.code == 1
    finally:
        sys.stdin = sys.__stdin__


def test_update_changelog_new_file():
    """update-changelog creates CHANGELOG.md if missing."""
    with tempfile.TemporaryDirectory() as d:
        section = Path(d) / "section.md"
        section.write_text("## [1.0.0] - 2026-01-01\n\n### Added\n\n- foo\n")
        cl = Path(d) / "CHANGELOG.md"
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
        args = argparse.Namespace(section_file=str(section), changelog=str(cl))
        mod.cmd_update_changelog(args)
        content = cl.read_text()
        assert content.index("2.0.0") < content.index("1.0.0")


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------

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
