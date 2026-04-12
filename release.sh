#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="Cargo.toml"

if ! command -v gh &>/dev/null; then
    echo "Error: gh CLI not found. Install from https://cli.github.com/"
    exit 1
fi
if ! gh auth status &>/dev/null; then
    echo "Error: gh is not authenticated. Run 'gh auth login' first."
    exit 1
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Portable in-place sed (macOS vs GNU)
sedi() {
    if sed --version &>/dev/null; then
        sed -i "$@"    # GNU
    else
        sed -i '' "$@" # BSD
    fi
}

get_version() {
    grep '^version = ' "$CARGO_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/'
}

set_version() {
    local new="$1"
    sedi "s/^version = \".*\"/version = \"${new}\"/" "$CARGO_TOML"
    cargo update --workspace
    echo "Version set to $new"
}

# Strip any suffix (-dev, -beta.N, etc.) → pure semver
strip_suffix() {
    echo "${1%%-*}"
}

# Parse M.m.p from a (possibly suffixed) version
parse_major() { echo "${1%%.*}"; }
parse_minor() { local tmp="${1#*.}"; echo "${tmp%%.*}"; }
parse_patch() { local tmp="${1%%-*}"; echo "${tmp##*.}"; }

confirm() {
    local prompt="$1"
    read -r -p "$prompt [type 'yes' to continue]: " answer
    if [ "$answer" != "yes" ]; then
        echo "Aborted."
        exit 1
    fi
}

ensure_clean() {
    if [ -n "$(git status --porcelain)" ]; then
        echo "Error: working tree is not clean. Commit or stash changes first."
        exit 1
    fi
}

ensure_on_main() {
    local branch
    branch=$(git branch --show-current)
    if [ "$branch" != "main" ]; then
        echo "Error: must be on 'main' branch (currently on '$branch')."
        exit 1
    fi
}

check_changelog() {
    local version="$1"
    if [ -f CHANGELOG.md ] && grep -q "## \[Unreleased\]" CHANGELOG.md; then
        echo "Note: CHANGELOG.md [Unreleased] section exists."
        echo "      CI (git-cliff) will generate the [${version}] entry on merge."
    fi
}

# Create a PR branch, commit, push, open PR, wait for merge, then
# tag the merge commit on main.
create_release_pr() {
    local version="$1"
    local tag="v${version}"
    local branch="release-${version}"

    git checkout -b "$branch"
    git add "$CARGO_TOML" Cargo.lock
    git commit -m "chore(release): ${version}"
    git push -u origin "$branch"

    echo ""
    echo "Creating PR for ${version}..."
    local pr_url
    pr_url=$(gh pr create \
        --base main \
        --head "$branch" \
        --title "chore(release): ${version}" \
        --body "Release ${version}. Merge this PR, then the tag will be created automatically.")

    echo "PR created: $pr_url"

    # Enable auto-merge so the PR merges as soon as checks pass
    gh pr merge "$branch" --auto --squash || true

    echo ""
    echo "Waiting for PR to be merged..."

    # Poll until merged (check every 10s, timeout after 30min)
    local elapsed=0
    while [ $elapsed -lt 1800 ]; do
        local state
        state=$(gh pr view "$branch" --json state -q '.state' 2>/dev/null || echo "UNKNOWN")
        if [ "$state" = "MERGED" ]; then
            echo "PR merged!"
            break
        elif [ "$state" = "CLOSED" ]; then
            echo "Error: PR was closed without merging. Aborting."
            git checkout main
            git branch -D "$branch" 2>/dev/null || true
            exit 1
        fi
        sleep 10
        elapsed=$((elapsed + 10))
    done

    if [ $elapsed -ge 1800 ]; then
        echo "Timeout waiting for PR merge. Tag manually with:"
        echo "  merge_sha=\$(gh pr view \"$branch\" --json mergeCommit -q '.mergeCommit.oid')"
        echo "  git tag ${tag} \"\$merge_sha\" && git push origin ${tag}"
        git checkout main
        git branch -D "$branch" 2>/dev/null || true
        exit 1
    fi

    # Tag the exact merge commit, not whatever HEAD happens to be
    local merge_sha
    merge_sha=$(gh pr view "$branch" --json mergeCommit -q '.mergeCommit.oid')
    git checkout main
    git pull origin main
    git tag "$tag" "$merge_sha"
    git push origin "$tag"
    git branch -D "$branch" 2>/dev/null || true
    echo "Tagged and pushed ${tag}"
}

bump_to_dev() {
    local base="$1"
    local major minor patch
    major=$(parse_major "$base")
    minor=$(parse_minor "$base")
    patch=$(parse_patch "$base")
    local next_patch=$((patch + 1))
    local dev_version="${major}.${minor}.${next_patch}-dev"

    set_version "$dev_version"

    local branch="post-release-${dev_version}"
    git checkout -b "$branch"
    git add "$CARGO_TOML" Cargo.lock
    git commit -m "chore(release): begin ${dev_version}"
    git push -u origin "$branch"

    gh pr create \
        --base main \
        --head "$branch" \
        --title "chore(release): begin ${dev_version}" \
        --body "Bump version to ${dev_version} for next development cycle."

    git checkout main
    echo ""
    echo "Development version PR created for $dev_version."
    echo "Merge it to continue development."
}

# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

do_release() {
    ensure_clean
    ensure_on_main

    local current
    current=$(get_version)
    local release_version
    release_version=$(strip_suffix "$current")

    echo "Current version: $current"
    echo "Release version: $release_version"
    confirm "Create release $release_version?"

    check_changelog "$release_version"
    set_version "$release_version"
    create_release_pr "$release_version"

    echo ""
    bump_to_dev "$release_version"
}

do_beta() {
    ensure_clean
    ensure_on_main
    git fetch --tags --quiet

    local current
    current=$(get_version)
    local base
    base=$(strip_suffix "$current")

    # Find next beta number from existing tags (only numeric suffixes)
    local last_beta
    last_beta=$(git tag -l "v${base}-beta.*" \
        | sed "s/v${base}-beta\.//" \
        | awk '/^[0-9]+$/' \
        | sort -n | tail -1)
    local next_beta
    if [ -z "$last_beta" ]; then
        next_beta=1
    else
        next_beta=$((last_beta + 1))
    fi
    local beta_version="${base}-beta.${next_beta}"

    echo "Current version: $current"
    echo "Beta version:    $beta_version"
    confirm "Create beta $beta_version?"

    check_changelog "$beta_version"
    set_version "$beta_version"
    create_release_pr "$beta_version"

    echo ""
    set_version "${base}-dev"

    local dev_version="${base}-dev"
    local dev_branch="post-beta-${dev_version}"
    git checkout -b "$dev_branch"
    git add "$CARGO_TOML" Cargo.lock
    git commit -m "chore(release): resume ${dev_version}"
    git push -u origin "$dev_branch"

    gh pr create \
        --base main \
        --head "$dev_branch" \
        --title "chore(release): resume ${dev_version}" \
        --body "Return version to ${dev_version} after beta ${beta_version}."

    git checkout main
    echo ""
    echo "Development version PR created for $dev_version."
    echo "Merge it to continue development."
}

do_bump() {
    local part="$1"

    ensure_clean
    ensure_on_main

    local current
    current=$(get_version)
    local major minor patch
    major=$(parse_major "$current")
    minor=$(parse_minor "$current")
    patch=$(parse_patch "$current")

    case "$part" in
        major)
            major=$((major + 1))
            minor=0
            patch=0
            ;;
        minor)
            minor=$((minor + 1))
            patch=0
            ;;
        patch)
            patch=$((patch + 1))
            ;;
    esac

    local new_version="${major}.${minor}.${patch}-dev"
    echo "Current version: $current"
    echo "New version:     $new_version"
    confirm "Bump to $new_version?"

    set_version "$new_version"

    local branch="bump-${new_version}"
    git checkout -b "$branch"
    git add "$CARGO_TOML" Cargo.lock
    git commit -m "chore(release): bump to ${new_version}"
    git push -u origin "$branch"

    gh pr create \
        --base main \
        --head "$branch" \
        --title "chore(release): bump to ${new_version}" \
        --body "Bump version to ${new_version}."

    git checkout main
    echo ""
    echo "PR created for version bump to $new_version. Merge it to apply."
}

do_help() {
    local current
    current=$(get_version)
    cat <<EOF
release.sh — version management for mayara-server (PR-based flow)

Current version: $current

Commands:
  --release     Release the current version:
                  • strips any suffix (-dev, -beta.N)
                  • creates a PR, waits for merge, tags the result
                  • opens a follow-up PR to bump to next -dev version
                  (CHANGELOG.md is generated by git-cliff in CI)

  --beta        Create a beta pre-release:
                  • strips suffix, appends -beta.N (auto-incremented)
                  • creates a PR, waits for merge, tags the result
                  • opens a follow-up PR to return to <base>-dev

  --major       Bump major version (N+1.0.0-dev) via PR
  --minor       Bump minor version (x.N+1.0-dev) via PR
  --patch       Bump patch version (x.y.N+1-dev) via PR

Workflow:
  1. ./release.sh --minor       # PR: 3.4.2 → 3.5.0-dev
  2. (development happens)
  3. ./release.sh --release     # PR: 3.5.0, tag v3.5.0, PR: 3.5.1-dev
  4. ./release.sh --beta        # PR: 3.5.0-beta.1, tag, PR: 3.5.0-dev

Requires: gh CLI (authenticated)
EOF
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

case "${1:-}" in
    --release)  do_release ;;
    --beta)     do_beta ;;
    --major)    do_bump major ;;
    --minor)    do_bump minor ;;
    --patch)    do_bump patch ;;
    *)          do_help ;;
esac
