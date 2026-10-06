#!/bin/bash
# Install Mirage: downloads a prebuilt binary from the latest GitHub release,
# and builds from source only when there is none for this machine.
set -euo pipefail

REPO="mukulx/mirage"
INSTALL_DIR="${MIRAGE_INSTALL_DIR:-$HOME/.local/bin}"

from_source() {
    if ! command -v cargo >/dev/null; then
        echo "No prebuilt binary for this system, and Cargo is missing. Install Rust first:" >&2
        echo "   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
        exit 1
    fi
    echo "Building from source (a few minutes)..."
    cargo install --git "https://github.com/$REPO" --locked --root "${INSTALL_DIR%/bin}"
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=linux-x86_64 ;;
    Linux-aarch64 | Linux-arm64) target=linux-aarch64 ;;
    *) target="" ;;
esac

echo "Installing Mirage Launcher..."
[ -n "$target" ] || { from_source; exit; }

# The newest release, pre-releases included (/releases/latest skips them).
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1 || true)
[ -n "$tag" ] || { echo "No release found yet."; from_source; exit; }

name="mirage-$tag-$target"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $tag ($target)..."
if ! curl -fsSL -o "$tmp/$name.tar.gz" "https://github.com/$REPO/releases/download/$tag/$name.tar.gz" \
    || ! curl -fsSL -o "$tmp/$name.tar.gz.sha256" "https://github.com/$REPO/releases/download/$tag/$name.tar.gz.sha256"; then
    echo "Download failed."
    from_source
    exit
fi
(cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null) || { echo "Checksum mismatch, aborting." >&2; exit 1; }

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$name/mirage" "$INSTALL_DIR/mirage"

echo "Installed $tag to $INSTALL_DIR/mirage"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Add it to your PATH:  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
echo "Run: mirage"
