#!/bin/sh
# gflog installer — transparent, consent-gated. Shows exactly what it will do,
# then asks before touching your system. Override: GFLOG_VERSION, GFLOG_INSTALL_DIR,
# GFLOG_YES=1 (skip the prompt, for automation).
set -eu

REPO="MehmetSecgin/gflog"
BIN="gflog"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || err "missing required tool: $1"; }

need curl
need tar
need shasum

os=$(uname -s)
arch=$(uname -m)
[ "$os" = "Darwin" ] || err "this installer is macOS-only (found $os). On other systems: cargo install --git https://github.com/$REPO"
[ "$arch" = "arm64" ] || err "this prebuilt binary is Apple Silicon (arm64) only (found $arch). Build from source: cargo install --git https://github.com/$REPO"
target="aarch64-apple-darwin"

tag="${GFLOG_VERSION:-}"
if [ -z "$tag" ]; then
    tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
fi
[ -n "$tag" ] || err "could not resolve the latest release tag (set GFLOG_VERSION to pin one)"

asset="$BIN-$tag-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
dir="${GFLOG_INSTALL_DIR:-/usr/local/bin}"
sudo=""
if [ -d "$dir" ]; then
    [ -w "$dir" ] || sudo="sudo"
else
    [ -w "$(dirname "$dir")" ] || sudo="sudo"
fi

cat <<EOF

gflog installer — here is exactly what will happen:

  version     : $tag
  platform    : macOS $arch ($target)
  download    : $url
  verify      : sha256 checksum against $asset.sha256
  install to  : $dir/$BIN $([ -n "$sudo" ] && echo "  (needs sudo — you'll be prompted)")
  also        : clear macOS Gatekeeper quarantine on the binary (xattr)

This places a prebuilt, UNSIGNED binary on your PATH. Nothing else is changed.
No telemetry, no shell-profile edits. To remove later: rm $dir/$BIN

EOF

if [ "${GFLOG_YES:-}" != "1" ]; then
    printf 'Proceed? [y/N] '
    read ans </dev/tty 2>/dev/null || err "no terminal for confirmation (set GFLOG_YES=1 to install non-interactively)"
    case "$ans" in
        [yY] | [yY][eE][sS]) ;;
        *) err "aborted by user — nothing was installed" ;;
    esac
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading $asset ..."
curl -fSL "$url" -o "$tmp/$asset"
if curl -fsSL "$url.sha256" -o "$tmp/$asset.sha256"; then
    ( cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null ) || err "checksum verification FAILED — refusing to install"
    echo "checksum OK"
else
    err "could not fetch checksum — refusing to install an unverified binary"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
xattr -dr com.apple.quarantine "$tmp/$BIN" 2>/dev/null || true
chmod +x "$tmp/$BIN"

$sudo mkdir -p "$dir"
$sudo mv "$tmp/$BIN" "$dir/$BIN"

echo "installed: $dir/$BIN"
"$dir/$BIN" --version || true
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "note: $dir is not on your PATH — add it to use 'gflog' directly." ;;
esac
