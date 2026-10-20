//! The handoff template: what one agent has to hand another to continue a
//! session, and a scaffold of it filled from records suvadu actually
//! captured.
//!
//! Nothing here generates prose. Suvadu never invokes or charges a model:
//! every section that needs judgement is left as an explicit fill-in marker
//! for the connected agent, and every factual line cites the event or
//! command ID it came from, so the receiving agent can check it.

/// The sections of a handoff, in order, each with the question it answers.
/// One list so the blank template, the filled scaffold and the skill text
/// cannot describe different handoffs.
pub const SECTIONS: &[(&str, &str)] = &[
    (
        "Goal",
        "What the session was asked to achieve, in the requester's words.",
    ),
    (
        "Attempted steps",
        "What was actually run or tried, in order.",
    ),
    (
        "Relevant failures",
        "Attempts that failed, with the evidence that shows they failed.",
    ),
    (
        "Decisions",
        "Choices made and the reason, including approaches ruled out.",
    ),
    (
        "Changed-file evidence",
        "File changes actually observed. Never assert contents that were not captured.",
    ),
    (
        "Verification state",
        "What was verified, how, and what is still unverified.",
    ),
    (
        "Open questions",
        "What is still unknown or needs the user to decide.",
    ),
    (
        "Next actions",
        "The concrete next steps for whoever picks this up.",
    ),
    (
        "Source references",
        "Exact session, revision and event/command IDs backing every line above.",
    ),
];

/// Marker for a section only the connected agent can write. It is never
/// filled in locally — a plausible-looking sentence suvadu invented would be
/// indistinguishable from evidence.
pub const FILL_IN: &str = "<agent: fill in from the transcript — do not guess>";

/// One captured command as the handoff needs it.
#[derive(Debug, Clone)]
pub struct HandoffCommand {
    /// Stable evidence ID (`command-<n>`).
    pub id: String,
    pub command: String,
    pub exit_code: Option<i32>,
}

impl HandoffCommand {
    fn failed(&self) -> bool {
        self.exit_code.is_some_and(|code| code != 0)
    }

    fn outcome(&self) -> String {
        self.exit_code
            .map_or_else(|| "exit unknown".into(), |code| format!("exit {code}"))
    }
}

/// Everything the scaffold is built from. Deliberately plain data so both
/// the TUI and an MCP client can build one from what they already hold.
#[derive(Debug, Clone)]
pub struct HandoffSession {
    pub session_id: String,
    pub revision: String,
    pub agent: String,
    pub project: String,
    /// The first captured prompt: `(evidence id, text)`.
    pub goal: Option<(String, String)>,
    /// The newest captured agent answer: `(evidence id, text)`.
    pub latest_answer: Option<(String, String)>,
    pub commands: Vec<HandoffCommand>,
    /// Gaps suvadu recorded in this session's capture, verbatim.
    pub capture_known_missing: Vec<String>,
    /// Newest saved summary as `(id, basis label)`, when there is one.
    pub summary: Option<(String, String)>,
    /// What the session's git snapshots saw change, when it has two that
    /// can be compared.
    pub git_changes: Option<super::git_snapshot::ObservedChanges>,
}

/// Most changed files the handoff lists; the rest are counted.
const MAX_HANDOFF_FILES: usize = 50;

