//! Suvadu Home: a searchable map of what Suvadu can do, opened by `suv home`
//! (and, when chosen, by a bare `suv`). It is a discovery layer: every
//! feature it opens is the existing command, run as its own process.

pub mod startup;

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
