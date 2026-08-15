#!/bin/sh
set -eu

EXECUTE=0

usage() {
    cat <<'EOF'
Tag the current main commit after a version PR has been merged.

Usage: tag-release.sh [--execute]

Reads the workspace version from cargo pkgid, creates an annotated
vX.Y.Z tag on origin/main, and pushes only that tag. Dry-run unless
--execute is passed.

Run this on an up-to-date main after prepare-release.sh's PR is merged.
Pushing the tag starts the existing GitHub Release workflow.
EOF
}

die() {
    printf 'tag-release.sh: %s\n' "$*" >&2
    exit 1
}

valid_version() {
    printf '%s\n' "$1" |
        awk -F. 'NF == 3 && $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ { found = 1 } END { exit !found }'
}

package_version() {
    cargo pkgid -p svarm | sed 's/.*[#@]//'
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
            die "unexpected argument: $1"
            ;;
    esac
    shift
done

ROOT=$(git rev-parse --show-toplevel) || die "not inside a git repository"
cd "$ROOT"

command -v cargo >/dev/null 2>&1 || die "cargo is required"
[ -z "$(git status --porcelain)" ] || die "working tree is not clean"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
[ "$BRANCH" = main ] || die "must be run on main after the version PR is merged"

git fetch origin
git rev-parse --verify origin/main >/dev/null 2>&1 || die "origin/main does not exist"
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] ||
    die "main is not up to date with origin/main; pull first"

VERSION=$(package_version)
valid_version "$VERSION" || die "package version is not a three-part version: $VERSION"
TAG="v$VERSION"

if git show-ref --tags --verify --quiet "refs/tags/$TAG"; then
    die "local tag $TAG already exists"
fi
if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
    die "origin already has $TAG"
fi

if [ "$EXECUTE" -ne 1 ]; then
    printf 'Would tag %s as %s and push it to origin.\n' "$(git rev-parse --short HEAD)" "$TAG"
    printf 'Dry run only. Re-run with --execute after the version PR is merged.\n'
    exit 0
fi

git tag -a "$TAG" -m "Release $TAG"
git push origin "$TAG"
printf 'Pushed %s. The Release workflow will publish GitHub release assets.\n' "$TAG"
