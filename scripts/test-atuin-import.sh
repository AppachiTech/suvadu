#!/bin/bash
# Imports a history database written by a real Atuin into Suvadu, end to end.
#
# The unit fixtures in src/import_export/atuin.rs and tests/atuin_import.rs
# build Atuin-shaped databases by hand. This script instead has an actual
# `atuin` binary record the history — a plain command, a multi-line one, a
# failure, an agent-authored row with an intent, Unicode, a deleted row, a
# command that never finished, and rows Atuin imported from a Bash history
# file — and then checks that `suv import --from atuin-db`:
#
#   * accepts the schema and imports every live row,
#   * leaves the Atuin database, its WAL and its shared-memory file
#     byte-for-byte unchanged,
#   * adds nothing when run again,
#   * prints a backup that really restores Suvadu to its pre-import state.
#
# Run it when a new Atuin release appears, before adding that release to the
# tested range in the README, SECURITY.md and `suv import --help`. Needs the
# sqlite3 CLI. Everything happens under a temporary directory.
#
# Usage: scripts/test-atuin-import.sh /path/to/atuin [/path/to/suv]

set -u

ATUIN="${1:?usage: $0 /path/to/atuin [/path/to/suv]}"
SUV="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/debug/suv}"
for bin in "$ATUIN" "$SUV"; do
    [ -x "$bin" ] || { echo "not executable: $bin"; exit 2; }
done
command -v sqlite3 >/dev/null || { echo "sqlite3 is required"; exit 2; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/home" "$WORK/data" "$WORK/config" "$WORK/work" "$WORK/suvhome"

echo "Atuin: $("$ATUIN" --version)"
echo "Suvadu: $("$SUV" --version)"
echo ""

failures=0
pass() { printf 'ok    %s\n' "$1"; }
fail() {
    printf 'FAIL  %s\n' "$1"
    printf '%s\n' "$2" | sed 's/^/      | /'
    failures=$((failures + 1))
}

atuin() {
    (cd "$WORK/work" && env -i HOME="$WORK/home" XDG_DATA_HOME="$WORK/data" \
        XDG_CONFIG_HOME="$WORK/config" PATH=/usr/bin:/bin \
        ATUIN_SESSION=0190f5b1c2d34e5f8a9b0c1d2e3f4a5b "$ATUIN" "$@")
}
suv() {
    env -i HOME="$WORK/suvhome" PATH=/usr/bin:/bin TERM=dumb "$SUV" "$@" 2>&1
}
# record EXIT DURATION_NS [history-start args...] — EXIT "none" never ends it.
record() {
    local exit="$1" duration="$2" id
    shift 2
    id=$(atuin history start "$@" 2>/dev/null)
    if [ "$exit" != "none" ] && [ -n "$id" ]; then
        atuin history end --exit "$exit" --duration "$duration" "$id" >/dev/null 2>&1
    fi
}

record 0 1000000000 -- echo hello
record 0 5000000 -- "$(printf 'for i in 1 2; do\n  echo $i\ndone')"
record 1 2000000 -- false
record 0 900000000 --author claude --author-kind agent --intent "run the test suite" \
    -- cargo test --offline
record 0 3000000 -- echo café Émile
record 0 1000000 -- echo delete-me-please
record none 0 -- sleep 1000
atuin search --delete "delete-me-please" >/dev/null 2>&1
printf 'ls -la\ngit status\ncd /tmp\n' >"$WORK/home/.bash_history"
atuin import bash >/dev/null 2>&1

DB="$WORK/data/atuin/history.db"
SDB="$WORK/suvhome/Library/Application Support/tech.appachi.suvadu/history.db"
[ -f "$SDB" ] || SDB="$WORK/suvhome/.local/share/suvadu/history.db"
live=$(sqlite3 "file:$DB?mode=ro" "select count(*) from history where deleted_at is null;")
echo "Atuin schema: $(sqlite3 "file:$DB?mode=ro" "select max(version) from _sqlx_migrations;")," \
    "$live live row(s)"
echo ""

digest() { shasum -a 256 "$DB" "$DB-wal" "$DB-shm" 2>/dev/null | awk '{print $1}' | tr '\n' ' '; }
before=$(digest)

out=$(suv import --from atuin-db "$DB")
[ -f "$SDB" ] || SDB=$(find "$WORK/suvhome" -name history.db -not -path '*/backups/*' | head -1)
stored=$(sqlite3 "$SDB" "select count(*) from entries;" 2>/dev/null)
if [[ "$out" == *"Imported: $live"* ]] && [ "$stored" = "$live" ]; then
    pass "every live row is imported ($live)"
else
    fail "every live row is imported ($live)" "$out"
fi

if [ "$(digest)" = "$before" ] && [[ "$out" == *"Source: unchanged"* ]]; then
    pass "the Atuin database, WAL and shm are unchanged"
else
    fail "the Atuin database, WAL and shm are unchanged" "$before -> $(digest)"
fi

agent=$(sqlite3 "$SDB" "select executor_type || ' ' || executor from entries where command = 'cargo test --offline';")
unfinished=$(sqlite3 "$SDB" "select coalesce(exit_code, 'unknown') from entries where command = 'sleep 1000';")
multiline=$(sqlite3 "$SDB" "select count(*) from entries where command = 'for i in 1 2; do' || char(10) || '  echo \$i' || char(10) || 'done';")
unicode=$(sqlite3 "$SDB" "select count(*) from entries where command = 'echo café Émile';")
deleted=$(sqlite3 "$SDB" "select count(*) from entries where command = 'echo delete-me-please';")
from_bash=$(sqlite3 "$SDB" "select count(*) from entries where command in ('ls -la', 'git status', 'cd /tmp') and exit_code is null;")
if [ "$agent" = "agent claude" ] && [ "$unfinished" = "unknown" ] && [ "$multiline" = 1 ] \
    && [ "$unicode" = 1 ] && [ "$deleted" = 0 ] && [ "$from_bash" = 3 ]; then
    pass "author, unknown exits, multi-line, Unicode and deletion carry over"
else
    fail "author, unknown exits, multi-line, Unicode and deletion carry over" \
        "agent=$agent unfinished=$unfinished multiline=$multiline unicode=$unicode deleted=$deleted from_bash=$from_bash"
fi

again=$(suv import --from atuin-db "$DB")
if [[ "$again" == *"Imported: 0"* ]] && [ "$(sqlite3 "$SDB" "select count(*) from entries;")" = "$live" ]; then
    pass "importing again adds nothing"
else
    fail "importing again adds nothing" "$again"
fi

backup=$(printf '%s\n' "$out" | grep -o '/[^"]*pre-atuin-import-[0-9-]*\.db' | head -1)
if [ -n "$backup" ] && cp "$backup" "$SDB" && rm -f "$SDB-wal" "$SDB-shm" \
    && [ "$(sqlite3 "$SDB" "select count(*) from entries;")" = 0 ]; then
    pass "the printed backup restores Suvadu to before the import"
else
    fail "the printed backup restores Suvadu to before the import" "backup=$backup"
fi

echo ""
if [ "$failures" -gt 0 ]; then
    echo "$failures Atuin import check(s) failed."
    exit 1
fi
echo "All Atuin import checks passed."
