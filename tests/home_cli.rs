//! How `suv` behaves when it is given no command, a help request or a bad
//! command, and what reaches stdout, stderr and the exit status.
//!
//! Scripts, pipes and command substitutions depend on these answers, so they
//! are pinned here: an interactive entry point must never change what a
//! non-interactive caller sees. Every run uses a private HOME with no
//! database, and nothing here reads or writes the user's own files.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_suv"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("NO_COLOR", "1")
            .current_dir(self.home.path())
            .stdin(Stdio::null());
        command
    }

    /// Run with stdin closed and stdout/stderr captured: the shape of a
    /// script, a pipe or `$(suv ...)`.
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Where the config file lives for this platform's directory scheme.
    /// Both candidates are written so the fixture holds on macOS and Linux.
    fn config_paths(&self) -> [PathBuf; 2] {
        [
            self.home
                .path()
                .join("Library/Application Support/tech.appachi.suvadu/config.toml"),
            self.home.path().join("config/suvadu/config.toml"),
        ]
    }

    fn write_config(&self, contents: &str) {
        for path in self.config_paths() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    /// Every file under the sandbox, so a test can tell whether a run
    /// created anything.
    fn files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                }
                out.push(path);
            }
        }
        let mut out = Vec::new();
        walk(self.home.path(), &mut out);
        out.sort();
        out
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("output is UTF-8")
}

#[test]
fn top_level_help_is_plain_and_successful() {
    let output = Sandbox::new().run(&["--help"]);
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    let help = text(&output.stdout);
    for section in ["Setup:", "Search & recall:", "Data:", "Shortcuts"] {
        assert!(help.contains(section), "missing {section}");
    }
    assert!(output.stderr.is_empty());
}

#[test]
fn help_subcommand_prints_the_same_overview_as_the_flag() {
    let sandbox = Sandbox::new();
    let flag = sandbox.run(&["--help"]);
    let subcommand = sandbox.run(&["help"]);
    assert!(subcommand.status.success());
    assert_eq!(subcommand.stdout, flag.stdout);
    assert!(subcommand.stderr.is_empty());
}

/// Without a command and without a terminal, `suv` has always printed the
/// overview to stderr and exited 2 (clap's missing-subcommand path). A
/// script that runs a bare `suv` must keep seeing exactly that.
#[test]
fn bare_suv_without_a_terminal_prints_the_overview_to_stderr_and_exits_2() {
    let sandbox = Sandbox::new();
    let bare = sandbox.run(&[]);
    let help = sandbox.run(&["--help"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(bare.stdout.is_empty(), "stdout: {:?}", text(&bare.stdout));
    assert_eq!(text(&bare.stderr), text(&help.stdout));
    assert!(!bare.stderr.contains(&0x1b), "no escape sequences");
}

#[test]
fn an_unknown_command_is_a_clap_error_not_a_search() {
    let output = Sandbox::new().run(&["frobnicate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("error: unrecognized subcommand 'frobnicate'"),
        "{stderr}"
    );
    assert!(stderr.contains("For more information, try '--help'."));
}

#[test]
fn version_flag_prints_name_and_version() {
    let output = Sandbox::new().run(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout),
        format!("suvadu {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn subcommand_help_is_plain_and_successful() {
    let output = Sandbox::new().run(&["search", "--help"]);
    assert!(output.status.success());
    assert!(!output.stdout.contains(&0x1b));
    assert!(text(&output.stdout).contains("Matching modes"));
}

/// Help is how someone recovers from a broken setup, so it must not depend
/// on a readable config or an existing database, and must not create either.
#[test]
fn help_needs_neither_a_valid_config_nor_a_database() {
    let sandbox = Sandbox::new();
    sandbox.write_config("this is = = not toml\n[home]\nstartup = \"sideways\"\n");
    let before = sandbox.files();
    for args in [
        &["--help"][..],
        &["help"][..],
        &["search", "--help"][..],
        &[][..],
    ] {
        let output = sandbox.run(args);
        let shown = if args.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        assert!(
            text(shown).contains("Usage:"),
            "`suv {}` printed no help",
            args.join(" ")
        );
    }
    assert_eq!(sandbox.files(), before, "help created or removed files");
}

#[test]
fn home_has_its_own_help() {
    let output = Sandbox::new().run(&["home", "--help"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let help = text(&output.stdout);
    assert!(help.contains("Usage: suv home"), "{help}");
    assert!(!output.stdout.contains(&0x1b));
}

/// `suv home` is interactive by definition. Asked for without a terminal it
/// says why in plain text and exits 2, rather than drawing into a pipe.
#[test]
fn home_without_a_terminal_is_refused_in_plain_text() {
    let sandbox = Sandbox::new();
    let before = sandbox.files();
    let output = sandbox.run(&["home"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.contains(&0x1b), "no escape sequences");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("needs an interactive terminal"), "{stderr}");
    assert!(stderr.contains("suv --help"), "{stderr}");
    assert_eq!(sandbox.files(), before, "a refused Home created files");
}

/// A saved preference for Home changes only what a person at a terminal
/// sees; a script running a bare `suv` keeps the old answer.
#[test]
fn a_home_preference_never_reaches_a_noninteractive_bare_suv() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["--help"]);
    for config in [
        "[home]\nstartup = \"home\"\n",
        "[home]\nstartup = \"sideways\"\n",
    ] {
        sandbox.write_config(config);
        let bare = sandbox.run(&[]);
        assert_eq!(bare.status.code(), Some(2), "config: {config}");
        assert!(bare.stdout.is_empty());
        assert_eq!(text(&bare.stderr), text(&help.stdout), "config: {config}");
    }
}

#[test]
fn a_dumb_terminal_never_opens_home() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command(&["home"])
        .env("TERM", "dumb")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!output.stderr.contains(&0x1b));
    assert!(text(&output.stderr).contains("needs an interactive terminal"));
}
