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

# The real file a path names, every link followed (portable; see install.sh).
resolve_path() {
    local path="$1" target
    while [ -L "$path" ]; do
        target=$(readlink "$path")
        case "$target" in
            /*) path="$target" ;;
            *) path="$(dirname "$path")/$target" ;;
        esac
    done
    if [ -d "$(dirname "$path")" ]; then
        echo "$(cd "$(dirname "$path")" && pwd -P)/$(basename "$path")"
    else
        echo "$path"
    fi
}

if [ -z "$INSTALL_DIR" ]; then
    FOUND=$(command -v "$BIN_NAME" 2>/dev/null || true)
    if [ -n "$FOUND" ]; then
        REAL=$(resolve_path "$FOUND")
        case "$FOUND $REAL" in
            *"/Cellar/"*|*"/homebrew/"*|*"/linuxbrew/"*)
                echo "suv at $FOUND is managed by Homebrew. Remove it with:"
                echo "  suv uninstall      (or: brew uninstall suvadu)"
                exit 1
                ;;
            *"/.cargo/bin/"*|*"${CARGO_HOME:-/nonexistent-cargo-home}/bin/"*)
                echo "suv at $FOUND was installed with Cargo. Remove it with:"
                echo "  suv uninstall      (or: cargo uninstall suvadu)"
                exit 1
                ;;
        esac
        # Only a suv with the install script's suvadu link beside it is this
        # script's to delete — the rule `suv uninstall` applies too.
        LINK="$(dirname "$REAL")/$SYMLINK_NAME"
        if [ ! -L "$LINK" ] || [ "$(resolve_path "$LINK")" != "$REAL" ]; then
            echo "suv at $FOUND was not installed by the install script (there is no"
            echo "$SYMLINK_NAME link beside it), so this script leaves it alone. Remove it the"
            echo "way it was installed, or name its directory with --dir DIR."
            exit 1
        fi
        INSTALL_DIR=$(dirname "$REAL")
    fi
fi
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
case "$INSTALL_DIR" in
    "~") INSTALL_DIR="$HOME" ;;
    "~/"*) INSTALL_DIR="$HOME/${INSTALL_DIR#"~/"}" ;;
esac
case "$INSTALL_DIR" in
    /*) ;;
    *) INSTALL_DIR="$PWD/$INSTALL_DIR" ;;
esac
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
