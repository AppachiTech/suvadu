//! Telling people when a newer release exists.
//!
//! At most once a day, a user-facing command run at a terminal starts a
//! background `suv update-check`, which reads the same version file
//! `suv update` does and saves the answer. The command itself never waits
//! for the network. When the saved answer is newer than this build, the
//! next such command prints one line on stderr — at most once a day — with
//! the update command for how Suvadu was installed. Hooks, recall, pipes,
//! CI and `suv update` itself never check or print, and the whole thing is
//! off with `[update] check = false` or `SUVADU_NO_UPDATE_CHECK=1`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How often a check may run, and a notice be shown.
const DAY_SECS: u64 = 24 * 60 * 60;

/// Set to `1` (or `true`) to turn checking and notices off.
pub const OPT_OUT_ENV: &str = "SUVADU_NO_UPDATE_CHECK";

/// What was last learned, kept beside the history database.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// When a check was last started (Unix seconds).
    #[serde(default)]
    pub checked_at: Option<u64>,
    /// The newest release version a check has seen.
    #[serde(default)]
    pub latest: Option<String>,
    /// When the notice was last shown.
    #[serde(default)]
    pub notified_at: Option<u64>,
}

/// What this process's surroundings say about checking.
#[derive(Clone, Copy, Debug, Default)]
pub struct Environment {
    /// `SUVADU_NO_UPDATE_CHECK` is set.
    pub opted_out: bool,
    /// `CI` is set.
    pub ci: bool,
    /// stdout and stderr are both a terminal: someone is watching.
    pub interactive: bool,
}

/// Whether checking and notices are allowed at all, right now: turned on in
/// the config, not opted out through the environment, not in CI, and
/// someone watching.
pub const fn allowed(enabled: bool, env: Environment) -> bool {
    enabled && !env.opted_out && !env.ci && env.interactive
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    Homebrew,
    Cargo,
    Direct,
}

/// `latest` is a higher release than `current`, compared number by number.
/// Anything that is not a plain dotted version is never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(version: &str) -> Option<Vec<u64>> {
        let core = version.trim().trim_start_matches('v');
        let core = core.split(['-', '+']).next()?;
        core.split('.').map(|n| n.parse().ok()).collect()
    }
    match (parts(latest), parts(current)) {
        (Some(mut latest), Some(mut current)) => {
            let len = latest.len().max(current.len());
            latest.resize(len, 0);
            current.resize(len, 0);
            latest > current
        }
        _ => false,
    }
}

fn older_than_a_day(at: Option<u64>, now: u64) -> bool {
    // A time in the future (a clock set back) counts as recent.
    at.is_none_or(|at| at <= now && now - at >= DAY_SECS)
}

pub fn should_check(now: u64, state: &State, allowed: bool) -> bool {
    allowed && older_than_a_day(state.checked_at, now)
}

/// The newer version to mention, if one is known and the notice has not
/// been shown in the last day.
pub fn should_notify(now: u64, state: &State, current: &str, allowed: bool) -> Option<String> {
    let latest = state.latest.as_deref()?;
    (allowed && is_newer(latest, current) && older_than_a_day(state.notified_at, now))
        .then(|| latest.to_string())
}

pub const fn update_command(install: Install) -> &'static str {
    match install {
        Install::Homebrew => "brew upgrade suvadu",
        Install::Cargo => "cargo install suvadu",
        Install::Direct => "suv update",
    }
}

pub fn notice(latest: &str, current: &str, install: Install) -> String {
    format!(
        "suvadu {latest} is available (you have {current}) — update with: {}",
        update_command(install)
    )
}

/// Keep a fetched version only when it looks like one, so a captive portal
/// or an error page never becomes "the latest release".
pub fn record_latest(state: &mut State, fetched: Option<String>) {
    let Some(fetched) = fetched else {
        return;
    };
    let version = fetched.trim().trim_start_matches('v');
    if is_newer(version, "0") {
        state.latest = Some(version.to_string());
    }
}

pub fn load(path: &Path) -> State {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write `state` atomically, creating the directory if needed.
pub fn save(path: &Path, state: &State) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), serde_json::to_vec(state)?)?;
    tmp.persist(path).map_err(std::io::Error::from)?;
    Ok(())
}

pub fn state_path() -> Option<PathBuf> {
    Some(
        crate::util::project_dirs()?
            .data_dir()
            .join("update-check.json"),
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| {
        let v = v.trim().to_ascii_lowercase();
        !v.is_empty() && v != "0" && v != "false"
    })
}

fn install() -> Install {
    if crate::update::is_homebrew_install() {
        Install::Homebrew
    } else if crate::update::is_cargo_install() {
        Install::Cargo
    } else {
        Install::Direct
    }
}

/// [`allowed`] for this process: config, environment and, unless the
/// caller draws its own screen, the terminal.
fn allowed_here(enabled: bool, needs_terminal: bool) -> bool {
    let interactive =
        !needs_terminal || (std::io::stdout().is_terminal() && std::io::stderr().is_terminal());
    allowed(
        enabled,
        Environment {
            opted_out: env_flag(OPT_OUT_ENV),
            ci: env_flag("CI"),
            interactive,
        },
    )
}

/// Before a user-facing command: print the notice if one is due, and start
/// a background check if one is due. Never waits on the network, and any
/// failure here is silent — this must not get in the way of the command.
pub fn before_command(enabled: bool) {
    let allowed = allowed_here(enabled, true);
    if !allowed {
        return;
    }
    let Some(path) = state_path() else {
        return;
    };
    let mut state = load(&path);
    let now = now();
    let current = env!("CARGO_PKG_VERSION");
    let mut changed = if let Some(latest) = should_notify(now, &state, current, allowed) {
        eprintln!("{}", notice(&latest, current, install()));
        state.notified_at = Some(now);
        true
    } else {
        false
    };
    if should_check(now, &state, allowed) {
        // Recorded before starting, so a slow or failed check is not
        // restarted by every command in the meantime.
        state.checked_at = Some(now);
        changed = true;
        spawn_check();
    }
    if changed {
        let _ = save(&path, &state);
    }
}

