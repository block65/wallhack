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
  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"
  git tag -f "$TAG" "$HEAD_SHA"
  git push origin "$TAG" --force-with-lease

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

  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"

  # Fetch the remote branch. Without this, a fresh CI checkout has no local
  # tracking ref and any push to an existing remote branch would be rejected.
  git fetch origin "$branch" 2>/dev/null || true

  local needs_rebuild=true
  if git show-ref --quiet "refs/remotes/origin/${branch}"; then
    local tip_msg
    tip_msg=$(git log -1 --format="%s" "refs/remotes/origin/${branch}")
    notice "existing branch tip: ${tip_msg}"
    if [ "$tip_msg" = "chore: release v${NEW_VERSION}" ]; then
      # The branch is already prepared for this version. Only rebuild if
      # new releasable commits have landed on main since the branch was cut.
      local branch_base new_subjects
      branch_base=$(git log -1 --format="%P" "refs/remotes/origin/${branch}")
      new_subjects=$(git log --format="%s" "${branch_base}..HEAD")
      notice "branch base: ${branch_base}"
      notice "commits since branch base:$(echo "$new_subjects" | sed 's/^/\n  /')"
      if ! echo "$new_subjects" | python3 "$SCRIPT" has-releasable; then
        notice "no new releasable commits since release branch was created — skipping rebuild"
        needs_rebuild=false
      else
        notice "new releasable commits found — rebuilding release branch"
      fi
    else
      notice "branch tip does not match expected release commit — rebuilding"
    fi
  else
    notice "no existing remote branch — creating fresh"
  fi

  if [ "$needs_rebuild" = true ]; then
    git checkout -B "$branch"

    python3 "$SCRIPT" update-version \
      --version-file "$VERSION_FILE" \
      --version "$NEW_VERSION"

    cargo update -p "$PACKAGE"

    python3 "$SCRIPT" update-changelog

    git add "$VERSION_FILE" Cargo.lock CHANGELOG.md
    git commit -m "chore: release v${NEW_VERSION}"
    git push --force origin "$branch"
  fi

  local existing_pr
  existing_pr=$(gh pr list --head "$branch" --json number \
    --jq 'if length > 0 then .[0].number | tostring else "" end')

  local pr_body
  pr_body="$(cat /tmp/pr_body_section.md)

---
Merging this PR will create tag \`${TAG}\` and trigger the binary build pipeline."

  if [ -n "$existing_pr" ]; then
    notice "updating PR #${existing_pr}"
    gh pr edit "$existing_pr" \
      --title "chore: release v${NEW_VERSION}" \
      --body "$pr_body"
  else
    notice "creating release PR"
    local base_branch
    base_branch=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name)
    gh pr create \
      --title "chore: release v${NEW_VERSION}" \
      --body "$pr_body" \
      --base "$base_branch" \
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
