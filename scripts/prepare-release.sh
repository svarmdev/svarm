#!/bin/sh
set -eu

LEVEL=
EXECUTE=0

usage() {
    cat <<'EOF'
Prepare a workspace version bump and push a release PR branch.

Usage: prepare-release.sh <patch|minor|major|VERSION> [--execute]

Requires cargo-release (cargo install cargo-release --locked). Dry-run
unless --execute is passed. --execute creates chore/release-vX.Y.Z from
origin/main, commits the bump, pushes the branch, and opens a pull
request when gh is available.

Do not tag from this script. After the PR is merged, run tag-release.sh.
EOF
}

die() {
    printf 'prepare-release.sh: %s\n' "$*" >&2
    exit 1
}

valid_version() {
    printf '%s\n' "$1" |
        awk -F. 'NF == 3 && $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ { found = 1 } END { exit !found }'
}

package_version() {
    cargo pkgid -p svarm | sed 's/.*[#@]//'
}

require_clean_worktree() {
    [ -z "$(git status --porcelain)" ] || die "working tree is not clean"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        --execute)
            EXECUTE=1
            ;;
        --*)
            die "unknown option: $1"
            ;;
        *)
            [ -z "$LEVEL" ] || die "only one bump level or version may be supplied"
            LEVEL=$1
            ;;
    esac
    shift
done

[ -n "$LEVEL" ] || die "a bump level or version is required; see --help"
case "$LEVEL" in
    patch | minor | major) ;;
    *)
        valid_version "$LEVEL" ||
            die "argument must be patch, minor, major, or a three-part version such as 0.3.0"
        ;;
esac

ROOT=$(git rev-parse --show-toplevel) || die "not inside a git repository"
cd "$ROOT"

command -v cargo >/dev/null 2>&1 || die "cargo is required"
cargo release --version >/dev/null 2>&1 ||
    die "cargo-release is required; install with: cargo install cargo-release --locked"

require_clean_worktree
git fetch origin
git rev-parse --verify origin/main >/dev/null 2>&1 || die "origin/main does not exist"

if [ "$EXECUTE" -ne 1 ]; then
    cargo release "$LEVEL" --allow-branch '*'
    printf '\nDry run only. Re-run with --execute to create the branch, commit, and push the PR.\n'
    exit 0
fi

case "$LEVEL" in
    patch | minor | major) BRANCH=chore/release-new ;;
    *) BRANCH="chore/release-v$LEVEL" ;;
esac

if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    die "local branch $BRANCH already exists"
fi
if git ls-remote --exit-code --heads origin "$BRANCH" >/dev/null 2>&1; then
    die "origin already has $BRANCH"
fi

git checkout -b "$BRANCH" origin/main
cargo release "$LEVEL" --execute --no-confirm
require_clean_worktree

VERSION=$(package_version)
valid_version "$VERSION" || die "bumped package version is not a three-part version: $VERSION"
FINAL_BRANCH="chore/release-v$VERSION"
if [ "$BRANCH" != "$FINAL_BRANCH" ]; then
    if git show-ref --verify --quiet "refs/heads/$FINAL_BRANCH"; then
        die "local branch $FINAL_BRANCH already exists"
    fi
    if git ls-remote --exit-code --heads origin "$FINAL_BRANCH" >/dev/null 2>&1; then
        die "origin already has $FINAL_BRANCH"
    fi
    git branch -m "$FINAL_BRANCH"
    BRANCH=$FINAL_BRANCH
fi

git push -u origin HEAD
if command -v gh >/dev/null 2>&1; then
    gh pr create --title "chore: prepare release v$VERSION" --body "Bump workspace version to $VERSION."
else
    printf 'Pushed %s. Open a pull request for v%s.\n' "$BRANCH" "$VERSION"
fi
