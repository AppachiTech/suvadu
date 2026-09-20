#!/bin/bash
# Checks that every URL scripts/install.sh can resolve is actually published.
#
# The installer's OS/arch mapping and the release workflow's artifact names are
# maintained separately; when they drifted apart, `curl | bash` asked for
# suv-macos-aarch64-latest.tar.gz, which is never published, and every Apple
# Silicon install failed with a 404 while Intel silently downloaded the arm64
# build. Run this after a release, or in CI.
#
# Usage: scripts/test-install-urls.sh [base-url]

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="$SCRIPT_DIR/install.sh"
BASE="${1:-https://downloads.appachi.tech}"

COMBINATIONS=(
    "Linux x86_64"
    "Linux aarch64"
    "Darwin arm64"
    "Darwin x86_64"
)

failures=0

for combo in "${COMBINATIONS[@]}"; do
    read -r os arch <<<"$combo"
    url=$(SUVADU_INSTALL_OS="$os" SUVADU_INSTALL_ARCH="$arch" SUVADU_INSTALL_PRINT_URL=1 bash "$INSTALLER")
    url="${url/https:\/\/downloads.appachi.tech/$BASE}"

    archive_status=$(curl -s -o /dev/null -w '%{http_code}' -I "$url")
    checksum_status=$(curl -s -o /dev/null -w '%{http_code}' -I "$url.sha256")

    if [ "$archive_status" = "200" ] && [ "$checksum_status" = "200" ]; then
        printf 'ok    %-16s %s\n' "$os $arch" "$url"
    else
        printf 'FAIL  %-16s %s (archive %s, checksum %s)\n' "$os $arch" "$url" "$archive_status" "$checksum_status"
        failures=$((failures + 1))
    fi
done

if [ "$failures" -gt 0 ]; then
    echo ""
    echo "$failures of ${#COMBINATIONS[@]} platform downloads are missing."
    echo "Either the installer's mapping or the release artifact names are wrong."
    exit 1
fi

echo ""
echo "All ${#COMBINATIONS[@]} platform downloads resolve."
