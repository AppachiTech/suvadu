#!/bin/bash
set -e

INSTALL_PATH="/usr/local/bin"
BIN_NAME="suv"
SYMLINK_NAME="suvadu"

echo "Uninstalling Suvadu..."

if [ -f "$INSTALL_PATH/$BIN_NAME" ]; then
    echo "Removing $INSTALL_PATH/$BIN_NAME"
    sudo rm -f "$INSTALL_PATH/$BIN_NAME"
else
    echo "$BIN_NAME not found in $INSTALL_PATH"
fi

if [ -L "$INSTALL_PATH/$SYMLINK_NAME" ]; then
    echo "Removing $INSTALL_PATH/$SYMLINK_NAME"
    sudo rm -f "$INSTALL_PATH/$SYMLINK_NAME"
else
    echo "$SYMLINK_NAME link not found in $INSTALL_PATH"
fi

# Remove shell integration hook from .zshrc
if [ -f "$HOME/.zshrc" ]; then
    echo "Removing shell integration hook from $HOME/.zshrc"
    # Use sed to remove the line. macOS requires -i ''
    if [[ "$OSTYPE" == "darwin"* ]]; then
        sed -i '' '/eval "$(suv init zsh)"/d' "$HOME/.zshrc"
    else
        sed -i '/eval "$(suv init zsh)"/d' "$HOME/.zshrc"
    fi
fi

# Remove shell integration hook from bash configs
for file in ".bash_profile" ".bashrc"; do
    if [ -f "$HOME/$file" ]; then
        echo "Removing shell integration hook from $HOME/$file"
        if [[ "$OSTYPE" == "darwin"* ]]; then
            sed -i '' '/eval "$(suv init bash)"/d' "$HOME/$file"
        else
            sed -i '/eval "$(suv init bash)"/d' "$HOME/$file"
        fi
    fi
done

echo "Uninstallation complete."
echo "Note: You may need to restart your terminal or run 'source ~/.zshrc' for changes to take effect."
