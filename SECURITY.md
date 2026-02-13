# Security Policy

## Reporting Vulnerabilities

If you discover a security vulnerability in Suvadu, please report it responsibly:

1. **Do NOT open a public GitHub issue**
2. Email: **madhu@appachi.tech**
3. Include: description, steps to reproduce, and potential impact

We aim to acknowledge reports within 48 hours and provide a fix timeline within 7 days.

## Security Design

### Data Storage

- All history is stored **locally** in a SQLite database (WAL mode)
- Default locations:
  - macOS: `~/Library/Application Support/suvadu/history.db`
  - Linux: `~/.local/share/suvadu/history.db`
- **No data is transmitted to external servers**
- **No telemetry or analytics** are collected

### Privacy Features

- Commands prefixed with a **space** are never recorded
- Configurable **exclusion patterns** (regex or substring) to ignore sensitive commands
- `suv delete` for bulk removal of entries matching a pattern
- `suv pause` for temporary recording suspension (per-shell)
- `suv disable` for global recording opt-out

### Self-Update

- Binary downloads are served over **HTTPS** from `downloads.appachi.tech`
- Downloads are verified with **SHA256 checksums**
- Update files are written to a unique temporary directory to prevent TOCTOU attacks
- Homebrew installs are handled through the official Homebrew tap

### Shell Hooks

- Shell hooks are installed via `eval "$(suv init zsh)"` or `eval "$(suv init bash)"`
- Hooks only capture: command text, working directory, exit code, timestamps, and executor type
- No environment variables, arguments to other programs, or file contents are recorded

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| Latest  | Yes                |
| < Latest | Best-effort       |
