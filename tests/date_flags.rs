//! Date flags on the command line: a date `suv` cannot read is an error that
//! names the accepted formats, never a filter silently dropped.
//!
//! Every test runs the real binary against a private HOME.

use std::process::{Command, Output, Stdio};

fn run(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_suv"))
        .args(args)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env_remove("SUVADU_PAUSED")
        .env_remove("SUVADU_LAUNCHED_BY_HOME")
        .env("SUVADU_NO_UPDATE_CHECK", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Every command that takes a date flag, with that flag set to `date`.
fn commands_with(date: &str) -> Vec<(String, Vec<String>)> {
    let with = |args: &[&str], flag: &str| {
        let mut args: Vec<String> = args.iter().map(ToString::to_string).collect();
        args.extend([flag.to_string(), date.to_string()]);
        (flag.to_string(), args)
    };
    vec![
        with(&["history"], "--after"),
        with(&["history"], "--before"),
        with(&["replay"], "--after"),
        with(&["replay"], "--before"),
        with(&["search"], "--after"),
        with(&["search"], "--before"),
        with(&["sessions", "--list"], "--after"),
        with(&["export"], "--after"),
        with(&["export"], "--before"),
        with(&["agent", "report"], "--after"),
        with(&["agent", "report"], "--before"),
        with(&["agent", "dashboard"], "--after"),
        with(&["agent", "prompts"], "--after"),
        with(&["delete", "zzz", "--dry-run"], "--before"),
    ]
}

#[test]
fn an_unreadable_date_flag_fails_and_names_the_accepted_formats() {
    let home = tempfile::tempdir().unwrap();
    for (flag, args) in commands_with("last tuesday") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(home.path(), &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`suv {}` accepted an unreadable date\n{stderr}",
            args.join(" ")
        );
        for expected in [
            flag.as_str(),
            "\"last tuesday\"",
            "YYYY-MM-DD",
            "N days ago",
        ] {
            assert!(
                stderr.contains(expected),
                "`suv {}` did not mention {expected}:\n{stderr}",
                args.join(" ")
            );
        }
    }
}

#[test]
fn a_readable_date_flag_still_works() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["history", "--after", "3 days ago", "--before", "today"],
        vec!["replay", "--after", "2024-01-15"],
        vec!["sessions", "--list", "--after", "yesterday"],
        vec!["export", "--after", "2024-01-15T10:00:00Z"],
        vec!["agent", "report", "--after", "7 days ago"],
    ] {
        let out = run(home.path(), &args);
        assert!(
            out.status.success(),
            "`suv {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