/// Start `suv update-check` detached: no terminal, its own process group
/// (so Ctrl+C in the shell does not reach it), and reaped by a thread so a
/// long-running command never leaves it as a zombie.
fn spawn_check() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = std::process::Command::new(exe);
    command
        .arg("update-check")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// `suv update-check`: fetch the newest release version and save it.
pub fn run_check() {
    let Some(path) = state_path() else {
        return;
    };
    let fetched = crate::update::fetch_latest_version();
    let mut state = load(&path);
    record_latest(&mut state, fetched);
    let _ = save(&path, &state);
}

/// The notice Home shows from what is already saved; Home itself never
/// checks.
pub fn available_notice(enabled: bool) -> Option<String> {
    let state = load(&state_path()?);
    let latest = state.latest.as_deref()?;
    let current = env!("CARGO_PKG_VERSION");
    (allowed_here(enabled, false) && is_newer(latest, current))
        .then(|| format!("suvadu {latest} available: {}", update_command(install())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;
    const NOW: u64 = 1_800_000_000;

    const OPEN: bool = true;

    #[test]
    fn only_a_higher_release_counts_as_newer() {
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.0"));
        assert!(is_newer("0.6", "0.5.9"));
        assert!(!is_newer("0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert!(!is_newer("", "0.5.0"));
        assert!(!is_newer("not a version", "0.5.0"));
        assert!(!is_newer("<html>", "0.5.0"));
    }

    #[test]
    fn a_check_runs_at_most_once_a_day() {
        let never = State::default();
        assert!(should_check(NOW, &never, OPEN));
        let recent = State {
            checked_at: Some(NOW - DAY / 2),
            ..State::default()
        };
        assert!(!should_check(NOW, &recent, OPEN));
        let stale = State {
            checked_at: Some(NOW - DAY - 1),
            ..State::default()
        };
        assert!(should_check(NOW, &stale, OPEN));
        // A clock set back is not a reason to check in a loop.
        let future = State {
            checked_at: Some(NOW + DAY),
            ..State::default()
        };
        assert!(!should_check(NOW, &future, OPEN));
    }

    #[test]
    fn nothing_happens_when_turned_off_in_ci_or_without_a_terminal() {
        let state = State::default();
        let newer = State {
            latest: Some("9.0.0".into()),
            ..State::default()
        };
        let watched = Environment {
            interactive: true,
            ..Environment::default()
        };
        assert!(allowed(true, watched));
        for (enabled, env) in [
            (false, watched),
            (
                true,
                Environment {
                    opted_out: true,
                    ..watched
                },
            ),
            (
                true,
                Environment {
                    ci: true,
                    ..watched
                },
            ),
            (true, Environment::default()),
        ] {
            let open = allowed(enabled, env);
            assert!(!open, "{enabled} {env:?}");
            assert!(!should_check(NOW, &state, open));
            assert_eq!(should_notify(NOW, &newer, "0.5.0", open), None);
        }
    }

    #[test]
    fn a_newer_release_is_mentioned_at_most_once_a_day() {
        let seen = State {
            latest: Some("0.5.1".into()),
            ..State::default()
        };
        assert_eq!(
            should_notify(NOW, &seen, "0.5.0", OPEN),
            Some("0.5.1".to_string())
        );
        let told_recently = State {
            notified_at: Some(NOW - DAY / 2),
            ..seen.clone()
        };
        assert_eq!(should_notify(NOW, &told_recently, "0.5.0", OPEN), None);
        let told_yesterday = State {
            notified_at: Some(NOW - DAY - 1),
            ..seen.clone()
        };
        assert!(should_notify(NOW, &told_yesterday, "0.5.0", OPEN).is_some());
        assert_eq!(should_notify(NOW, &seen, "0.5.1", OPEN), None, "up to date");
        assert_eq!(should_notify(NOW, &State::default(), "0.5.0", OPEN), None);
    }

    #[test]
    fn the_notice_names_the_update_command_for_the_install() {
        assert_eq!(
            notice("0.5.1", "0.5.0", Install::Homebrew),
            "suvadu 0.5.1 is available (you have 0.5.0) — update with: brew upgrade suvadu"
        );
        assert!(notice("0.5.1", "0.5.0", Install::Cargo).ends_with("cargo install suvadu"));
        assert!(notice("0.5.1", "0.5.0", Install::Direct).ends_with("suv update"));
    }

    #[test]
    fn saved_state_round_trips_and_a_bad_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("update-check.json");
        assert_eq!(load(&path), State::default(), "missing file");
        let state = State {
            checked_at: Some(NOW),
            latest: Some("0.5.1".into()),
            notified_at: Some(NOW - 5),
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), State::default(), "corrupt file");
    }

    #[test]
    fn a_fetched_version_is_saved_only_when_it_looks_like_one() {
        let mut state = State::default();
        record_latest(&mut state, Some("0.5.1\n".into()));
        assert_eq!(state.latest.as_deref(), Some("0.5.1"));
        record_latest(&mut state, Some("<!doctype html>".into()));
        assert_eq!(
            state.latest.as_deref(),
            Some("0.5.1"),
            "kept the last good answer"
        );
        record_latest(&mut state, None);
        assert_eq!(
            state.latest.as_deref(),
            Some("0.5.1"),
            "a failed fetch changes nothing"
        );
    }
}
