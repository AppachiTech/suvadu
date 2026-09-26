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

# 7. With minisign present, a signature it rejects stops the install.
mkdir -p "$WORK/minisign-bad"
printf '#!/bin/sh\necho "Signature verification failed" >&2\nexit 1\n' >"$WORK/minisign-bad/minisign"
chmod +x "$WORK/minisign-bad/minisign"
out=$(run_installer "$home1" "$WORK/minisign-bad:$BASE_PATH" --dir "$WORK/signed-bad/bin")
status=$?
if [ $status -ne 0 ] && [[ "$out" == *"signature verification FAILED"* ]] \
    && [ ! -e "$WORK/signed-bad/bin/suv" ]; then
    pass "a signature minisign rejects aborts before anything is installed"
else
    fail "a signature minisign rejects aborts before anything is installed" "$out"
fi

# 8. A real minisign, if the caller has one, accepts the published signature.
if command -v minisign >/dev/null 2>&1; then
    real_minisign_dir=$(dirname "$(command -v minisign)")
    out=$(run_installer "$home1" "$real_minisign_dir:$BASE_PATH" --dir "$WORK/signed/bin")
    if [[ "$out" == *"Signature verified (minisign)"* ]] && [ -x "$WORK/signed/bin/suv" ]; then
        pass "minisign verifies the published signature"
    else
        fail "minisign verifies the published signature" "$out"
    fi
else
    printf 'skip  minisign verifies the published signature (minisign not installed)\n'
fi

# run_in_terminal ANSWER HOME PATH [args...] — runs the installer on a pseudo
# terminal, typing ANSWER at the first "[y/N]" prompt. Prints the transcript.
run_in_terminal() {
    local answer="$1" home="$2" path="$3"
    shift 3
    HOME="$home" PATH="$path" SHELL=/bin/zsh SUVADU_INSTALL_OS=Linux ANSWER="$answer" \
        python3 - "$INSTALLER" "$@" <<'PY'
import os, pty, select, sys
pid, fd = pty.fork()
if pid == 0:
    os.execvp("bash", ["bash"] + sys.argv[1:])
out, answered = b"", False
while True:
    ready, _, _ = select.select([fd], [], [], 300)
    if not ready:
        break
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    out += chunk
    if not answered and b"[y/N]" in out:
        os.write(fd, os.environ["ANSWER"].encode() + b"\n")
        answered = True
_, status = os.waitpid(pid, 0)
sys.stdout.write(out.decode(errors="replace"))
sys.exit(os.waitstatus_to_exitcode(status))
PY
}

if command -v python3 >/dev/null 2>&1; then
    # 9. At a terminal, "y" appends the hook once and keeps the old file.
    home9="$WORK/home9"
    mkdir -p "$home9"
    printf 'export EDITOR=vi\n' >"$home9/.zshrc"
    out=$(run_in_terminal y "$home9" "$WORK/tty/bin:$BASE_PATH" --dir "$WORK/tty/bin")
    if [ "$(grep -c '^eval "$(suv init zsh)"$' "$home9/.zshrc")" = 1 ] \
        && grep -q '^export EDITOR=vi$' "$home9/.zshrc" \
        && [ "$(cat "$home9/.zshrc.suvadu-backup")" = "export EDITOR=vi" ] \
        && [[ "$out" == *'eval "$(suv init zsh)"'* ]]; then
        pass "at a terminal, yes adds the hook once and keeps a backup"
    else
        fail "at a terminal, yes adds the hook once and keeps a backup" "$out"
    fi

    # 10. With the hook already present there is nothing to ask.
    out=$(run_in_terminal y "$home9" "$WORK/tty2/bin:$BASE_PATH" --dir "$WORK/tty2/bin")
    if [[ "$out" != *"[y/N]"* ]] && [ "$(grep -c 'suv init zsh' "$home9/.zshrc")" = 1 ]; then
        pass "an existing hook is never added twice"
    else
        fail "an existing hook is never added twice" "$out"
    fi

    # 11. Anything but yes leaves the file byte-for-byte alone.
    home11="$WORK/home11"
    mkdir -p "$home11"
    printf 'export EDITOR=vi\n' >"$home11/.zshrc"
    out=$(run_in_terminal n "$home11" "$WORK/tty3/bin:$BASE_PATH" --dir "$WORK/tty3/bin")
    if [ "$(cat "$home11/.zshrc")" = "export EDITOR=vi" ] && [ ! -e "$home11/.zshrc.suvadu-backup" ] \
        && [[ "$out" == *"Left $home11/.zshrc unchanged"* ]]; then
        pass "declining leaves the startup file untouched"
    else
        fail "declining leaves the startup file untouched" "$out"
    fi

    # 12. --no-modify-rc never asks, even at a terminal.
    out=$(run_in_terminal y "$home11" "$WORK/tty4/bin:$BASE_PATH" --dir "$WORK/tty4/bin" --no-modify-rc)
    if [[ "$out" != *"[y/N]"* ]] && [ "$(cat "$home11/.zshrc")" = "export EDITOR=vi" ]; then
        pass "--no-modify-rc never offers to edit a startup file"
    else
        fail "--no-modify-rc never offers to edit a startup file" "$out"
    fi
else
    printf 'skip  terminal prompts (python3 not installed)\n'
fi

# 13. uninstall.sh removes what the installer added, and nothing else.
UNINSTALLER="$(dirname "$INSTALLER")/uninstall.sh"
home13="$WORK/home13"
mkdir -p "$home13"
printf 'export EDITOR=vi\n  eval "$(suv init zsh)"\nalias ll="ls -l"\n' >"$home13/.zshrc"
printf 'export PS1=x\n' >"$home13/.bashrc"
out=$(run_installer "$home13" "$BASE_PATH" --dir "$WORK/un/bin" --no-modify-rc)
touch "$WORK/un/bin/other-tool"
out=$(HOME="$home13" PATH="$BASE_PATH" bash "$UNINSTALLER" --dir "$WORK/un/bin" 2>&1)
if [ ! -e "$WORK/un/bin/suv" ] && [ ! -L "$WORK/un/bin/suvadu" ] && [ -e "$WORK/un/bin/other-tool" ] \
    && [ "$(cat "$home13/.zshrc")" = "$(printf 'export EDITOR=vi\nalias ll="ls -l"')" ] \
    && grep -q 'suv init zsh' "$home13/.zshrc.suvadu-backup" \
    && [ ! -e "$home13/.bashrc.suvadu-backup" ] && [[ "$out" != *"sudo was called"* ]]; then
    pass "uninstall.sh removes suv, its link and the hook line, keeping a backup"
else
    fail "uninstall.sh removes suv, its link and the hook line, keeping a backup" "$out"
fi

# 14. Running it again finds nothing to do and makes no new backups.
out=$(HOME="$home13" PATH="$BASE_PATH" bash "$UNINSTALLER" --dir "$WORK/un/bin" 2>&1)
if [[ "$out" == *"not found"* ]] && [ "$(ls -a "$home13" | grep -c suvadu-backup)" = 1 ]; then
    pass "a second uninstall is a no-op"
else
    fail "a second uninstall is a no-op" "$out
$(ls -a "$home13")"
fi

# 15. An unknown option is an error, not a silent default install.
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
