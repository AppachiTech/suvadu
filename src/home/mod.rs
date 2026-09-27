//! Suvadu Home: a searchable map of what Suvadu can do, opened by `suv home`
//! (and, when chosen, by a bare `suv`). It is a discovery layer: every
//! feature it opens is the existing command, run as its own process.

pub mod catalog;
pub mod model;
pub mod reference;
pub mod runner;
pub mod startup;
pub mod ui;

use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::config::{self, Config, ConfigError, HomeConfig, HomeStartup};
use catalog::{LaunchMode, LaunchRequest};
use model::{HomeAction, HomeState, HomeStatus, Outcome};
use startup::{resolve_startup, StartupRoute};

/// Exit status for Ctrl+C, as a shell reports an interrupted program.
const INTERRUPTED: i32 = 130;

/// A screen that closes sooner than this printed something and left, rather
/// than being used and closed: its words stay on screen until a key.
const QUICK_EXIT: Duration = Duration::from_secs(1);

/// Entry point for a bare `suv` (`explicit_home == false`) and `suv home`.
///
/// Returns only for routes that finish normally; the refusal and legacy
/// routes exit with status 2 themselves, exactly as before Home existed.
pub fn start(explicit_home: bool) -> Result<(), Box<dyn std::error::Error>> {
    let interactive = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");

    // The config is read only once a terminal can use the answer, so a pipe
    // or a script never depends on it.
    let loaded = (interactive && !dumb).then(config::read_global_config);
    let preference = match &loaded {
        Some(Ok(Some(config))) => config.home.effective_startup(),
        Some(Err(_)) => HomeStartup::Help,
        Some(Ok(None)) | None => HomeConfig::DEFAULT_STARTUP,
    };

    match resolve_startup(explicit_home, interactive, dumb, preference) {
        StartupRoute::LegacyMissingCommand => crate::cli::exit_missing_command(),
        StartupRoute::RejectHome => {
            eprintln!(
                "suv home needs an interactive terminal: stdin, stdout and stderr must all be a \
                 terminal, and TERM must not be \"dumb\".\nRun `suv --help` for the command \
                 overview, or `suv <command> --help` for one command."
            );
            std::process::exit(2);
        }
        StartupRoute::ClassicHelp => {
            if let Some(Err(e)) = &loaded {
                eprintln!(
                    "suv: could not read your config ({}); showing the command overview. \
                     The file was not changed.",
                    concise_error(&e.to_string())
                );
            }
            print!("{}", crate::cli::overview());
            Ok(())
        }
        StartupRoute::Home => match run(loaded.unwrap_or(Ok(None))) {
            Err(HomeError::Setup(e)) => match after_setup_failure(explicit_home, &e.to_string()) {
                SetupFallback::Overview(warning) => {
                    eprintln!("{warning}");
                    print!("{}", crate::cli::overview());
                    Ok(())
                }
                SetupFallback::Error(message) => Err(message.into()),
            },
            Err(HomeError::Other(e)) => Err(e),
            Ok(()) => Ok(()),
        },
    }
}

/// Why Home stopped: it could not take the terminal at all, or something
/// failed once it was running.
enum HomeError {
    Setup(Box<dyn std::error::Error>),
    Other(Box<dyn std::error::Error>),
}

impl<E: Into<Box<dyn std::error::Error>>> From<E> for HomeError {
    fn from(e: E) -> Self {
        Self::Other(e.into())
    }
}

enum SetupFallback {
    /// Print this warning, then the overview, and exit successfully.
    Overview(String),
    Error(String),
}

/// When the terminal cannot be set up for Home, a bare suv still helps by
/// showing the overview; an explicit suv home reports what failed.
fn after_setup_failure(explicit_home: bool, error: &str) -> SetupFallback {
    if explicit_home {
        SetupFallback::Error(format!("suv home could not set up the terminal: {error}"))
    } else {
        SetupFallback::Overview(format!(
            "suv: could not set up the terminal for Home ({error}); showing the command overview."
        ))
    }
}

