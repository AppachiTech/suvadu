use crate::config;

#[allow(clippy::too_many_lines)]
pub fn get_zsh_hook(config: &config::Config) -> Result<String, Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy();

    let mut script = format!(
        r#"# Suvadu - Shell History Integration
# Add this to your ~/.zshrc:
# eval "$(suv init zsh)"

# Require Zsh 5.1+ for EPOCHREALTIME support
autoload -Uz is-at-least
if ! is-at-least 5.1 "$ZSH_VERSION"; then
    echo "[suvadu] Warning: Zsh 5.1+ required (you have $ZSH_VERSION). Shell hooks disabled." >&2
    return 0 2>/dev/null || true
fi

# Load zsh/datetime for $EPOCHREALTIME (not auto-loaded on all systems)
zmodload zsh/datetime

# Use add-zsh-hook for proper hook registration (cooperates with oh-my-zsh, plugins)
autoload -Uz add-zsh-hook

# Legacy alias support
alias suvadu="suv"

export SUVADU_SESSION_ID="${{SUVADU_SESSION_ID:-$(uuidgen)}}"
_SUVADU_START_TIME=0
_SUVADU_OFFSET=-1
_SUVADU_BIN="{bin_path}"

# Detect executor type and name
__suvadu_detect_executor() {{
    local executor_type="unknown"
    local executor="unknown"
    
    # CI/CD Detection
    if [[ -n "$CI" ]]; then
        executor_type="ci"
        if [[ -n "$GITHUB_ACTIONS" ]]; then
            executor="github-actions"
        elif [[ -n "$GITLAB_CI" ]]; then
            executor="gitlab"
        elif [[ -n "$CIRCLECI" ]]; then
            executor="circleci"
        else
            executor="ci-unknown"
        fi
    # AI Agent Detection
    elif [[ -n "$CLAUDE_CODE" ]] || [[ "$TERM_PROGRAM" == "claude" ]]; then
        executor_type="agent"
        executor="claude-code"
    elif [[ -n "$CODEX_CLI" ]]; then
        executor_type="agent"
        executor="openai-codex"
    elif [[ -n "$AIDER" ]] || [[ -n "$AIDER_SESSION" ]]; then
        executor_type="agent"
        executor="aider"
    elif [[ -n "$CONTINUE_SESSION" ]]; then
        executor_type="agent"
        executor="continue-dev"
    elif [[ -n "$COPILOT_WORKSPACE" ]]; then
        executor_type="agent"
        executor="copilot"
    # IDE Detection
    elif [[ -n "$WINDSURF" ]] || [[ -n "$CODEIUM" ]]; then
        executor_type="ide"
        executor="windsurf"
    elif [[ "$TERM_PROGRAM" == "vscode" ]] || [[ -n "$VSCODE_INJECTION" ]]; then
        executor_type="ide"
        executor="vscode"
    elif [[ -n "$CURSOR_INJECTION" ]] || [[ -n "$CURSOR_TRACE_ID" ]]; then
        executor_type="ide"
        executor="cursor"
    elif [[ -n "$ANTIGRAVITY_AGENT" ]]; then
        executor_type="ide"
        executor="antigravity"
    elif [[ -n "$INTELLIJ_ENVIRONMENT_READER" ]]; then
        executor_type="ide"
        executor="intellij"
    elif [[ -n "$PYCHARM_HOSTED" ]]; then
        executor_type="ide"
        executor="pycharm"
    # Human Detection
    elif [[ -t 0 ]]; then
        executor_type="human"
        executor="terminal"
    else
        # Non-interactive shell — likely agent or script
        executor_type="programmatic"
        executor="subprocess"
    fi
    
    echo "$executor_type:$executor"
}}

# Capture command start time (registered via add-zsh-hook)
_suvadu_preexec() {{
    _SUVADU_CMD="$1"
    _SUVADU_START_TIME=$(( ${{EPOCHREALTIME%.*}} * 1000 + ${{${{EPOCHREALTIME#*.}}:0:3}} ))
}}

# Capture command completion and save to DB (registered via add-zsh-hook)
_suvadu_precmd() {{
    local exit_code=$?
    local end_time=$(( ${{EPOCHREALTIME%.*}} * 1000 + ${{${{EPOCHREALTIME#*.}}:0:3}} ))

    # Reset history offset for new prompt
    _SUVADU_OFFSET=-1

    # Skip if no command captured (e.g. empty enter)
    if [ -z "$_SUVADU_CMD" ]; then
        return
    fi

    # Detect executor
    local executor_info=$(__suvadu_detect_executor)
    local executor_type="${{executor_info%%:*}}"
    local executor="${{executor_info##*:}}"

    # Synchronous add to avoid race conditions with immediate Up arrow
    $_SUVADU_BIN add \
        --session-id "$SUVADU_SESSION_ID" \
        --command "$_SUVADU_CMD" \
        --cwd "$PWD" \
        --exit-code "$exit_code" \
        --started-at "$_SUVADU_START_TIME" \
        --ended-at "$end_time" \
        --executor-type "$executor_type" \
        --executor "$executor" >/dev/null 2>&1

    # Clear captured command so empty enters don't duplicated
    _SUVADU_CMD=""
}}

# Register hooks properly (cooperates with oh-my-zsh, zsh-autosuggestions, etc.)
add-zsh-hook preexec _suvadu_preexec
add-zsh-hook precmd _suvadu_precmd

# Interactive Search Widget
_suvadu_search_widget() {{
    local selected tty_dev

    # Prefer $TTY (zsh sets this to the actual device, e.g. /dev/ttys003).
    # Fall back to /dev/tty for environments where $TTY is not readable (SSM sessions).
    if [[ -r "$TTY" ]]; then
        tty_dev="$TTY"
    elif [[ -r /dev/tty ]]; then
        tty_dev=/dev/tty
    else
        # Terminal not readable — fall back to default search
        zle .history-incremental-search-backward
        return
    fi

    # If suvadu is disabled (exit code 10), fallback to default search
    selected=$($_SUVADU_BIN search --query "$BUFFER" < "$tty_dev")
    local ret=$?

    if [ $ret -eq 10 ]; then
        zle .history-incremental-search-backward
        return
    fi

    if [ -n "$selected" ]; then
        BUFFER="$selected"
        CURSOR=$#BUFFER
    fi
    zle reset-prompt
}}

# Up Arrow Widget (Native Cycling)
_suvadu_up_arrow_widget() {{
    # If starting fresh, reset state
    if [[ "$LASTWIDGET" != suvadu-* && "$LASTWIDGET" != *autosuggest* && "$LASTWIDGET" != zle-line-* && "$LASTWIDGET" != *highlight* ]]; then
        _SUVADU_OFFSET=0
        _SUVADU_QUERY="$BUFFER"
    else
        # If we were at the prompt (-1), start at 0. Otherwise increment.
        if [[ $_SUVADU_OFFSET -eq -1 ]]; then
            _SUVADU_OFFSET=0
        else
            ((_SUVADU_OFFSET++))
        fi
    fi

    local result
    result=$($_SUVADU_BIN get --query "$_SUVADU_QUERY" --offset $_SUVADU_OFFSET --prefix --cwd "$PWD" 2>/dev/null)

    if [[ -n "$result" ]]; then
        BUFFER="$result"
        CURSOR=$#BUFFER
    else
        # No more results, stay at the last found or current
        [[ $_SUVADU_OFFSET -gt 0 ]] && ((_SUVADU_OFFSET--))
    fi
}}

# Down Arrow Widget (Native Cycling)
_suvadu_down_arrow_widget() {{
    # If not already in history mode, use standard down-line-or-history
    if [[ "$LASTWIDGET" != suvadu-* && "$LASTWIDGET" != *autosuggest* && "$LASTWIDGET" != zle-line-* && "$LASTWIDGET" != *highlight* ]]; then
        zle down-line-or-history
        return
    fi

    if [[ $_SUVADU_OFFSET -gt 0 ]]; then
        ((_SUVADU_OFFSET--))
        local result
        result=$($_SUVADU_BIN get --query "$_SUVADU_QUERY" --offset $_SUVADU_OFFSET --prefix --cwd "$PWD" 2>/dev/null)
        if [[ -n "$result" ]]; then
            BUFFER="$result"
            CURSOR=$#BUFFER
        fi
    elif [[ $_SUVADU_OFFSET -eq 0 ]]; then
        # Restore original input and set state to prompt (-1)
        BUFFER="$_SUVADU_QUERY"
        CURSOR=$#BUFFER
        _SUVADU_OFFSET=-1
    else
        # We are at prompt (-1), pass through
        zle down-line-or-history
    fi
}}


# Register widget and bind to Ctrl+R
zle -N suvadu-search _suvadu_search_widget
bindkey '^R' suvadu-search

# Register Up/Down
zle -N suvadu-up-arrow _suvadu_up_arrow_widget
zle -N suvadu-down-arrow _suvadu_down_arrow_widget

"#
    );

    if config.shell.enable_arrow_navigation {
        script.push_str(
            r"
bindkey '^[[A' suvadu-up-arrow
bindkey '^[OA' suvadu-up-arrow
bindkey '^[[B' suvadu-down-arrow
bindkey '^[OB' suvadu-down-arrow
",
        );
    }

    Ok(script)
}

#[allow(clippy::too_many_lines)]
pub fn get_bash_hook(config: &config::Config) -> Result<String, Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let bin_path = current_exe.to_string_lossy();

    let script = format!(
        r#"# Suvadu - Bash Shell History Integration
# Add this to your ~/.bashrc:
# eval "$(suv init bash)"

# Require Bash 4.0+ for associative arrays and EPOCHREALTIME
if [[ "${{BASH_VERSINFO[0]}}" -lt 4 ]]; then
    echo "[suvadu] Warning: Bash 4.0+ required (you have $BASH_VERSION). Shell hooks disabled." >&2
    return 0 2>/dev/null || true
fi

# Legacy alias support
alias suvadu="suv"

export SUVADU_SESSION_ID="${{SUVADU_SESSION_ID:-$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())' 2>/dev/null || echo "bash-$$-$RANDOM")}}"
_SUVADU_START_TIME=0
_SUVADU_CMD=""
_SUVADU_BIN="{bin_path}"

# Detect executor type and name
__suvadu_detect_executor() {{
    local executor_type="unknown"
    local executor="unknown"

    # CI/CD Detection
    if [[ -n "$CI" ]]; then
        executor_type="ci"
        if [[ -n "$GITHUB_ACTIONS" ]]; then
            executor="github-actions"
        elif [[ -n "$GITLAB_CI" ]]; then
            executor="gitlab"
        elif [[ -n "$CIRCLECI" ]]; then
            executor="circleci"
        else
            executor="ci-unknown"
        fi
    # AI Agent Detection
    elif [[ -n "$CLAUDE_CODE" ]] || [[ "$TERM_PROGRAM" == "claude" ]]; then
        executor_type="agent"
        executor="claude-code"
    elif [[ -n "$CODEX_CLI" ]]; then
        executor_type="agent"
        executor="openai-codex"
    elif [[ -n "$AIDER" ]] || [[ -n "$AIDER_SESSION" ]]; then
        executor_type="agent"
        executor="aider"
    # IDE Detection
    elif [[ "$TERM_PROGRAM" == "vscode" ]] || [[ -n "$VSCODE_INJECTION" ]]; then
        executor_type="ide"
        executor="vscode"
    elif [[ -n "$CURSOR_INJECTION" ]] || [[ -n "$CURSOR_TRACE_ID" ]]; then
        executor_type="ide"
        executor="cursor"
    elif [[ -n "$ANTIGRAVITY_AGENT" ]]; then
        executor_type="ide"
        executor="antigravity"
    # Human Detection
    elif [[ -t 0 ]]; then
        executor_type="human"
        executor="terminal"
    else
        executor_type="programmatic"
        executor="subprocess"
    fi

    echo "$executor_type:$executor"
}}

# Get current time in milliseconds (Bash 5+ has EPOCHREALTIME, fallback to date)
__suvadu_time_ms() {{
    if [[ -n "$EPOCHREALTIME" ]]; then
        local secs="${{EPOCHREALTIME%%.*}}"
        local frac="${{EPOCHREALTIME##*.}}"
        echo "$(( secs * 1000 + ${{frac:0:3}} ))"
    else
        # Fallback: date +%s gives seconds, multiply by 1000
        echo "$(( $(date +%s) * 1000 ))"
    fi
}}

# Capture command via DEBUG trap (preexec equivalent)
__suvadu_preexec() {{
    # Don't capture PROMPT_COMMAND itself or empty commands
    if [[ "$BASH_COMMAND" == "$PROMPT_COMMAND" ]] || [[ -z "$BASH_COMMAND" ]]; then
        return
    fi
    # Don't capture our own functions
    if [[ "$BASH_COMMAND" == __suvadu_* ]]; then
        return
    fi
    # Don't capture during tab completion (COMP_LINE is set by readline)
    if [[ -n "${{COMP_LINE+x}}" ]]; then
        return
    fi

    _SUVADU_CMD="$BASH_COMMAND"
    _SUVADU_START_TIME=$(__suvadu_time_ms)
}}

# Capture command completion (precmd equivalent via PROMPT_COMMAND)
__suvadu_precmd() {{
    local exit_code=$?
    local end_time=$(__suvadu_time_ms)

    # Skip if no command was captured
    if [[ -z "$_SUVADU_CMD" ]]; then
        return
    fi

    # Detect executor
    local executor_info=$(__suvadu_detect_executor)
    local executor_type="${{executor_info%%:*}}"
    local executor="${{executor_info##*:}}"

    "$_SUVADU_BIN" add \
        --session-id "$SUVADU_SESSION_ID" \
        --command "$_SUVADU_CMD" \
        --cwd "$PWD" \
        --exit-code "$exit_code" \
        --started-at "$_SUVADU_START_TIME" \
        --ended-at "$end_time" \
        --executor-type "$executor_type" \
        --executor "$executor" >/dev/null 2>&1

    _SUVADU_CMD=""
}}

# Interactive Search Widget (Ctrl+R replacement)
__suvadu_search_widget() {{
    local selected tty_dev

    # Find a readable TTY for the TUI.
    # Bash has no $TTY built-in, so check /dev/tty first, then $(tty).
    if [[ -r /dev/tty ]]; then
        tty_dev=/dev/tty
    else
        tty_dev=$(tty 2>/dev/null)
        if [[ ! -r "$tty_dev" ]]; then
            # No readable terminal — skip (regular readline search still works)
            return
        fi
    fi

    selected=$("$_SUVADU_BIN" search --query "$READLINE_LINE" < "$tty_dev" 2>"$tty_dev")
    local ret=$?

    if [[ $ret -eq 10 ]]; then
        # Disabled: fall back to default reverse search
        return
    fi

    if [[ -n "$selected" ]]; then
        READLINE_LINE="$selected"
        READLINE_POINT=${{#READLINE_LINE}}
    fi
}}

# Install hooks
trap '__suvadu_preexec' DEBUG

# Append to PROMPT_COMMAND (don't overwrite existing)
if [[ -z "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__suvadu_precmd"
elif [[ "$PROMPT_COMMAND" != *"__suvadu_precmd"* ]]; then
    PROMPT_COMMAND="__suvadu_precmd;$PROMPT_COMMAND"
fi

# Bind Ctrl+R to suvadu search
bind -x '"\C-r": __suvadu_search_widget'

"#
    );

    // Note: Bash doesn't have zsh's zle widgets for arrow key override,
    // so arrow-based history navigation is not supported in Bash.
    let _ = config; // Config reserved for future Bash-specific settings

    Ok(script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zsh_hook_generation() {
        let config = config::Config::default();
        let hook = get_zsh_hook(&config).expect("Failed to generate zsh hook");

        // Verify essential components are present
        assert!(hook.contains("# Suvadu - Shell History Integration"));
        assert!(hook.contains("alias suvadu=\"suv\""));
        assert!(hook.contains("export SUVADU_SESSION_ID"));
        assert!(hook.contains("__suvadu_detect_executor"));
        assert!(hook.contains("_suvadu_preexec"));
        assert!(hook.contains("_suvadu_precmd"));
        assert!(hook.contains("add-zsh-hook preexec"));
        assert!(hook.contains("add-zsh-hook precmd"));
        assert!(hook.contains("_suvadu_search_widget"));
        assert!(hook.contains("_suvadu_up_arrow_widget"));
        assert!(hook.contains("_suvadu_down_arrow_widget"));
        assert!(hook.contains("zle -N suvadu-search"));
        assert!(hook.contains("bindkey '^R' suvadu-search"));

        // Verify executor detection logic
        assert!(hook.contains("ANTIGRAVITY_AGENT"));
        assert!(hook.contains("GITHUB_ACTIONS"));
        assert!(hook.contains("executor_type=\"ci\""));
        assert!(hook.contains("executor_type=\"ide\""));
        assert!(hook.contains("executor_type=\"agent\""));

        // Verify arrow navigation binding when enabled
        if config.shell.enable_arrow_navigation {
            assert!(hook.contains("bindkey '^[[A' suvadu-up-arrow"));
            assert!(hook.contains("bindkey '^[[B' suvadu-down-arrow"));
        }
    }

    #[test]
    fn test_bash_hook_generation() {
        let config = config::Config::default();
        let hook = get_bash_hook(&config).expect("Failed to generate bash hook");

        // Verify essential Bash-specific components
        assert!(hook.contains("# Suvadu - Bash Shell History Integration"));
        assert!(hook.contains("alias suvadu=\"suv\""));
        assert!(hook.contains("export SUVADU_SESSION_ID"));
        assert!(hook.contains("__suvadu_detect_executor"));
        assert!(hook.contains("__suvadu_preexec"));
        assert!(hook.contains("__suvadu_precmd"));
        assert!(hook.contains("__suvadu_search_widget"));
        assert!(hook.contains("trap '__suvadu_preexec' DEBUG"));
        assert!(hook.contains("PROMPT_COMMAND"));
        assert!(hook.contains("bind -x"));
        assert!(hook.contains("BASH_VERSINFO"));
    }

    #[test]
    fn test_zsh_hook_binary_path() {
        let config = config::Config::default();
        let hook = get_zsh_hook(&config).expect("Failed to generate zsh hook");

        // Verify the binary path is set correctly
        assert!(hook.contains("_SUVADU_BIN=\""));
    }

    #[test]
    fn test_zsh_hook_contains_session_id() {
        let config = config::Config::default();
        let hook = get_zsh_hook(&config).expect("Failed to generate zsh hook");
        // Verify SUVADU_SESSION_ID export is present
        assert!(
            hook.contains("export SUVADU_SESSION_ID"),
            "Zsh hook must export SUVADU_SESSION_ID"
        );
    }

    #[test]
    fn test_bash_hook_contains_executor_detection() {
        let config = config::Config::default();
        let hook = get_bash_hook(&config).expect("Failed to generate bash hook");
        // Verify executor detection function is present
        assert!(
            hook.contains("__suvadu_detect_executor"),
            "Bash hook must contain executor detection function"
        );
        // Verify CI detection
        assert!(hook.contains("GITHUB_ACTIONS"));
        // Verify agent detection
        assert!(hook.contains("CLAUDE_CODE"));
        // Verify IDE detection
        assert!(hook.contains("CURSOR_INJECTION"));
    }

    #[test]
    fn test_zsh_hook_arrow_nav_disabled() {
        let mut config = config::Config::default();
        config.shell.enable_arrow_navigation = false;

        let hook = get_zsh_hook(&config).expect("Failed to generate zsh hook");

        // Arrow key bindings should NOT be present when disabled
        assert!(
            !hook.contains("bindkey '^[[A' suvadu-up-arrow"),
            "Arrow up binding should not be present when arrow nav is disabled"
        );
        assert!(
            !hook.contains("bindkey '^[[B' suvadu-down-arrow"),
            "Arrow down binding should not be present when arrow nav is disabled"
        );

        // But Ctrl+R should still be bound
        assert!(hook.contains("bindkey '^R' suvadu-search"));
    }

    #[test]
    fn test_zsh_hook_arrow_nav_enabled() {
        let mut config = config::Config::default();
        config.shell.enable_arrow_navigation = true;

        let hook = get_zsh_hook(&config).expect("Failed to generate zsh hook");

        // Arrow key bindings SHOULD be present when enabled
        assert!(
            hook.contains("bindkey '^[[A' suvadu-up-arrow"),
            "Arrow up binding should be present when arrow nav is enabled"
        );
        assert!(
            hook.contains("bindkey '^[OA' suvadu-up-arrow"),
            "Alternate arrow up binding should be present"
        );
        assert!(
            hook.contains("bindkey '^[[B' suvadu-down-arrow"),
            "Arrow down binding should be present when arrow nav is enabled"
        );
        assert!(
            hook.contains("bindkey '^[OB' suvadu-down-arrow"),
            "Alternate arrow down binding should be present"
        );
    }
}
