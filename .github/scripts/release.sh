#!/usr/bin/env bash
# release.sh — orchestration functions called by the Release workflow.
# Each function is invoked as: bash release.sh <function-name>
# All configuration comes from environment variables set in the workflow.
set -euo pipefail

notice() { echo "::notice::$1"; }

# --------------------------------------------------------------------------
# find-latest-tag
# Env: GH_TOKEN, REPO, TAG_PREFIX
# Output: latest=<tag> >> GITHUB_OUTPUT
# --------------------------------------------------------------------------
cmd_find_latest_tag() {
  local latest
  latest=$(gh api "repos/${REPO}/git/matching-refs/tags/${TAG_PREFIX}" \
    --jq '.[].ref' | sed 's|refs/tags/||' | sort -V | tail -1)
  echo "latest=${latest:-}" >> "$GITHUB_OUTPUT"
  notice "latest tag: ${latest:-(none)}"
}

# --------------------------------------------------------------------------
# check-tagged
# Env: GH_TOKEN, REPO, LATEST_TAG, HEAD_SHA
# Output: skip=true|false >> GITHUB_OUTPUT
# --------------------------------------------------------------------------
cmd_check_tagged() {
  if [ -z "${LATEST_TAG:-}" ]; then
    echo "skip=false" >> "$GITHUB_OUTPUT"
    return
  fi

  local tag_obj obj_type obj_sha
  tag_obj=$(gh api "repos/${REPO}/git/ref/tags/${LATEST_TAG}")
  obj_type=$(echo "$tag_obj" | jq -r '.object.type')
  obj_sha=$(echo "$tag_obj" | jq -r '.object.sha')

  # Annotated tags: dereference to the underlying commit
  if [ "$obj_type" = "tag" ]; then
    obj_sha=$(gh api "repos/${REPO}/git/tags/${obj_sha}" --jq '.object.sha')
  fi

  if [ "$obj_sha" = "$HEAD_SHA" ]; then
    echo "skip=true" >> "$GITHUB_OUTPUT"
    notice "HEAD is already tagged ${LATEST_TAG} — skipping"
  else
    echo "skip=false" >> "$GITHUB_OUTPUT"
  fi
}

# --------------------------------------------------------------------------
# fetch-commits
# Env: GH_TOKEN, REPO, LATEST_TAG
# Output: /tmp/commit_messages.txt
# --------------------------------------------------------------------------
cmd_fetch_commits() {
  if [ -n "${LATEST_TAG:-}" ]; then
    # Compare API: max 300 commits (sufficient for typical release cadence)
    gh api "repos/${REPO}/compare/${LATEST_TAG}...HEAD" \
      --jq '[.commits[] | {sha: .sha, message: .commit.message}]' \
      > /tmp/commits.json
  else
    # No prior tag — paginate all commits and flatten into one JSON array.
    gh api "repos/${REPO}/commits" --paginate \
      --jq '[.[] | {sha: .sha, message: .commit.message}]' \
      | jq -s 'flatten' \
      > /tmp/commits.json
  fi
  notice "fetched $(jq length /tmp/commits.json) commits"
}

# --------------------------------------------------------------------------
# create-release
# Env: GH_TOKEN, TAG, VERSION, HEAD_SHA, BUILD_WORKFLOW
# --------------------------------------------------------------------------
cmd_create_release() {
  notice "creating draft release ${TAG}"

  local notes_file escaped_version
  notes_file=$(mktemp)
  if [ -f CHANGELOG.md ]; then
    escaped_version="${VERSION//./\\.}"
    awk "/^## \[${escaped_version}\]/{found=1; next} /^## \[/{found=0} found" \
      CHANGELOG.md > "$notes_file"
  fi
  if [ ! -s "$notes_file" ]; then
    echo "Release ${TAG}" > "$notes_file"
  fi

  if [ -n "${LATEST_TAG:-}" ] && [ -n "${REPO_URL:-}" ]; then
    printf "\n\n**Full Changelog**: %s/compare/%s...%s\n" \
      "$REPO_URL" "$LATEST_TAG" "$TAG" >> "$notes_file"
  fi

  # Push the tag explicitly first — gh release create --draft does not create
  # the git ref, so workflow_dispatch --ref "$TAG" would fail without this.
  git config user.name "github-actions[bot]"
  git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
  git tag "$TAG" "$HEAD_SHA"
  git push origin "$TAG"

  gh release create "$TAG" \
    --title "$TAG" \
    --notes-file "$notes_file" \
    --draft

  notice "dispatching ${BUILD_WORKFLOW} for ${TAG}"
  gh workflow run "$BUILD_WORKFLOW" \
    --ref "$TAG" \
    -f tag_name="$TAG"
}

# --------------------------------------------------------------------------
# open-pr
# Env: GH_TOKEN, SCRIPT, VERSION_FILE, NEW_VERSION, PACKAGE, TAG
# --------------------------------------------------------------------------
cmd_open_pr() {
  local branch="release/v${NEW_VERSION}"
  notice "preparing release PR for v${NEW_VERSION}"

  git config user.name "github-actions[bot]"
  git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
  git checkout -B "$branch"

  python3 "$SCRIPT" update-version \
    --version-file "$VERSION_FILE" \
    --version "$NEW_VERSION"

  cargo update -p "$PACKAGE"

  python3 "$SCRIPT" update-changelog

  git add "$VERSION_FILE" Cargo.lock CHANGELOG.md
  git commit -m "chore: release v${NEW_VERSION}"
  git push --force-with-lease origin "$branch"

  local existing_pr
  existing_pr=$(gh pr list --head "$branch" --json number \
    --jq 'if length > 0 then .[0].number | tostring else "" end')

  local pr_body
  pr_body="$(cat /tmp/changelog_section.md)

---
Merging this PR will create tag \`${TAG}\` and trigger the binary build pipeline."

  if [ -n "$existing_pr" ]; then
    notice "updating PR #${existing_pr}"
    gh pr edit "$existing_pr" \
      --title "chore: release v${NEW_VERSION}" \
      --body "$pr_body"
  else
    notice "creating release PR"
    gh pr create \
      --title "chore: release v${NEW_VERSION}" \
      --body "$pr_body" \
      --base main \
      --head "$branch"
  fi
}

# --------------------------------------------------------------------------
# Dispatch
# --------------------------------------------------------------------------
cmd="${1:?usage: release.sh <command>}"
shift
fn="cmd_${cmd//-/_}"
declare -f "$fn" > /dev/null || { echo "Unknown command: $cmd" >&2; exit 1; }
"$fn" "$@"