/// Home's loop: draw and handle keys with the terminal, give the terminal
/// up entirely while a screen or picker it opened runs, then take it back
/// exactly where the person left off.
fn run(loaded: Result<Option<Config>, ConfigError>) -> Result<(), HomeError> {
    let mut state = HomeState::new();
    state.no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    apply_config(&mut state, loaded);
    // The very program running now, so a development build opens its own
    // features rather than whichever suv is first on PATH.
    let executable = std::env::current_exe()?;
    let mut clipboard = SystemClipboard::default();
    let mut first = true;
    loop {
        let action = {
            let _guard = match crate::util::TerminalGuardStderr::new() {
                Ok(guard) => guard,
                // Home never started: the caller can still fall back.
                Err(e) if first => return Err(HomeError::Setup(e)),
                Err(e) => return Err(HomeError::Other(e)),
            };
            first = false;
            // Buffered, so a frame reaches the terminal in a few writes
            // rather than one per changed cell; ratatui flushes each frame.
            let backend = ratatui::backend::CrosstermBackend::new(
                std::io::BufWriter::with_capacity(64 * 1024, std::io::stderr()),
            );
            let mut terminal = ratatui::Terminal::new(backend)?;
            interact(&mut terminal, &mut state, &mut clipboard, &executable)?
            // The guard drops here: the terminal is restored before any
            // child starts.
        };
        match action {
            HomeAction::Launch(request) => {
                let result = runner::run_child(&executable, &request);
                if let Ok(result) = &result {
                    if needs_a_pause(&request, result) && wait_for_key()? == HomeAction::Interrupt {
                        std::process::exit(INTERRUPTED);
                    }
                }
                show_result(&mut state, &request, result);
                // Settings may have changed the theme, icons or recording.
                apply_config(&mut state, config::read_global_config());
            }
            HomeAction::Interrupt => std::process::exit(INTERRUPTED),
            _ => return Ok(()),
        }
    }
}

/// Draw and handle events until Home must hand the terminal to a child or
/// exit. Copies and short reports are handled here, without leaving.
fn interact(
    terminal: &mut ratatui::Terminal<
        ratatui::backend::CrosstermBackend<std::io::BufWriter<std::io::Stderr>>,
    >,
    state: &mut HomeState,
    clipboard: &mut dyn Clipboard,
    executable: &Path,
) -> Result<HomeAction, Box<dyn std::error::Error>> {
    loop {
        let size = terminal.size()?;
        state.set_viewport(size.width, size.height);
        terminal.draw(|frame| ui::draw(frame, state))?;
        match state.on_event(event::read()?) {
            HomeAction::None | HomeAction::Redraw => {}
            HomeAction::Copy(text) => copy(state, clipboard, &text),
            HomeAction::Launch(request) => {
                // Only a request the catalog itself would make is ever run.
                if catalog::launch_request(request.feature).as_ref() != Some(&request) {
                    continue;
                }
                if request.mode != LaunchMode::Report {
                    return Ok(HomeAction::Launch(request));
                }
                // A report never touches the terminal, so Home stays up.
                let command = catalog::feature(request.feature).map_or("suv", |f| f.command);
                state.set_notice(format!("Running {command}…"), false);
                terminal.draw(|frame| ui::draw(frame, state))?;
                let result = runner::run_child(executable, &request);
                state.clear_notice();
                show_result(state, &request, result);
            }
            action => return Ok(action),
        }
    }
}

fn show_result(
    state: &mut HomeState,
    request: &LaunchRequest,
    result: std::io::Result<runner::LaunchResult>,
) {
    let outcome = match result {
        Ok(result) => runner::outcome(request, &result),
        Err(e) => {
            let command = catalog::feature(request.feature).map_or("suv", |f| f.command);
            Some(Outcome::Failed {
                message: format!("Could not start {command}: {e}"),
                retry: true,
            })
        }
    };
    if let Some(outcome) = outcome {
        state.show_outcome(request.feature, outcome);
    }
}

/// Whether a child's last words need a moment on screen: it failed, or it
/// closed too soon to have been used — most likely printing why — and
/// returned nothing Home will show instead.
fn needs_a_pause(request: &LaunchRequest, result: &runner::LaunchResult) -> bool {
    let answered = request.mode == LaunchMode::Selection && !result.stdout.is_empty();
    result.code != Some(0) || (result.elapsed < QUICK_EXIT && !answered)
}

/// On the normal screen, under whatever the child printed: wait for a key.
fn wait_for_key() -> std::io::Result<HomeAction> {
    use crate::util::TerminalStep;
    eprint!("\r\nPress any key to return to Suvadu Home.");
    crate::util::apply_all(&[TerminalStep::EnableRawMode], |_| {
        crossterm::terminal::enable_raw_mode()
    })?;
    let pressed = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => break Ok(key),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprint!("\r\n");
    let key = pressed?;
    let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'));
    Ok(if ctrl_c {
        HomeAction::Interrupt
    } else {
        HomeAction::Redraw
    })
}

