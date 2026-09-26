//! Suvadu Home: a searchable map of what Suvadu can do, opened by `suv home`
//! (and, when chosen, by a bare `suv`). It is a discovery layer: every
//! feature it opens is the existing command, run as its own process.

// The catalog, model and reference are built before the controller that
// drives them; this allowance goes when the controller lands.
#[allow(dead_code)]
pub mod catalog;
#[allow(dead_code)]
pub mod model;
#[allow(dead_code)]
pub mod reference;
#[allow(dead_code)]
pub mod runner;
pub mod startup;
#[allow(dead_code)]
pub mod ui;

use std::io::IsTerminal;

use crate::config::{self, HomeConfig, HomeStartup};
use startup::{resolve_startup, StartupRoute};

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
        StartupRoute::Home => Err("Suvadu Home is not available in this build yet".into()),
    }
}

/// Where copied text goes. A trait so tests can stand in for the system
/// clipboard, which may be missing (no display, no clipboard service).
// Used by the controller; allowed until it lands.
#[allow(dead_code)]
pub trait Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

/// The system clipboard, opened on first use and kept open while Home runs:
/// on some Linux desktops copied text lasts only while its owner does.
// Used by the controller; allowed until it lands.
#[allow(dead_code)]
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
// Used by the controller; allowed until it lands.
#[allow(dead_code)]
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
