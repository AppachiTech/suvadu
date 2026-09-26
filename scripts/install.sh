#!/bin/bash
set -e

# Suvadu installer — handles both fresh installs and updates.
# Usage: curl -fsSL https://downloads.appachi.tech/suvadu/install.sh | bash
#        curl -fsSL https://downloads.appachi.tech/suvadu/install.sh | bash -s -- --user
#
# Options:
#   --user          Install into ~/.local/bin. No sudo.
#   --dir DIR       Install into DIR. sudo is used only if DIR is not writable.
#   --no-modify-rc  Never offer to add the shell hook to a startup file.
#   -h, --help      Show this help.
#
# SUVADU_INSTALL_DIR=DIR is the same as --dir DIR.
#
# With no directory given, an existing script-installed suv is updated where
# it is; otherwise suv goes to /usr/local/bin, as it always has.

BIN_NAME="suv"
SYMLINK_NAME="suvadu"
DEFAULT_INSTALL_DIR="/usr/local/bin"
INSTALL_DIR="${SUVADU_INSTALL_DIR:-}"
MODIFY_RC=1

while [ $# -gt 0 ]; do
    case "$1" in
        --user) INSTALL_DIR="$HOME/.local/bin" ;;
        --dir)
            if [ -z "${2:-}" ]; then
                echo "Error: --dir needs a directory."
                exit 1
            fi
            INSTALL_DIR="$2"
            shift
            ;;
        --dir=*) INSTALL_DIR="${1#--dir=}" ;;
        --no-modify-rc) MODIFY_RC=0 ;;
        -h|--help)
            echo "Suvadu installer"
            echo ""
            echo "  curl -fsSL https://downloads.appachi.tech/suvadu/install.sh | bash -s -- [options]"
            echo ""
            echo "  --user          Install into ~/.local/bin. No sudo."
            echo "  --dir DIR       Install into DIR. sudo is used only if DIR is not writable."
            echo "  --no-modify-rc  Never offer to add the shell hook to a startup file."
            echo ""
            echo "SUVADU_INSTALL_DIR=DIR is the same as --dir DIR."
            exit 0
            ;;
        *)
            echo "Error: unknown option '$1'. Run with --help to see the options."
            exit 1
            ;;
    esac
    shift
done

# Detect platform.
#
# The release workflow names each OS's primary target with no arch suffix and
# the secondary one with a suffix, and the primary target differs per OS:
#   linux  -> x86_64 is "suv-linux-latest",  aarch64 is "suv-linux-aarch64-latest"
#   macos  -> arm64  is "suv-macos-latest",  x86_64  is "suv-macos-x86_64-latest"
# Resolve OS and architecture together so these stay in step; a single
# arch-only mapping previously requested a macOS file that is never published.
# SUVADU_INSTALL_OS/ARCH exist so scripts/test-install-urls.sh can check every
# combination from one machine.
OS="${SUVADU_INSTALL_OS:-$(uname -s)}"
ARCH="${SUVADU_INSTALL_ARCH:-$(uname -m)}"