/// Take the preferences and recording facts Home shows from the global
/// config. An unreadable config is reported, never repaired.
fn apply_config(state: &mut HomeState, loaded: Result<Option<Config>, ConfigError>) {
    let paused = config::is_paused();
    match loaded {
        Ok(found) => {
            let config = found.unwrap_or_default();
            crate::theme::init_theme(config.theme);
            state.icons = config.home.icons;
            state.status = HomeStatus {
                recording: Some(config.enabled),
                paused,
                warning: None,
                // Only what an earlier check saved: Home never checks.
                update: crate::update_check::available_notice(config.update.check),
            };
        }
        Err(e) => {
            state.status = HomeStatus {
                recording: None,
                paused,
                warning: Some(format!(
                    "{} (left unchanged; defaults in use)",
                    concise_error(&e.to_string())
                )),
                update: None,
            };
        }
    }
}

/// Where copied text goes. A trait so tests can stand in for the system
/// clipboard, which may be missing (no display, no clipboard service).
pub trait Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

/// The system clipboard, opened on first use and kept open while Home runs:
/// on some Linux desktops copied text lasts only while its owner does.
#[derive(Default)]
pub struct SystemClipboard {
    inner: Option<arboard::Clipboard>,
}

impl Clipboard for SystemClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        if self.inner.is_none() {
            self.inner = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        self.inner
            .as_mut()
            .ok_or_else(|| "no clipboard".to_string())?
            .set_text(text)
            .map_err(|e| e.to_string())
    }
}

/// Copy `text` and tell the person what happened — success only once the
/// clipboard has accepted it. Copying never runs anything.
fn copy(state: &mut model::HomeState, clipboard: &mut dyn Clipboard, text: &str) {
    let result = clipboard.set_text(text);
    state.copied(text, &result);
}

/// One line from a config error, which for TOML is a multi-line snippet:
/// where it is, then what is wrong (`line 2, column 11: unknown variant …`).
fn concise_error(error: &str) -> String {
    let lines: Vec<&str> = error
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let reason = lines.last().copied().unwrap_or("unknown error");
    let location = lines
        .first()
        .and_then(|first| first.find("line ").map(|at| &first[at..]))
        .filter(|location| lines.len() > 1 && *location != reason);
    location.map_or_else(
        || reason.to_string(),
        |location| format!("{location}: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::HomeState;

    struct Recording(Vec<String>);
    impl Clipboard for Recording {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            self.0.push(text.to_string());
            Ok(())
        }
    }

    struct Broken;
    impl Clipboard for Broken {
        fn set_text(&mut self, _text: &str) -> Result<(), String> {
            Err("no clipboard service".into())
        }
    }

    #[test]
    fn a_copy_sends_the_exact_text_and_then_says_so() {
        let mut state = HomeState::new();
        let mut clipboard = Recording(Vec::new());
        let text = "  printf 'a\\nb'  \nsecond line";
        copy(&mut state, &mut clipboard, text);
        assert_eq!(clipboard.0, [text]);
        assert!(state.notice().unwrap().starts_with("Copied"));
        assert!(!state.notice_is_error());
    }

    #[test]
    fn a_failed_copy_never_claims_success() {
        let mut state = HomeState::new();
        copy(&mut state, &mut Broken, "suv backup");
        let notice = state.notice().unwrap();
        assert!(state.notice_is_error());
        assert!(!notice.contains("Copied"), "{notice}");
        assert!(notice.contains("no clipboard service"), "{notice}");
    }

    /// A bare suv that cannot set up the terminal still helps: it shows the
    /// overview with the reason. suv home says plainly what failed.
    #[test]
    fn a_terminal_that_cannot_be_set_up_falls_back_to_the_overview() {
        match after_setup_failure(false, "Operation not supported") {
            SetupFallback::Overview(warning) => {
                assert!(warning.contains("Operation not supported"), "{warning}");
                assert!(warning.contains("overview"), "{warning}");
            }
            SetupFallback::Error(e) => panic!("bare suv gave an error: {e}"),
        }
        match after_setup_failure(true, "Operation not supported") {
            SetupFallback::Error(e) => {
                assert!(e.contains("suv home could not set up the terminal"), "{e}");
                assert!(e.contains("Operation not supported"), "{e}");
            }
            SetupFallback::Overview(_) => panic!("suv home should report the failure"),
        }
    }

    #[test]
    fn a_toml_error_becomes_one_line_with_its_location() {
        let error = "TOML deserialization error: TOML parse error at line 2, column 11\n  |\n2 | \
                     startup = \"sideways\"\n  |           ^^^^^^^^^^\nunknown variant `sideways`, \
                     expected `home` or `help`\n";
        assert_eq!(
            concise_error(error),
            "line 2, column 11: unknown variant `sideways`, expected `home` or `help`"
        );
        assert_eq!(concise_error("permission denied"), "permission denied");
        assert_eq!(concise_error(""), "unknown error");
    }
}
