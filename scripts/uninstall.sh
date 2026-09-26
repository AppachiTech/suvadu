#!/bin/bash
set -e

# Removes a Suvadu install made by the install script. History and config are
# kept. While suv still runs, `suv uninstall` does all of this and also removes
# agent integrations (Claude Code, Codex, ...); this script is the fallback.
#
# Usage: uninstall.sh [--user | --dir DIR]
#
# With no option, removes the suv found on PATH — unless Homebrew or Cargo
# owns it — and otherwise the one in /usr/local/bin. SUVADU_INSTALL_DIR=DIR
# is the same as --dir DIR.

BIN_NAME="suv"
SYMLINK_NAME="suvadu"
INSTALL_DIR="${SUVADU_INSTALL_DIR:-}"

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
        -h|--help)
            echo "Usage: uninstall.sh [--user | --dir DIR]"
            exit 0
            ;;
        *)
            echo "Error: unknown option '$1'. Usage: uninstall.sh [--user | --dir DIR]"
            exit 1
            ;;
    esac
    shift
done

if [ -z "$INSTALL_DIR" ]; then
    FOUND=$(command -v "$BIN_NAME" 2>/dev/null || true)
    if [ -n "$FOUND" ]; then
        TARGET=$(readlink "$FOUND" 2>/dev/null || true)
        case "$FOUND $TARGET" in
            *"/Cellar/"*|*"/homebrew/"*|*"/linuxbrew/"*)
                echo "suv at $FOUND is managed by Homebrew. Remove it with:"
                echo "  suv uninstall      (or: brew uninstall suvadu)"
                exit 1
                ;;
            *"/.cargo/bin/"*)
                echo "suv at $FOUND was installed with Cargo. Remove it with:"
                echo "  suv uninstall      (or: cargo uninstall suvadu)"
                exit 1
                ;;
        esac
        INSTALL_DIR=$(dirname "$FOUND")
    fi
fi
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
INSTALL_DIR="${INSTALL_DIR%/}"

echo "Uninstalling Suvadu from $INSTALL_DIR..."

if [ -e "$INSTALL_DIR/$BIN_NAME" ] || [ -L "$INSTALL_DIR/$SYMLINK_NAME" ]; then
    SUDO=""
    if [ ! -w "$INSTALL_DIR" ]; then
        SUDO="sudo"
        echo "$INSTALL_DIR is not writable; using sudo."
    fi
    for name in "$BIN_NAME" "$SYMLINK_NAME"; do
        if [ -e "$INSTALL_DIR/$name" ] || [ -L "$INSTALL_DIR/$name" ]; then
            echo "Removing $INSTALL_DIR/$name"
            $SUDO rm -f "$INSTALL_DIR/$name"
        fi
    done
else
    echo "$BIN_NAME not found in $INSTALL_DIR"
fi

# Remove the shell hook line, and only that line, from files that have it —
# compared with surrounding whitespace trimmed, as `suv uninstall` does. The
# file as it was is kept beside it, so the edit can be undone.
remove_hook() {
    local file="$1" shell="$2" backup
    local line="eval \"\$(suv init $shell)\""
    local trimmed='{ t = $0; gsub(/^[ \t]+|[ \t]+$/, "", t) }'
    [ -f "$file" ] || return 0
    awk -v l="$line" "$trimmed"' t == l { found = 1 } END { exit !found }' "$file" || return 0
    backup="$file.suvadu-backup"
    if [ -e "$backup" ]; then
        backup="$backup.$(date +%Y%m%d%H%M%S)"
    fi
    cp -p "$file" "$backup"
    awk -v l="$line" "$trimmed"' t != l { print }' "$backup" >"$file"
    echo "Removed the Suvadu hook from $file (previous version: $backup)"
}

remove_hook "$HOME/.zshrc" zsh
remove_hook "$HOME/.bashrc" bash
remove_hook "$HOME/.bash_profile" bash

echo "Uninstallation complete. Your history and config were kept."
echo "Open a new terminal for the change to take effect."
