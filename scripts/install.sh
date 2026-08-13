#!/bin/sh
set -eu

REPOSITORY="svarmdev/svarm"
RELEASE_API="https://api.github.com/repos/$REPOSITORY/releases/latest"
RELEASE_BASE="https://github.com/$REPOSITORY/releases/download"

if [ -n "${HOME:-}" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    INSTALL_DIR=
fi
REQUESTED_TAG=
YES=0

usage() {
    cat <<'EOF'
Install Svarm from a GitHub release.

Usage: install.sh [TAG] [--dir PATH] [--yes]

TAG          Release tag such as v0.1.0 (latest by default)
--dir PATH   Installation directory (default: ~/.local/bin)
--yes        Replace an existing binary without prompting
EOF
}

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

download() {
    url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 3 --silent --show-error -A svarm -o "$destination" "$url" ||
            die "could not download $url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --tries=3 --user-agent=svarm -O "$destination" "$url" ||
            die "could not download $url"
    else
        die "curl or wget is required"
    fi
}

checksum() {
    file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        die "sha256sum or shasum is required for checksum verification"
    fi
}

valid_version() {
    printf '%s\n' "$1" |
        awk -F. 'NF == 3 && $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ { found = 1 } END { exit !found }'
}

confirm_replace() {
    if [ ! -r /dev/tty ]; then
        die "${INSTALL_BINARY} already exists; rerun with --yes to replace it"
    fi
    printf 'Replace %s? [y/N] ' "$INSTALL_BINARY" > /dev/tty
    answer=
    IFS= read -r answer < /dev/tty || true
    [ "$answer" = y ] || [ "$answer" = Y ]
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --yes)
            YES=1
            ;;
        --dir)
            shift
            [ "$#" -gt 0 ] || die "--dir requires a path"
            INSTALL_DIR=$1
            ;;
        --dir=*)
            INSTALL_DIR=${1#--dir=}
            [ -n "$INSTALL_DIR" ] || die "--dir requires a path"
            ;;
        --*)
            die "unknown option: $1"
            ;;
        *)
            [ -z "$REQUESTED_TAG" ] || die "only one release tag may be supplied"
            REQUESTED_TAG=$1
            ;;
    esac
    shift
done

[ -n "$INSTALL_DIR" ] || die 'HOME is not set; pass --dir PATH'
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v install >/dev/null 2>&1 || die "install is required"
mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
INSTALL_DIR=$(cd "$INSTALL_DIR" && pwd -P)
INSTALL_BINARY=$INSTALL_DIR/svarm

case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64) TARGET=x86_64-unknown-linux-gnu ;;
    Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu ;;
    Darwin:x86_64|Darwin:amd64) TARGET=x86_64-apple-darwin ;;
    Darwin:arm64|Darwin:aarch64) TARGET=aarch64-apple-darwin ;;
    *) die "unsupported platform; supported targets are Linux x86_64/ARM64 and macOS Intel/Apple Silicon" ;;
esac

TEMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/svarm-install.XXXXXXXX") ||
    die "could not create a temporary directory"
trap 'rm -rf "$TEMP_DIR"' EXIT HUP INT TERM
umask 077

if [ -n "$REQUESTED_TAG" ]; then
    case "$REQUESTED_TAG" in
        v*) TAG=$REQUESTED_TAG; VERSION=${REQUESTED_TAG#v} ;;
        *) TAG="v$REQUESTED_TAG"; VERSION=$REQUESTED_TAG ;;
    esac
    valid_version "$VERSION" || die "release tag must be a three-part version, for example v0.1.0"
else
    download "$RELEASE_API" "$TEMP_DIR/release.json"
    TAG=$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$TEMP_DIR/release.json" | head -n 1)
    [ -n "$TAG" ] || die "GitHub did not return a latest release tag"
    case "$TAG" in
        v*) VERSION=${TAG#v} ;;
        *) die "GitHub returned an invalid release tag: $TAG" ;;
    esac
fi

ARCHIVE="svarm-${VERSION}-${TARGET}.tar.gz"
download "$RELEASE_BASE/$TAG/$ARCHIVE" "$TEMP_DIR/$ARCHIVE"
download "$RELEASE_BASE/$TAG/SHA256SUMS" "$TEMP_DIR/SHA256SUMS"

EXPECTED=$(awk -v file="$ARCHIVE" '$2 == file || $2 == "*" file { print $1; exit }' "$TEMP_DIR/SHA256SUMS")
case "$EXPECTED" in
    ''|*[![:xdigit:]]*) die "SHA256SUMS does not contain a checksum for $ARCHIVE" ;;
esac
[ "${#EXPECTED}" -eq 64 ] || die "SHA256SUMS does not contain a checksum for $ARCHIVE"
ACTUAL=$(checksum "$TEMP_DIR/$ARCHIVE")
[ "$ACTUAL" = "$EXPECTED" ] || die "checksum verification failed for $ARCHIVE"

mkdir "$TEMP_DIR/extracted"
tar -xzf "$TEMP_DIR/$ARCHIVE" -C "$TEMP_DIR/extracted" || die "could not extract $ARCHIVE"
[ -f "$TEMP_DIR/extracted/svarm" ] || die "release archive does not contain svarm"

if [ -e "$INSTALL_BINARY" ] && [ "$YES" -ne 1 ]; then
    confirm_replace || {
        printf 'Installation cancelled.\n'
        exit 0
    }
fi
install -m 0755 "$TEMP_DIR/extracted/svarm" "$INSTALL_BINARY" ||
    die "could not install $INSTALL_BINARY"
printf 'Installed Svarm %s at %s.\n' "$VERSION" "$INSTALL_BINARY"

case ":${PATH:-}:" in
    *:"$INSTALL_DIR":*) ;;
    *) printf 'Add %s to PATH to run svarm.\n' "$INSTALL_DIR" ;;
esac