case "$OS/$ARCH" in
    Linux/x86_64)                 PLATFORM="linux"; ARCH_SUFFIX="" ;;
    Linux/aarch64|Linux/arm64)    PLATFORM="linux"; ARCH_SUFFIX="-aarch64" ;;
    Darwin/arm64|Darwin/aarch64)  PLATFORM="macos"; ARCH_SUFFIX="" ;;
    Darwin/x86_64)                PLATFORM="macos"; ARCH_SUFFIX="-x86_64" ;;
    Linux/*|Darwin/*)
        echo "Error: Unsupported architecture '$ARCH' on $OS."
        echo "Build from source instead: cargo install suvadu"
        exit 1
        ;;
    *)
        echo "Error: Unsupported OS '$OS'. Only Linux and macOS are supported."
        exit 1
        ;;
esac

ARCHIVE="suv-${PLATFORM}${ARCH_SUFFIX}-latest.tar.gz"
URL="https://downloads.appachi.tech/${PLATFORM}/${ARCHIVE}"
CHECKSUM_URL="${URL}.sha256"

VERSION_URL="https://downloads.appachi.tech/version.txt"

# Used by scripts/test-install-urls.sh to check the resolved URL without installing.
if [ -n "${SUVADU_INSTALL_PRINT_URL:-}" ]; then
    echo "$URL"
    exit 0
fi

echo "Suvadu installer"
echo ""

# Which package manager, if any, owns the suv at this path. Homebrew links its
# binary from a Cellar, so the link target is checked as well as the path:
# on Intel macOS, /usr/local/bin/suv can be Homebrew's.
managed_by() {
    local target
    target=$(readlink "$1" 2>/dev/null || true)
    case "$1 $target" in
        *"/Cellar/"*|*"/homebrew/"*|*"/linuxbrew/"*) echo "homebrew" ;;
        *"/.cargo/bin/"*) echo "cargo" ;;
    esac
}

# With no directory given, update the suv already on PATH in place — unless a
# package manager owns it. Overwriting Homebrew's link would break brew, and a
# second copy elsewhere on PATH would shadow one install with the other.
if [ -z "$INSTALL_DIR" ]; then
    FOUND=$(command -v "$BIN_NAME" 2>/dev/null || true)
    if [ -n "$FOUND" ]; then
        case "$(managed_by "$FOUND")" in
            homebrew)
                echo "suv at $FOUND is managed by Homebrew. Update it with:"
                echo "  brew upgrade suvadu"
                echo ""
                echo "To install a separate copy anyway, pass --user or --dir DIR."
                exit 0
                ;;
            cargo)
                echo "suv at $FOUND was installed with Cargo. Update it with:"
                echo "  cargo install suvadu"
                echo ""
                echo "To install a separate copy anyway, pass --user or --dir DIR."
                exit 0
                ;;
            *) INSTALL_DIR=$(dirname "$FOUND") ;;
        esac
    fi
fi
INSTALL_DIR="${INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
INSTALL_DIR="${INSTALL_DIR%/}"

# Show current version if already installed here
CURRENT_VERSION=""
if [ -x "$INSTALL_DIR/$BIN_NAME" ]; then
    CURRENT_VERSION=$("$INSTALL_DIR/$BIN_NAME" version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || echo "")
    echo "Current version: ${CURRENT_VERSION:-unknown} ($INSTALL_DIR/$BIN_NAME)"
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
if ! curl --proto '=https' -fsSL -m 300 -o "$TMPDIR/$ARCHIVE" "$URL"; then
    echo ""
    echo "Error: could not download $ARCHIVE for ${PLATFORM} ${ARCH}."
    echo "Install another way, or report this build as missing:"
    echo "  brew tap AppachiTech/suvadu && brew install suvadu"
    echo "  cargo install suvadu"
    echo "  https://github.com/AppachiTech/suvadu/issues"
    exit 1
fi

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

# A checksum fetched from the same server only proves the download was not
# damaged. The minisign signature proves the maintainers built it; `suv update`
# always checks it with the key compiled into suv. This key must equal
# MINISIGN_PUBLIC_KEY in src/update.rs — a unit test there compares them.
MINISIGN_PUBLIC_KEY="RWSnsbPkvYdmk4EtxJ9WjItHLwx/GkmnBFNjeUhGWT2Z2efNdLTNMBy5"
if command -v minisign &>/dev/null; then
    if ! curl --proto '=https' -fsSL -m 30 -o "$TMPDIR/$ARCHIVE.minisig" "${URL}.minisig"; then
        echo "Error: could not fetch the release signature. Aborting for security."
        exit 1
    fi
    if ! minisign -Vm "$TMPDIR/$ARCHIVE" -x "$TMPDIR/$ARCHIVE.minisig" \
        -P "$MINISIGN_PUBLIC_KEY" >/dev/null 2>&1; then
        echo "Error: signature verification FAILED."
        echo "The download was not signed by the Suvadu maintainers. Aborting."
        exit 1
    fi
    echo "Signature verified (minisign)"
else
    echo "Signature not checked: minisign is not installed (suv update always checks it)."
fi

# Extract
tar --no-same-owner -xzf "$TMPDIR/$ARCHIVE" -C "$TMPDIR"

if [ ! -f "$TMPDIR/$BIN_NAME" ]; then
    echo "Error: Binary not found after extraction."
    exit 1
fi

# Install — remove first to avoid "Text file busy" on Linux. sudo only when
# the directory cannot be written as this user.
echo ""
if mkdir -p "$INSTALL_DIR" 2>/dev/null && [ -w "$INSTALL_DIR" ]; then
    SUDO=""
    echo "Installing to $INSTALL_DIR..."
else
    if ! command -v sudo &>/dev/null; then
        echo "Error: $INSTALL_DIR is not writable and sudo is not available."
        echo "Install into your home directory instead:"
        echo "  curl -fsSL https://downloads.appachi.tech/suvadu/install.sh | bash -s -- --user"
        exit 1
    fi
    SUDO="sudo"
    echo "Installing to $INSTALL_DIR (requires sudo; --user installs without it)..."
    sudo mkdir -p "$INSTALL_DIR"
fi
$SUDO rm -f "$INSTALL_DIR/$BIN_NAME"
$SUDO cp "$TMPDIR/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
$SUDO chmod 755 "$INSTALL_DIR/$BIN_NAME"
$SUDO ln -sf "$INSTALL_DIR/$BIN_NAME" "$INSTALL_DIR/$SYMLINK_NAME"

echo ""
NEW_VERSION=$("$INSTALL_DIR/$BIN_NAME" version 2>/dev/null || echo "installed")
echo "Suvadu $NEW_VERSION"
echo ""

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ON_PATH=1 ;;
    *) ON_PATH=0 ;;
esac

if [ "$ON_PATH" = 0 ]; then
    echo "$INSTALL_DIR is not on your PATH. Add it in your shell startup file"
    echo "(~/.zshrc or ~/.bashrc), above the Suvadu hook:"
    echo ""
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    echo ""
fi

print_hook_instructions() {
    echo "To set up shell integration, run:"
    echo ""
    echo "  # For zsh:"
    echo "  echo 'eval \"\$(suv init zsh)\"' >> ~/.zshrc && source ~/.zshrc"
    echo ""
    echo "  # For bash:"
    echo "  echo 'eval \"\$(suv init bash)\"' >> ~/.bashrc && source ~/.bashrc"
}

# The startup file this shell reads, when there is exactly one sure answer.
# Not offered: a ZDOTDIR elsewhere, and bash on macOS, whose login shells read
# ~/.bash_profile and may never read ~/.bashrc.
hook_rc_file() {
    case "$(basename "${SHELL:-}")" in
        zsh)
            if [ -z "${ZDOTDIR:-}" ] || [ "${ZDOTDIR%/}" = "${HOME%/}" ]; then
                echo "$HOME/.zshrc"
            fi
            ;;
        bash) [ "$OS" = "Linux" ] && echo "$HOME/.bashrc" ;;
    esac
    return 0
}

# Shell integration. Nothing is written without a "y" typed at the terminal:
# the exact line and file are shown first, a copy of an existing file is kept,
# and a hook already in any startup file means there is nothing to add.
if ! grep -qs 'eval "$(suv init' "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile"; then
    RC_FILE=$(hook_rc_file)
    if [ "$MODIFY_RC" = 1 ] && [ -n "$RC_FILE" ] && [ "$ON_PATH" = 1 ] \
        && [ -t 1 ] && { : </dev/tty; } 2>/dev/null; then
        HOOK_LINE="eval \"\$(suv init $(basename "$SHELL"))\""
        echo "Suvadu records commands once its hook is in your shell's startup file."
        echo "It can add this line to the end of $RC_FILE:"
        echo ""
        echo "  $HOOK_LINE"
        echo ""
        printf "Add it now? [y/N] "
        read -r ANSWER </dev/tty || ANSWER=""
        case "$ANSWER" in
            y|Y|yes|Yes|YES)
                if [ -f "$RC_FILE" ]; then
                    BACKUP="$RC_FILE.suvadu-backup"
                    if [ -e "$BACKUP" ]; then
                        BACKUP="$BACKUP.$(date +%Y%m%d%H%M%S)"
                    fi
                    cp -p "$RC_FILE" "$BACKUP"
                    echo "Saved the previous $RC_FILE as $BACKUP"
                fi
                printf '\n%s\n' "$HOOK_LINE" >>"$RC_FILE"
                echo "Added. Open a new terminal (or run: source $RC_FILE), then check with: suv status"
                echo "suv uninstall removes this line again."
                ;;
            *)
                echo "Left $RC_FILE unchanged."
                echo ""
                print_hook_instructions
                ;;
        esac
    else
        print_hook_instructions
    fi
fi

# Agent hook definitions are not refreshed by replacing the binary.
echo ""
echo 'After updating agent integrations:'
echo '  Codex: run suv init codex, then review/trust Suvadu hooks with /hooks in the Codex terminal CLI.'
echo '  Relaunch Codex; for its VS Code extension, fully quit and reopen VS Code.'
echo '  Claude Code: after suv init claude-code, relaunch Claude Code (or its VS Code host).'
echo '  Native session and token capture supports Claude Code, Codex, and OpenCode.'
