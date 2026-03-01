#!/usr/bin/env python3
"""Release preparation tool with subcommands.

Subcommands:
  analyze           Parse commits (stdin) and emit a release decision as JSON.
  emit-outputs      Read the result JSON and print KEY=VALUE lines for
                    GITHUB_OUTPUT. Writes the changelog section to a file.
  update-changelog  Insert a changelog section into CHANGELOG.md.
  update-version    Rewrite the version in a Cargo.toml [package] section.

No git calls — all inputs come via CLI arguments, stdin, or files.
Logs to stderr for Actions visibility.
"""

from __future__ import annotations

import argparse
import json
import re
import sys

_MD_LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
from datetime import datetime, timezone
from pathlib import Path


def log(msg: str) -> None:
    print(msg, file=sys.stderr)


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

COMMIT_RE = re.compile(
    r"^(?P<type>[a-z]+)"
    r"(?:\((?P<scope>[^)]*)\))?"
    r"(?P<bang>!)?"
    r":\s*(?P<desc>.+)",
)


def parse_commits(raw: str) -> list[dict]:
    """Parse a JSON array of {sha, message} objects into structured commits."""
    import json as _json
    entries = _json.loads(raw)
    commits = []
    for entry in entries:
        sha = entry.get("sha", "")
        block = entry.get("message", "").strip()
        if not block:
            continue
        lines = block.splitlines()
        subject = lines[0].strip()
        body = "\n".join(lines[1:]) if len(lines) > 1 else ""
        has_breaking_footer = bool(
            re.search(r"^BREAKING[ -]CHANGE\s*:", body, re.MULTILINE)
        )
        m = COMMIT_RE.match(subject)
        if not m:
            if has_breaking_footer:
                commits.append(
                    {
                        "type": "breaking",
                        "bang": False,
                        "description": subject,
                        "breaking": True,
                        "sha": sha,
                    }
                )
            continue
        commits.append(
            {
                "type": m.group("type"),
                "scope": m.group("scope") or "",
                "bang": bool(m.group("bang")),
                "description": m.group("desc"),
                "breaking": bool(m.group("bang")) or has_breaking_footer,
                "sha": sha,
            }
        )
    return commits


def determine_bump(commits: list[dict]) -> str | None:
    # ci-, release-, and website-scoped commits don't warrant a CLI release.
    releasable = [c for c in commits if c.get("scope") not in ("ci", "release", "website")]
    if any(c["breaking"] for c in releasable):
        return "major"
    if any(c["type"] == "feat" for c in releasable):
        return "minor"
    if any(c["type"] in ("fix", "perf") for c in releasable):
        return "patch"
    return None


def parse_version(version: str) -> tuple[int, int, int]:
    parts = version.split(".")
    if len(parts) != 3:
        log(f"Invalid version: {version}")
        sys.exit(1)
    return int(parts[0]), int(parts[1]), int(parts[2])


def bump_version(version: str, bump: str) -> str:
    major, minor, patch = parse_version(version)
    if bump == "major":
        major += 1
        minor = 0
        patch = 0
    elif bump == "minor":
        minor += 1
        patch = 0
    elif bump == "patch":
        patch += 1
    return f"{major}.{minor}.{patch}"


def version_gt(a: str, b: str) -> bool:
    return parse_version(a) > parse_version(b)


def _sanitise_desc(desc: str) -> str:
    # Escape markdown links anywhere in the description
    desc = _MD_LINK.sub(lambda m: r"\[" + m.group(1) + r"\]" + m.group(0)[len(m.group(1))+2:], desc)
    # Escape # and - only at the start (prevents heading breakout and nested bullets)
    if desc.startswith("#"):
        desc = r"\#" + desc[1:]
    if desc.startswith("-"):
        desc = r"\-" + desc[1:]
    return desc


def format_entry(c: dict, repo_url: str) -> str:
    scope = c.get("scope", "")
    desc = _sanitise_desc(c["description"])
    text = f"**{scope}:** {desc}" if scope else desc
    sha = c.get("sha", "")
    if sha and repo_url:
        text += f" ([{sha[:7]}]({repo_url}/commit/{sha}))"
    return text


def _bucket_commits(commits: list[dict], repo_url: str) -> tuple[dict[str, list[str]], list[str]]:
    """Split commits into named sections and an 'other' list.

    Within each section, unscoped entries come first, then scoped entries
    sorted alphabetically by scope.
    """
    sections: dict[str, list[tuple[str, str]]] = {
        "Breaking Changes": [],
        "Features": [],
        "Bug Fixes": [],
    }
    other: list[str] = []
    for c in commits:
        entry = format_entry(c, repo_url)
        ctype = c["type"]
        scope = c.get("scope", "")
        if c["breaking"]:
            sections["Breaking Changes"].append((scope, entry))
        _infra_scope = scope in ("ci", "release", "website")
        if ctype == "feat" and not _infra_scope:
            sections["Features"].append((scope, entry))
        elif ctype in ("fix", "perf") and not _infra_scope:
            sections["Bug Fixes"].append((scope, entry))
        elif ctype != "chore":
            other.append(entry)
    # unscoped first, then scoped alphabetically
    sorted_sections: dict[str, list[str]] = {}
    for key, entries in sections.items():
        entries.sort(key=lambda x: (x[0] != "", x[0]))
        sorted_sections[key] = [e for _, e in entries]
    return sorted_sections, other