/// The observed half of "Changed-file evidence": what the git snapshots at
/// the session's first prompt and latest turn end differ by.
fn git_evidence(out: &mut String, changes: Option<&super::git_snapshot::ObservedChanges>) {
    use super::git_snapshot::Commits;
    let Some(changes) = changes else {
        bullet(
            out,
            "No git snapshots of this session can be compared, so no file change was \
             observed.",
        );
        return;
    };
    let short = |head: Option<&str>| {
        head.map_or_else(|| "none".to_string(), |h| h.chars().take(12).collect())
    };
    let commits = match &changes.commits {
        Commits::Unchanged => "HEAD did not move".to_string(),
        Commits::Read { ahead, .. } => format!(
            "HEAD {} → {}, {ahead} commit{}",
            short(changes.start.head.as_deref()),
            short(changes.end.head.as_deref()),
            if *ahead == 1 { "" } else { "s" }
        ),
        Commits::Unread(reason) => format!(
            "HEAD {} → {} (commits not read: {reason})",
            short(changes.start.head.as_deref()),
            short(changes.end.head.as_deref())
        ),
    };
    bullet(
        out,
        &format!(
            "Observed in git snapshots of `{}` at the first prompt and the latest turn end \
             [git-snapshot-start, git-snapshot-end]: {commits}; {} path(s) changed. Paths \
             only, no contents — changed in the repository, not necessarily by the agent.",
            changes.end.repo_root,
            changes.files.len()
        ),
    );
    if let Commits::Read { subjects, .. } = &changes.commits {
        for subject in subjects {
            bullet(out, &format!("Commit: `{}`", one_line(subject, 200)));
        }
    }
    for file in changes.files.iter().take(MAX_HANDOFF_FILES) {
        let from = file.from.as_deref().map_or_else(String::new, |from| {
            format!(" (from `{}`)", one_line(from, 200))
        });
        bullet(
            out,
            &format!(
                "`{}`{from} — {}",
                one_line(&file.path, 200),
                file.describe()
            ),
        );
    }
    if changes.files.len() > MAX_HANDOFF_FILES {
        bullet(
            out,
            &format!(
                "… {} more changed path(s) not listed.",
                changes.files.len() - MAX_HANDOFF_FILES
            ),
        );
    }
    if !changes.complete() {
        bullet(
            out,
            "A snapshot held more than 500 changed paths, so this list may miss some.",
        );
    }
}

/// Whether a command's *text* claims to modify files. This classifies what
/// was typed, never what happened on disk: suvadu captures no file contents
/// and no command output, so the scaffold reports these as claims to check.
fn claims_file_change(command: &str) -> bool {
    let trimmed = command.trim();
    let first = trimmed.split_whitespace().next().unwrap_or("");
    if matches!(
        first,
        "rm" | "rmdir"
            | "mv"
            | "cp"
            | "touch"
            | "mkdir"
            | "tee"
            | "sed"
            | "patch"
            | "vim"
            | "nvim"
            | "nano"
            | "vi"
    ) {
        return true;
    }
    if trimmed.contains(" > ") || trimmed.contains(" >> ") {
        return true;
    }
    first == "git"
        && matches!(
            trimmed.split_whitespace().nth(1).unwrap_or(""),
            "apply" | "checkout" | "restore" | "revert" | "merge" | "rebase" | "stash"
        )
}

