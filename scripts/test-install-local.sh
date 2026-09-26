#!/bin/bash
# Exercises scripts/install.sh end to end against the published build, inside
# a throwaway HOME and a PATH that holds no suv of the developer's own.
#
# Covers the paths that decide *where* suv lands and whether sudo is needed:
# an explicit --dir, --user, updating a script install in place, and leaving
# a Homebrew-managed suv alone. `sudo` on PATH is a stub that fails the run,
# so any case that should not need it proves it did not use it.
#
# Needs network access to downloads.appachi.tech. Nothing outside the
# temporary directory is touched.
#
# Usage: scripts/test-install-local.sh [installer]

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="${1:-$SCRIPT_DIR/install.sh}"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat >"$STUBS/sudo" <<'EOF'
#!/bin/sh
echo "sudo was called: $*" >&2
exit 97
EOF
chmod +x "$STUBS/sudo"

BASE_PATH="$STUBS:/usr/bin:/bin:/usr/sbin:/sbin"
failures=0

pass() { printf 'ok    %s\n' "$1"; }
fail() {
    printf 'FAIL  %s\n' "$1"
    printf '%s\n' "$2" | sed 's/^/      | /'
    failures=$((failures + 1))
}

# run_installer HOME PATH [args...] — prints combined output, returns status.
run_installer() {
    local home="$1" path="$2"
    shift 2
    HOME="$home" PATH="$path" SHELL=/bin/zsh bash "$INSTALLER" "$@" </dev/null 2>&1
}

# 1. Explicit --dir into a fresh directory: no sudo, both names, PATH hint.
home1="$WORK/home1"
mkdir -p "$home1"
out=$(run_installer "$home1" "$BASE_PATH" --dir "$WORK/opt/bin")
status=$?
if [ $status -eq 0 ] && [ -x "$WORK/opt/bin/suv" ] && [ -L "$WORK/opt/bin/suvadu" ] \
    && "$WORK/opt/bin/suv" --version | grep -q suvadu \
    && [[ "$out" == *"is not on your PATH"* ]] && [[ "$out" != *"sudo was called"* ]]; then
    pass "--dir installs into a new directory without sudo"
else
    fail "--dir installs into a new directory without sudo" "$out"
fi

# 2. Same directory again: recognised as current, nothing re-downloaded.
out=$(run_installer "$home1" "$BASE_PATH" --dir "$WORK/opt/bin")
if [[ "$out" == *"Already on the latest version"* ]]; then
    pass "re-running --dir on a current install does nothing"
else
    fail "re-running --dir on a current install does nothing" "$out"
fi

# 3. An older script install on PATH is updated where it is.
home3="$WORK/home3"
mkdir -p "$home3" "$WORK/old/bin"
cat >"$WORK/old/bin/suv" <<'EOF'
#!/bin/sh
echo "suvadu 0.0.1"
EOF
chmod +x "$WORK/old/bin/suv"
out=$(run_installer "$home3" "$WORK/old/bin:$BASE_PATH")
if grep -q "suvadu" <("$WORK/old/bin/suv" --version 2>/dev/null) \
    && ! "$WORK/old/bin/suv" --version 2>/dev/null | grep -q "0.0.1" \
    && [[ "$out" == *"Updating v0.0.1"* ]] && [[ "$out" != *"is not on your PATH"* ]] \
    && [[ "$out" != *"sudo was called"* ]]; then
    pass "an existing script install is updated in place"
else
    fail "an existing script install is updated in place" "$out"
fi

# 4. A Homebrew-managed suv is left alone, with the brew command to use.
home4="$WORK/home4"
cellar="$WORK/brew/Cellar/suvadu/0.0.1/bin"
mkdir -p "$home4" "$cellar" "$WORK/brew/bin"
printf '#!/bin/sh\necho "suvadu 0.0.1"\n' >"$cellar/suv"
chmod +x "$cellar/suv"
ln -s "../Cellar/suvadu/0.0.1/bin/suv" "$WORK/brew/bin/suv"
out=$(run_installer "$home4" "$WORK/brew/bin:$BASE_PATH")
status=$?
if [ $status -eq 0 ] && [[ "$out" == *"managed by Homebrew"* ]] \
    && [[ "$out" == *"brew upgrade suvadu"* ]] && [[ "$out" != *"Downloading"* ]] \
    && grep -q "0.0.1" "$cellar/suv"; then
    pass "a Homebrew-managed suv is not overwritten or shadowed"
else
    fail "a Homebrew-managed suv is not overwritten or shadowed" "$out"
fi

# 5. --user goes to ~/.local/bin without sudo.
home5="$WORK/home5"
mkdir -p "$home5"
out=$(run_installer "$home5" "$BASE_PATH" --user)
if [ -x "$home5/.local/bin/suv" ] && [[ "$out" != *"sudo was called"* ]]; then
    pass "--user installs into ~/.local/bin without sudo"
else
    fail "--user installs into ~/.local/bin without sudo" "$out"
fi

# 6. No shell startup file is edited without being asked.
if [ ! -e "$home1/.zshrc" ] && [ ! -e "$home5/.zshrc" ] && [ ! -e "$home5/.bashrc" ]; then
    pass "no startup file is written without a terminal to ask on"
else
    fail "no startup file is written without a terminal to ask on" "$(ls -la "$home1" "$home5")"
fi

# 7. An unknown option is an error, not a silent default install.
out=$(run_installer "$home1" "$BASE_PATH" --prefix /tmp/x)
status=$?
if [ $status -ne 0 ] && [[ "$out" == *"unknown option"* ]]; then
    pass "an unknown option is rejected"
else
    fail "an unknown option is rejected" "$out"
fi

echo ""
if [ "$failures" -gt 0 ]; then
    echo "$failures installer check(s) failed."
    exit 1
fi
echo "All installer checks passed."