def _render_sections(sections: dict[str, list[str]]) -> list[str]:
    lines: list[str] = []
    for heading, items in sections.items():
        if not items:
            continue
        lines.append(f"### {heading}")
        lines.append("")
        for item in items:
            lines.append(f"* {item}")
        lines.append("")
    return lines


def generate_changelog(version: str, commits: list[dict], url: str = "", repo_url: str = "", date: str = "") -> str:
    """User-facing changelog: named sections + 'x other changes' count."""
    today = date or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    sections, other = _bucket_commits(commits, repo_url)
    header = f"## [{version}]({url}) ({today})" if url else f"## [{version}] ({today})"
    lines = [header, ""] + _render_sections(sections)
    if other:
        noun = "change" if len(other) == 1 else "changes"
        if url:
            lines.append(f"_{len(other)} other {noun} — [view diff]({url})_")
        else:
            lines.append(f"_{len(other)} other {noun}_")
        lines.append("")
    return "\n".join(lines)


def generate_pr_body(version: str, commits: list[dict], url: str = "", repo_url: str = "", date: str = "") -> str:
    """PR body: named sections + full 'Other Changes' list for reviewer context."""
    today = date or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    sections, other = _bucket_commits(commits, repo_url)
    header = f"## [{version}]({url}) ({today})" if url else f"## [{version}] ({today})"
    lines = [header, ""] + _render_sections(sections)
    if other:
        lines.append("### Other Changes")
        lines.append("")
        for item in other:
            lines.append(f"* {item}")
        lines.append("")
    return "\n".join(lines)


def read_current_version(version_file: str) -> str:
    """Read the version from the [package] section of a Cargo.toml."""
    in_package = False
    with open(version_file) as f:
        for line in f:
            stripped = line.strip()
            if stripped == "[package]":
                in_package = True
                continue
            if in_package and stripped.startswith("["):
                break
            if in_package:
                m = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
                if m:
                    return m.group(1)
    log(f"Could not find version in [package] of {version_file}")
    sys.exit(1)


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

def cmd_analyze(args: argparse.Namespace) -> None:
    """Parse commits and decide the release action."""
    latest_tag = args.latest_tag or None
    current_version = read_current_version(args.version_file)
    log(f"latest tag: {latest_tag or '(none)'}")
    log(f"version in {args.version_file}: {current_version}")

    if latest_tag:
        tag_version = latest_tag.removeprefix(args.tag_prefix)
        if version_gt(current_version, tag_version):
            log(f"file version {current_version} > tag version {tag_version} — needs tagging")
            result = {
                "action": "tag",
                "version": current_version,
                "tag": f"{args.tag_prefix}{current_version}",
            }
            Path(args.output).write_text(json.dumps(result))
            return

    messages = sys.stdin.read()
    if not messages.strip():
        log("no commits on stdin")
        Path(args.output).write_text(
            json.dumps({"action": "none", "reason": "no commits since last tag"})
        )
        return

    commits = parse_commits(messages)
    log(f"parsed {len(commits)} conventional commit(s)")
    if not commits:
        Path(args.output).write_text(
            json.dumps({"action": "none", "reason": "no conventional commits found"})
        )
        return

    bump = determine_bump(commits)
    if bump is None:
        log("no release-worthy commits (chore/docs/refactor only)")
        Path(args.output).write_text(
            json.dumps({"action": "none", "reason": "no release-worthy commits"})
        )
        return

    new_version = bump_version(current_version, bump)
    log(f"bump: {bump} ({current_version} -> {new_version})")
    if args.repo_url:
        prev = args.latest_tag or ""
        new_tag = f"{args.tag_prefix}{new_version}"
        url = (
            f"{args.repo_url}/compare/{prev}...{new_tag}"
            if prev
            else f"{args.repo_url}/releases/tag/{new_tag}"
        )
    else:
        url = ""
    changelog = generate_changelog(new_version, commits, url=url, repo_url=args.repo_url)
    pr_body = generate_pr_body(new_version, commits, url=url, repo_url=args.repo_url)

    result = {
        "action": "bump",
        "bump": bump,
        "current_version": current_version,
        "new_version": new_version,
        "tag": f"{args.tag_prefix}{new_version}",
        "changelog": changelog,
        "pr_body": pr_body,
        "version_file": args.version_file,
        "package": args.package,
    }
    Path(args.output).write_text(json.dumps(result))


