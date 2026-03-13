#!/bin/bash
set -e

# Suvadu installer — handles both fresh installs and updates.
# Usage: curl -fsSL https://downloads.appachi.tech/install.sh | bash

BIN_NAME="suv"
SYMLINK_NAME="suvadu"
INSTALL_DIR="/usr/local/bin"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)
        echo "Error: Unsupported OS '$OS'. Only Linux and macOS are supported."
        exit 1
        ;;
esac

case "$ARCH" in
    aarch64|arm64) ARCH_SUFFIX="-aarch64" ;;
    x86_64)        ARCH_SUFFIX="" ;;
    *)
        echo "Error: Unsupported architecture '$ARCH'."
        exit 1
        ;;
esac

ARCHIVE="suv-${PLATFORM}${ARCH_SUFFIX}-latest.tar.gz"
URL="https://downloads.appachi.tech/${PLATFORM}/${ARCHIVE}"
CHECKSUM_URL="${URL}.sha256"

VERSION_URL="https://downloads.appachi.tech/version.txt"

echo "Suvadu installer"
echo ""

# Show current version if already installed
CURRENT_VERSION=""
if command -v "$BIN_NAME" &>/dev/null; then
    CURRENT_VERSION=$("$BIN_NAME" version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || echo "")
    echo "Current version: ${CURRENT_VERSION:-unknown}"
fi

echo "Platform: ${PLATFORM} ${ARCH}"
echo ""

# Check latest version and skip if already up to date
LATEST_VERSION=$(curl --proto '=https' -fsSL -m 10 "$VERSION_URL" 2>/dev/null | tr -d '[:space:]')
if [ -n "$LATEST_VERSION" ] && [ -n "$CURRENT_VERSION" ]; then
    if [ "$CURRENT_VERSION" = "$LATEST_VERSION" ]; then
        echo "Already on the latest version (v${LATEST_VERSION}). Nothing to do."
        exit 0
    fi
    echo "Updating v${CURRENT_VERSION} -> v${LATEST_VERSION}..."
elif [ -n "$CURRENT_VERSION" ]; then
    echo "Updating..."
else
    echo "Installing..."
fi
echo ""

# Download
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading from: $URL"
curl --proto '=https' -fsSL -m 300 -o "$TMPDIR/$ARCHIVE" "$URL"

# Verify checksum
EXPECTED=$(curl --proto '=https' -fsSL -m 30 "$CHECKSUM_URL" | awk '{print $1}')
if [ -z "$EXPECTED" ]; then
    echo "Error: Could not fetch checksum. Aborting for security."
    exit 1
fi

if command -v sha256sum &>/dev/null; then
    ACTUAL=$(sha256sum "$TMPDIR/$ARCHIVE" | awk '{print $1}')
elif command -v shasum &>/dev/null; then
    ACTUAL=$(shasum -a 256 "$TMPDIR/$ARCHIVE" | awk '{print $1}')
else
    echo "Error: No sha256sum or shasum found. Cannot verify download."
    exit 1
fi

if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: Checksum mismatch!"
    echo "  Expected: $EXPECTED"
    echo "  Got:      $ACTUAL"
    echo "Aborting — the download may be corrupted or tampered with."
    exit 1
fi
echo "SHA256 checksum verified: ${ACTUAL:0:16}"

# Extract
tar --no-same-owner -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"

if [ ! -f "$TMPDIR/$BIN_NAME" ]; then
    echo "Error: Binary not found after extraction."
    exit 1
fi

# Install — remove first to avoid "Text file busy" on Linux
echo ""
echo "Installing to $INSTALL_DIR (requires sudo)..."
sudo rm -f "$INSTALL_DIR/$BIN_NAME"
sudo cp "$TMPDIR/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
sudo chmod 755 "$INSTALL_DIR/$BIN_NAME"
sudo ln -sf "$INSTALL_DIR/$BIN_NAME" "$INSTALL_DIR/$SYMLINK_NAME"

echo ""
NEW_VERSION=$("$INSTALL_DIR/$BIN_NAME" version 2>/dev/null || echo "installed")
echo "Suvadu $NEW_VERSION"
echo ""

# Check if shell integration is already set up
if ! grep -q 'eval "$(suv init' "$HOME/.zshrc" 2>/dev/null && \
   ! grep -q 'eval "$(suv init' "$HOME/.bashrc" 2>/dev/null && \
   ! grep -q 'eval "$(suv init' "$HOME/.bash_profile" 2>/dev/null; then
    echo "To set up shell integration, run:"
    echo ""
    echo "  # For zsh:"
    echo "  echo 'eval \"\$(suv init zsh)\"' >> ~/.zshrc && source ~/.zshrc"
    echo ""
    echo "  # For bash:"
    echo "  echo 'eval \"\$(suv init bash)\"' >> ~/.bashrc && source ~/.bashrc"
fi