/// Whether a command looks like a verification step (tests, build, lint).
fn looks_like_verification(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "test", "check", "lint", "build", "clippy", "fmt", "pytest", "vitest",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn bullet(out: &mut String, text: &str) {
    out.push_str("- ");
    out.push_str(text);
    out.push('\n');
}

fn section(out: &mut String, index: usize) {
    use std::fmt::Write;
    let (name, guidance) = SECTIONS[index];
    let _ = write!(out, "\n## {name}\n_{guidance}_\n");
}

/// Collapse captured text to one line so a multi-paragraph prompt does not
/// break the shape of the handoff.
fn one_line(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::util::truncate_str(&collapsed, max_chars, "…")
}

/// Build the handoff scaffold for one captured session: observed facts with
/// their evidence IDs, and explicit fill-in markers everywhere judgement is
/// required.
#[allow(clippy::too_many_lines)] // One linear document; splitting it hides the shape.
pub fn scaffold(session: &HandoffSession) -> String {
    let mut out = format!(
        "# Session handoff — {}\n\nSession `{}` · agent `{}` · project `{}` · revision `{}`\n\
         \nEvery line below either cites a captured record or is marked for you to fill in. \
         Suvadu generated no prose here.\n",
        session.project, session.session_id, session.agent, session.project, session.revision
    );

    section(&mut out, 0);
    match &session.goal {
        Some((id, text)) => bullet(
            &mut out,
            &format!("First captured prompt [{id}]: \"{}\"", one_line(text, 400)),
        ),
        None => bullet(
            &mut out,
            &format!("No prompt was captured for this session. {FILL_IN}"),
        ),
    }

    section(&mut out, 1);
    if session.commands.is_empty() {
        bullet(
            &mut out,
            "No shell commands were captured for this session.",
        );
    } else {
        for command in &session.commands {
            bullet(
                &mut out,
                &format!(
                    "`{}` — {} [{}]",
                    one_line(&command.command, 200),
                    command.outcome(),
                    command.id
                ),
            );
        }
    }

    section(&mut out, 2);
    let failures = session
        .commands
        .iter()
        .filter(|command| command.failed())
        .collect::<Vec<_>>();
    if failures.is_empty() {
        bullet(
            &mut out,
            "No captured command exited non-zero. That is not proof nothing failed — \
             command output is not captured, and a failure outside a recorded command \
             would not appear here.",
        );
    } else {
        for command in failures {
            bullet(
                &mut out,
                &format!(
                    "`{}` failed with {} [{}] — the error text itself was not captured.",
                    one_line(&command.command, 200),
                    command.outcome(),
                    command.id
                ),
            );
        }
    }

    section(&mut out, 3);
    bullet(&mut out, FILL_IN);

    section(&mut out, 4);
    let changing = session
        .commands
        .iter()
        .filter(|command| claims_file_change(&command.command))
        .collect::<Vec<_>>();
    bullet(
        &mut out,
        "Suvadu captures no file contents and no command output, so nothing below is a \
         verified diff.",
    );
    git_evidence(&mut out, session.git_changes.as_ref());
    if changing.is_empty() {
        bullet(
            &mut out,
            "No captured command claims to modify files. Any edits the agent made through \
             its own tools were not recorded.",
        );
    } else {
        for command in changing {
            bullet(
                &mut out,
                &format!(
                    "Command claiming a file change: `{}` — {} [{}]. Check the working tree \
                     rather than trusting this line.",
                    one_line(&command.command, 200),
                    command.outcome(),
                    command.id
                ),
            );
        }
    }

    section(&mut out, 5);
    let verifications = session
        .commands
        .iter()
        .filter(|command| looks_like_verification(&command.command))
        .collect::<Vec<_>>();
    if verifications.is_empty() {
        bullet(
            &mut out,
            "No captured command looks like a test, build or lint run — treat this session \
             as unverified.",
        );
    } else {
        for command in verifications {
            bullet(
                &mut out,
                &format!(
                    "`{}` — {} [{}]",
                    one_line(&command.command, 200),
                    command.outcome(),
                    command.id
                ),
            );
        }
    }
    if let Some((id, text)) = &session.latest_answer {
        bullet(
            &mut out,
            &format!(
                "Latest captured agent answer [{id}]: \"{}\" — the agent's own claim, not a \
                 verified result.",
                one_line(text, 400)
            ),
        );
    }
    for gap in &session.capture_known_missing {
        bullet(&mut out, &format!("Known capture gap: {gap}"));
    }

    section(&mut out, 6);
    bullet(&mut out, FILL_IN);

    section(&mut out, 7);
    bullet(&mut out, FILL_IN);

    section(&mut out, 8);
    bullet(
        &mut out,
        &format!(
            "Session `{}` at revision `{}` — re-read it with `get_agent_session` before \
             relying on any line above.",
            session.session_id, session.revision
        ),
    );
    let mut ids = Vec::new();
    if let Some((id, _)) = &session.goal {
        ids.push(id.clone());
    }
    if let Some((id, _)) = &session.latest_answer {
        ids.push(id.clone());
    }
    ids.extend(session.commands.iter().map(|command| command.id.clone()));
    if session.git_changes.is_some() {
        ids.push("git-snapshot-start".into());
        ids.push("git-snapshot-end".into());
    }
    if ids.is_empty() {
        bullet(&mut out, "No evidence records were available to cite.");
    } else {
        bullet(&mut out, &format!("Cited records: {}", ids.join(", ")));
    }
    match &session.summary {
        Some((id, basis)) => bullet(
            &mut out,
            &format!("Newest saved summary `{id}` — basis: {basis}."),
        ),
        None => bullet(
            &mut out,
            "No saved summary checkpoint exists for this session.",
        ),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(id: &str, text: &str, exit: Option<i32>) -> HandoffCommand {
        HandoffCommand {
            id: id.into(),
            command: text.into(),
            exit_code: exit,
        }
    }

    fn session() -> HandoffSession {
        HandoffSession {
            session_id: "codex-fixture".into(),
            revision: "e4-c2-2".into(),
            agent: "openai-codex".into(),
            project: "/work/project".into(),
            goal: Some(("codex-1".into(), "Fix the flaky parser test".into())),
            latest_answer: Some(("codex-9".into(), "The parser test passes now.".into())),
            commands: vec![
                command("command-1", "cargo test parser", Some(101)),
                command("command-2", "sed -i '' 's/a/b/' src/parser.rs", Some(0)),
                command("command-3", "cargo test parser", Some(0)),
            ],
            capture_known_missing: vec!["Records from a paused window were skipped".into()],
            summary: Some(("summary-1".into(), "CURRENT".into())),
            git_changes: None,
        }
    }

    #[test]
    fn scaffold_lists_files_its_git_snapshots_observed_changing() {
        let mut observed = session();
        observed.git_changes = Some(crate::ai_sessions::git_snapshot::tests::sample_changes());
        let text = scaffold(&observed);
        let evidence = text
            .split("## Changed-file evidence")
            .nth(1)
            .unwrap()
            .split("## Verification state")
            .next()
            .unwrap();
        assert!(evidence.contains("Observed in git snapshots"), "{evidence}");
        assert!(evidence.contains("1 commit"), "{evidence}");
        assert!(evidence.contains("`bbbbbbb fix the parser`"), "{evidence}");
        assert!(
            evidence.contains("`src/parser.rs` — changed by the commits"),
            "{evidence}"
        );
        assert!(evidence.contains("`notes.md` — untracked"), "{evidence}");
        assert!(
            evidence.contains("not necessarily by the agent"),
            "{evidence}"
        );
        assert!(
            text.contains("git-snapshot-start, git-snapshot-end"),
            "{text}"
        );

        let text = scaffold(&session());
        assert!(text.contains("No git snapshots"), "{text}");
    }

    /// The template is one list, and every filled scaffold renders all of
    /// it — a handoff missing "Verification state" or "Open questions" is
    /// the kind that gets acted on as if it were finished.
    #[test]
    fn every_required_section_is_in_the_template_and_in_a_filled_scaffold() {
        let text = scaffold(&session());
        for (name, _) in SECTIONS {
            assert!(text.contains(name), "scaffold is missing {name}:\n{text}");
        }
        for required in [
            "Goal",
            "Attempted steps",
            "Relevant failures",
            "Decisions",
            "Changed-file evidence",
            "Verification state",
            "Open questions",
            "Next actions",
            "Source references",
        ] {
            assert!(text.contains(required), "{required}");
        }
    }

    /// The receiving agent has to be able to see the failed attempt, the
    /// later success, and which record each came from.
    #[test]
    fn scaffold_shows_the_failed_attempt_the_later_success_and_cites_both() {
        let text = scaffold(&session());
        assert!(text.contains("`cargo test parser` failed with exit 101 [command-1]"));
        assert!(text.contains("`cargo test parser` — exit 0 [command-3]"));
        assert!(text.contains("Cited records: codex-1, codex-9, command-1, command-2, command-3"));
        assert!(text.contains("revision `e4-c2-2`"));
    }

    #[test]
    fn scaffold_never_presents_a_command_as_an_observed_file_change() {
        let text = scaffold(&session());
        assert!(text.contains("Suvadu captures no file contents"));
        assert!(text.contains("Command claiming a file change: `sed -i"));

        let mut no_edits = session();
        no_edits.commands = vec![command("command-1", "cargo test", Some(0))];
        let text = scaffold(&no_edits);
        assert!(text.contains("No captured command claims to modify files"));
    }

    /// Judgement is the connected agent's job; suvadu invents nothing.
    #[test]
    fn scaffold_marks_judgement_sections_for_the_agent_instead_of_inventing_them() {
        let text = scaffold(&session());
        for heading in ["## Decisions", "## Open questions", "## Next actions"] {
            let after = text.split(heading).nth(1).expect(heading);
            assert!(
                after.lines().take(4).any(|line| line.contains(FILL_IN)),
                "{heading} was not left for the agent:\n{after}"
            );
        }
    }

    #[test]
    fn scaffold_carries_capture_gaps_and_refuses_to_call_a_clean_run_proof() {
        let text = scaffold(&session());
        assert!(text.contains("Known capture gap: Records from a paused window were skipped"));

        let mut clean = session();
        clean.commands = vec![command("command-1", "cargo test", Some(0))];
        clean.capture_known_missing.clear();
        let text = scaffold(&clean);
        assert!(text.contains("That is not proof nothing failed"));
    }

    #[test]
    fn scaffold_says_so_when_there_is_nothing_captured_to_hand_over() {
        let empty = HandoffSession {
            goal: None,
            latest_answer: None,
            commands: Vec::new(),
            capture_known_missing: Vec::new(),
            summary: None,
            ..session()
        };
        let text = scaffold(&empty);
        assert!(text.contains("No prompt was captured"));
        assert!(text.contains("No shell commands were captured"));
        assert!(text.contains("treat this session as unverified"));
        assert!(text.contains("No saved summary checkpoint exists"));
    }
}