def cmd_emit_outputs(args: argparse.Namespace) -> None:
    """Read result JSON and print KEY=VALUE lines for GITHUB_OUTPUT."""
    data = json.loads(Path(args.result_file).read_text())
    action = data["action"]
    print(f"action={action}")

    if action == "tag":
        print(f"tag={data['tag']}")
        print(f"version={data['version']}")
    elif action == "bump":
        for key in ("new_version", "package", "tag"):
            print(f"{key}={data[key]}")
        Path(args.changelog_file).write_text(data["changelog"])
        log(f"wrote changelog section to {args.changelog_file}")
        Path(args.pr_body_file).write_text(data["pr_body"])
        log(f"wrote PR body to {args.pr_body_file}")

    # Pretty-print the full result to stderr for debugging
    json.dump(data, sys.stderr, indent=2)
    print(file=sys.stderr)


def cmd_update_changelog(args: argparse.Namespace) -> None:
    """Insert a changelog section into CHANGELOG.md."""
    section = Path(args.section_file).read_text()
    cl = Path(args.changelog)

    if cl.exists():
        lines = cl.read_text().splitlines(True)
        out: list[str] = []
        inserted = False
        for line in lines:
            out.append(line)
            if not inserted and line.startswith("# "):
                out.append("\n")
                out.append(section)
                out.append("\n")
                inserted = True
        if not inserted:
            out.insert(0, section + "\n")
        cl.write_text("".join(out))
    else:
        cl.write_text(f"# Changelog\n\n{section}\n")

    log(f"updated {cl}")


def cmd_has_releasable(args: argparse.Namespace) -> None:
    """Read commit subjects (one per line) from stdin.

    Exit 0 if any are releasable (would trigger a version bump).
    Exit 1 if none are (chore/docs/refactor/ci/website only).
    """
    lines = [l.strip() for l in sys.stdin.read().splitlines() if l.strip()]
    log(f"has-releasable: checking {len(lines)} commit(s)")
    fake_json = json.dumps([{"sha": "", "message": line} for line in lines])
    commits = parse_commits(fake_json)
    bump = determine_bump(commits)
    log(f"has-releasable: bump={bump!r}")
    sys.exit(0 if bump is not None else 1)


def cmd_update_version(args: argparse.Namespace) -> None:
    """Rewrite the version in the [package] section of a Cargo.toml."""
    p = Path(args.version_file)
    lines = p.read_text().splitlines(True)
    in_package = False
    replaced = False
    out: list[str] = []
    for line in lines:
        stripped = line.strip()
        if stripped == "[package]":
            in_package = True
        elif in_package and stripped.startswith("["):
            in_package = False
        if in_package and not replaced and re.match(r'^version\s*=\s*"', stripped):
            out.append(f'version = "{args.version}"\n')
            replaced = True
        else:
            out.append(line)
    if not replaced:
        log(f"Could not find version in [package] of {args.version_file}")
        sys.exit(1)
    p.write_text("".join(out))
    log(f"updated {args.version_file} to {args.version}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    # analyze
    p_analyze = sub.add_parser("analyze", help="Decide the release action from commits on stdin")
    p_analyze.add_argument("--tag-prefix", required=True)
    p_analyze.add_argument("--version-file", required=True)
    p_analyze.add_argument("--package", required=True)
    p_analyze.add_argument("--latest-tag", default="")
    p_analyze.add_argument("--repo-url", default="", help="GitHub repo URL for changelog links")
    p_analyze.add_argument("--output", default="/tmp/prepare-result.json",
                           help="Path to write the result JSON")

    # emit-outputs
    p_emit = sub.add_parser("emit-outputs", help="Emit KEY=VALUE lines from result JSON")
    p_emit.add_argument("--result-file", default="/tmp/prepare-result.json")
    p_emit.add_argument("--changelog-file", default="/tmp/changelog_section.md")
    p_emit.add_argument("--pr-body-file", default="/tmp/pr_body_section.md")

    # update-changelog
    p_cl = sub.add_parser("update-changelog", help="Insert section into CHANGELOG.md")
    p_cl.add_argument("--section-file", default="/tmp/changelog_section.md")
    p_cl.add_argument("--changelog", default="CHANGELOG.md")

    # update-version
    p_ver = sub.add_parser("update-version", help="Set version in Cargo.toml [package]")
    p_ver.add_argument("--version-file", required=True)
    p_ver.add_argument("--version", required=True)

    # has-releasable
    sub.add_parser("has-releasable",
                   help="Read commit subjects from stdin (one per line); exit 0 if any are releasable")

    args = parser.parse_args()
    {
        "analyze": cmd_analyze,
        "emit-outputs": cmd_emit_outputs,
        "update-changelog": cmd_update_changelog,
        "update-version": cmd_update_version,
        "has-releasable": cmd_has_releasable,
    }[args.command](args)


if __name__ == "__main__":
    main()
