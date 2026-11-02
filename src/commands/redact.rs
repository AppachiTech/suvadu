//! `suv redact`: mask secrets in what is already stored.
//!
//! Redaction runs before a row is written, so it never reached text recorded
//! before a rule existed. This applies today's rules to the database, the
//! backups beside it and the cached prompts.
//!
//! It cannot be undone and masks by pattern, so it shows its work first:
//! A rule needs a key name to go on, and a secret pasted into a prompt has
//! none, so the scan runs twice: what the rules mask in the first pass is
//! masked wherever else it appears in the second.
//!
//! `--dry-run` lists every distinct change, a real run asks, and the live
//! database is copied before it is touched. That copy is the way back, so no
//! later run masks it, and it is the one file left holding the old text, so
//! every run names it until it is deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use regex::{Regex, RegexBuilder};

use crate::config::{self, Config};
use crate::repository::{KeptSecrets, Mask, RedactionPlan, Repository};
use crate::util;

/// The shortest value that is masked beyond where a rule found it. Below this
/// it could be a word: `password=hunter2` says nothing about `grep hunter2`.
const MIN_SPREAD_LEN: usize = 16;

/// Whether a value a rule masked is the secret itself, and so a secret
/// wherever it appears. A reference to one (`${DB_PASSWORD}`, `$(pass show x)`,
/// `<your-token>`) is not: it means the same harmless thing everywhere else.
fn is_a_literal_secret(value: &str) -> bool {
    value.len() >= MIN_SPREAD_LEN
        && !value.contains(crate::redact::REDACTED)
        && value
            .chars()
            .all(|c| c.is_ascii_graphic() && !"$`{}()<>[]\"'\\|;,".contains(c))
}

/// Which rules apply to a row: the global ones plus those of the project it
/// was recorded in, as when it was written. `redaction.enabled` is not
/// consulted: asking for this is asking for redaction.
struct Rules {
    global: Config,
    global_patterns: Rc<Vec<String>>,
    by_dir: HashMap<String, Rc<Vec<String>>>,
    /// Directories whose `.suvadu.toml` could not be read. Their rows get the
    /// global rules, and a later run can do better once the file is fixed.
    unreadable_dirs: BTreeSet<String>,
    /// Literal secrets the rules have masked so far.
    known: BTreeSet<String>,
    /// `known`, compiled by [`Self::spread_known_values`] for the next pass.
    everywhere: Option<Regex>,
}

impl Rules {
    fn new(global: Config) -> Self {
        let global_patterns = Rc::new(global.redaction.extra_patterns.clone());
        Self {
            global,
            global_patterns,
            by_dir: HashMap::new(),
            unreadable_dirs: BTreeSet::new(),
            known: BTreeSet::new(),
            everywhere: None,
        }
    }

    fn patterns_for(&mut self, cwd: Option<&str>) -> Rc<Vec<String>> {
        let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
            return Rc::clone(&self.global_patterns);
        };
        if let Some(found) = self.by_dir.get(cwd) {
            return Rc::clone(found);
        }
        let patterns = config::overlay_config_for_dir(&self.global, Path::new(cwd)).map_or_else(
            |_| {
                self.unreadable_dirs.insert(cwd.to_string());
                Rc::clone(&self.global_patterns)
            },
            |merged| Rc::new(merged.redaction.extra_patterns),
        );
        self.by_dir.insert(cwd.to_string(), Rc::clone(&patterns));
        patterns
    }

    fn mask(&mut self, text: &str, cwd: Option<&str>) -> String {
        let patterns = self.patterns_for(cwd);
        let known = &mut self.known;
        let masked = crate::redact::redact_secrets_noting(text, &patterns, &mut |value| {
            let value = value.trim_end_matches([',', ';', '.']);
            if is_a_literal_secret(value) {
                known.insert(value.to_owned());
            }
        });
        match &self.everywhere {
            Some(values) => values
                .replace_all(&masked, crate::redact::REDACTED)
                .into_owned(),
            None => masked,
        }
    }

    /// From now on, mask every value seen so far wherever it appears. Whether
    /// there were any, and so whether another pass can find more.
    fn spread_known_values(&mut self) -> Result<bool, regex::Error> {
        // Longest first, so that a value holding a shorter one goes whole.
        let mut values: Vec<&String> = self.known.iter().collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        if values.is_empty() {
            return Ok(false);
        }
        let alternatives: Vec<String> = values.iter().map(|value| regex::escape(value)).collect();
        self.everywhere = Some(
            RegexBuilder::new(&alternatives.join("|"))
                .size_limit(1 << 28)
                .build()?,
        );
        Ok(true)
    }
}

/// A backup, and what the rules make of it.
struct BackupScan {
    path: PathBuf,
    plan: Result<RedactionPlan, String>,
}

impl BackupScan {
    fn name(&self) -> std::borrow::Cow<'_, str> {
        self.path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy()
    }
}

/// A cached prompt whose masked text differs.
struct PromptFile {
    path: PathBuf,
    masked: String,
}

/// Everything one run found, before anything is written.
struct Scan {
    plan: RedactionPlan,
    kept: KeptSecrets,
    backups: Vec<BackupScan>,
    prompts: Vec<PromptFile>,
}

impl Scan {
    fn backup_rows(&self) -> usize {
        self.backups
            .iter()
            .filter_map(|backup| backup.plan.as_ref().ok())
            .map(|plan| plan.changes.len())
            .sum()
    }

    /// Whether anything would be masked, as opposed to only compacted.
    fn masks_anything(&self) -> bool {
        !self.plan.changes.is_empty() || self.backup_rows() > 0 || !self.prompts.is_empty()
    }

    fn compaction_due(&self) -> bool {
        self.plan.compaction_due
            || self
                .backups
                .iter()
                .any(|backup| backup.plan.as_ref().is_ok_and(|plan| plan.compaction_due))
    }

    /// Rows left alone, anywhere, because they could not be read.
    fn unreadable_rows(&self) -> usize {
        let in_backups: usize = self
            .backups
            .iter()
            .filter_map(|backup| backup.plan.as_ref().ok())
            .map(|plan| plan.unreadable)
            .sum();
        self.plan.unreadable + in_backups
    }
}

/// File name prefix of the copy taken before masking.
const PRE_REDACT_PREFIX: &str = "preredact";

fn is_pre_redact_copy(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(PRE_REDACT_PREFIX))
}

/// The `.db` files in the backups directory, oldest name first.
fn database_files_in_backups() -> Vec<PathBuf> {
    let Some(entries) = crate::db::backup_dir_path()
        .ok()
        .and_then(|dir| std::fs::read_dir(dir).ok())
    else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .collect();
    files.sort();
    files
}

/// The older backups to mask. Pre-redact copies are not among them: rerunning
/// this to confirm it worked must not destroy the way back.
fn backup_files() -> Vec<PathBuf> {
    let mut files = database_files_in_backups();
    files.retain(|path| !is_pre_redact_copy(path));
    files
}

fn pre_redact_copies() -> Vec<PathBuf> {
    let mut files = database_files_in_backups();
    files.retain(|path| is_pre_redact_copy(path));
    files
}

fn scan_backup(path: &Path, mask: &mut Mask<'_>) -> Result<RedactionPlan, String> {
    let describe = |e: crate::db::DbError| e.to_string();
    let repo = Repository::init_read_only(path).map_err(describe)?;
    if !repo.has_table("entries").map_err(describe)? {
        return Err("not a Suvadu database".to_string());
    }
    repo.find_unmasked(mask).map_err(describe)
}

/// The `.prompt` files under `dir`, however deep.
fn prompt_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            prompt_files(&path, found);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "prompt") {
            found.push(path);
        }
    }
}

fn scan_prompts(mask: &mut Mask<'_>) -> Vec<PromptFile> {
    let Some(dir) = super::entry::get_prompts_dir() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    prompt_files(&dir, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let masked = mask(&text, None);
            (masked != text).then_some(PromptFile { path, masked })
        })
        .collect()
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn rows(n: usize) -> String {
    count(n, "row", "rows")
}

/// How many rows change in each store, in report order.
fn print_store_counts(plan: &RedactionPlan) {
    let mut totals: Vec<(&str, usize)> = Vec::new();
    for change in &plan.changes {
        match totals.last_mut() {
            Some((store, total)) if *store == change.store => *total += 1,
            _ => totals.push((change.store, 1)),
        }
    }
    for (store, total) in totals {
        println!("  {store:<27} {total}");
    }
}

/// The note on a file that an earlier run masked and did not compact.
const fn owed(plan: &RedactionPlan) -> &'static str {
    if plan.compaction_due {
        " (and the compaction an earlier run left undone)"
    } else {
        ""
    }
}

fn print_counts(scan: &Scan) {
    println!(
        "history.db: {}{}",
        rows(scan.plan.changes.len()),
        owed(&scan.plan)
    );
    print_store_counts(&scan.plan);
    if !scan.backups.is_empty() {
        println!("Backups:");
        for backup in &scan.backups {
            match &backup.plan {
                Ok(plan) => println!(
                    "  {}: {}{}",
                    backup.name(),
                    rows(plan.changes.len()),
                    owed(plan)
                ),
                Err(why) => println!("  {}: could not be read ({why})", backup.name()),
            }
        }
    }
    if !scan.prompts.is_empty() {
        println!(
            "Prompt cache: {}",
            count(scan.prompts.len(), "file", "files")
        );
    }
}

/// Every distinct change across the database and its backups. The same row
/// in a backup reads the same, so each is listed once with how often it comes.
fn print_changes(scan: &Scan) {
    let plans = std::iter::once(&scan.plan).chain(
        scan.backups
            .iter()
            .filter_map(|backup| backup.plan.as_ref().ok()),
    );
    let mut seen: BTreeMap<(&str, String), usize> = BTreeMap::new();
    for change in plans.flat_map(|plan| &plan.changes) {
        *seen.entry((change.store, change.preview())).or_default() += 1;
    }
    if seen.is_empty() {
        return;
    }
    println!("\nWould become (each row's first change):");
    for ((store, preview), times) in seen {
        let times = if times > 1 {
            format!(" (×{times})")
        } else {
            String::new()
        };
        println!("  {store}{times}: {preview}");
    }
}

/// What masking leaves in place. Saying so is the difference between a
/// cleanup and a promise the storage layer cannot keep.
fn what_stays(kept: KeptSecrets, dry_run: bool) -> Vec<String> {
    let mut lines = vec!["\nWhat this does not reach:".to_string()];
    let copies = pre_redact_copies();
    for copy in &copies {
        lines.push(format!(
            "  · the pre-redact backup {} still holds every secret: delete it once you have \
             checked the result",
            copy.display()
        ));
    }
    if dry_run && copies.is_empty() {
        lines.push(
            "  · the pre-redact backup a real run takes first, which holds every secret until \
             you delete it"
                .to_string(),
        );
    }
    lines.push(
        "  · `suv export` files, `suv backup --out` copies, filesystem snapshots and Time Machine"
            .to_string(),
    );
    lines.push("  · your shell's own history file and agents' transcript files".to_string());
    let mut yours = Vec::new();
    if kept.bookmarks > 0 {
        yours.push(count(kept.bookmarks, "bookmark", "bookmarks"));
    }
    if kept.notes > 0 {
        yours.push(count(kept.notes, "note", "notes"));
    }
    if kept.aliases > 0 {
        yours.push(count(kept.aliases, "alias", "aliases"));
    }
    if !yours.is_empty() {
        lines.push(format!(
            "  · {} the rules match: you wrote these, and masking one would break it, so edit \
             them yourself",
            yours.join(", ")
        ));
    }
    lines.push("  Masking a stored secret does not make it safe: rotate any still in use.".into());
    lines
}

fn print_warnings(scan: &Scan, rules: &Rules) {
    let unreadable = scan.unreadable_rows();
    if unreadable > 0 {
        println!(
            "⚠ {} could not be read and {} left as they are.",
            rows(unreadable),
            if unreadable == 1 { "was" } else { "were" }
        );
    }
    for dir in &rules.unreadable_dirs {
        println!(
            "⚠ {dir}: the project's .suvadu.toml could not be read, so its rows used the global \
             rules. Fix it and run this again."
        );
    }
}

fn confirm(scan: &Scan) -> Result<bool, Box<dyn std::error::Error>> {
    if !util::stdin_is_terminal() {
        return Err(
            "Refusing to mask without confirmation: stdin is not a terminal. \
             Re-run with --yes to confirm non-interactively."
                .into(),
        );
    }
    let mut parts = Vec::new();
    if !scan.plan.changes.is_empty() {
        parts.push(format!("{} in history.db", rows(scan.plan.changes.len())));
    }
    if scan.backup_rows() > 0 {
        parts.push(format!("{} in backups", rows(scan.backup_rows())));
    }
    if !scan.prompts.is_empty() {
        parts.push(count(
            scan.prompts.len(),
            "prompt cache file",
            "prompt cache files",
        ));
    }
    eprint!("Mask {}? This cannot be undone. [y/N] ", parts.join(", "));
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

/// What one file's pass did, for the report.
fn done(name: &str, masked: usize) -> String {
    if masked == 0 {
        format!("✓ {name}: finished the compaction an earlier run left undone.")
    } else {
        format!("✓ {name}: masked {}.", rows(masked))
    }
}

/// Mask the live database, then clear what the old text left behind.
fn mask_database(
    repo: &Repository,
    plan: &RedactionPlan,
    mask: &mut Mask<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Reported only once the work is done: a closed pipe stops the process
    // at the first print.
    let masked = repo.apply_redaction(plan)?;
    if let Err(e) = repo.compact_after_masking() {
        return Err(format!(
            "{} masked, but compacting the file failed ({e}), so it still holds old text. Run \
             `suv redact` again to finish.",
            rows(masked)
        )
        .into());
    }
    let emptied = repo.checkpoint_wal()?;
    println!("{}", done("history.db", masked));
    if !emptied {
        println!(
            "⚠ Another suv process was reading, so the write-ahead log was not emptied. It \
             empties when the last one exits."
        );
    }
    if !repo.find_unmasked(mask)?.changes.is_empty() {
        println!("⚠ Some rows changed while this ran. Run `suv redact` again.");
    }
    if !repo.search_index_is_consistent()? {
        println!("⚠ The search index no longer matches your history. Run `suv gc` to repair it.");
    }
    Ok(())
}

/// Mask one older backup, leaving its schema as it was.
fn mask_backup(path: &Path, plan: &RedactionPlan) -> crate::db::DbResult<usize> {
    let repo = Repository::open_backup(path)?;
    let masked = repo.apply_redaction(plan)?;
    repo.compact_after_masking()?;
    Ok(masked)
}

/// Mask every backup that needs it. Returns how many could not be finished,
/// having carried on past each: one bad file must not leave the rest exposed.
fn mask_backups(backups: &[BackupScan]) -> usize {
    let mut failed = 0;
    for backup in backups {
        let plan = match &backup.plan {
            Ok(plan) if plan.changes.is_empty() && !plan.compaction_due => continue,
            Ok(plan) => plan,
            Err(why) => {
                println!("⚠ {}: could not be read ({why})", backup.name());
                failed += 1;
                continue;
            }
        };
        match mask_backup(&backup.path, plan) {
            Ok(masked) => println!("{}", done(&backup.name(), masked)),
            Err(e) => {
                println!("⚠ {}: could not be masked ({e})", backup.name());
                failed += 1;
            }
        }
    }
    failed
}

fn mask_prompts(prompts: &[PromptFile]) -> usize {
    let mut failed = 0;
    for prompt in prompts {
        if let Err(e) = util::atomic_write_with_mode(&prompt.path, &prompt.masked, 0o600) {
            println!("⚠ {}: could not be masked ({e})", prompt.path.display());
            failed += 1;
        }
    }
    if failed < prompts.len() {
        let masked = prompts.len() - failed;
        println!("✓ Prompt cache: masked {}.", count(masked, "file", "files"));
    }
    failed
}

/// One pass of the rules over everything this command reaches.
fn scan_everything(repo: &Repository, rules: &mut Rules) -> crate::db::DbResult<Scan> {
    let mut mask = |text: &str, cwd: Option<&str>| rules.mask(text, cwd);
    Ok(Scan {
        plan: repo.find_unmasked(&mut mask)?,
        kept: repo.count_kept_secrets(&mut mask)?,
        backups: backup_files()
            .into_iter()
            .map(|path| {
                let plan = scan_backup(&path, &mut mask);
                BackupScan { path, plan }
            })
            .collect(),
        prompts: scan_prompts(&mut mask),
    })
}

/// `suv redact`.
pub fn handle_redact(
    dry_run: bool,
    skip_confirm: bool,
    no_backup: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;
    let mut rules = Rules::new(config::load_config()?);
    let first_pass = scan_everything(&repo, &mut rules)?;
    let scan = if rules.spread_known_values()? {
        scan_everything(&repo, &mut rules)?
    } else {
        first_pass
    };

    if dry_run {
        println!("Dry run — nothing will change.\n");
        print_counts(&scan);
        print_changes(&scan);
        print_warnings(&scan, &rules);
        for line in what_stays(scan.kept, true) {
            println!("{line}");
        }
        return Ok(());
    }

    if !scan.masks_anything() && !scan.compaction_due() {
        println!("Nothing to mask.");
        print_warnings(&scan, &rules);
        let failed = mask_backups(&scan.backups);
        for line in what_stays(scan.kept, false) {
            println!("{line}");
        }
        if failed > 0 {
            return Err(format!("{} not masked.", count(failed, "backup", "backups")).into());
        }
        return Ok(());
    }

    print_counts(&scan);
    // Compacting changes nothing anyone could read, so only masking asks.
    if scan.masks_anything() && !skip_confirm && !confirm(&scan)? {
        println!("Aborted.");
        return Ok(());
    }

    // The live database is copied first because this cannot be undone. A
    // failed copy stops the run rather than proceeding without a way back.
    if !no_backup && !scan.plan.changes.is_empty() {
        let path = super::entry::timestamped_backup_path(PRE_REDACT_PREFIX).map_err(|e| {
            format!("Could not prepare backup ({e}); nothing was masked. Re-run with --no-backup to skip.")
        })?;
        repo.backup_to(&path).map_err(|e| {
            format!("Backup failed ({e}); nothing was masked. Re-run with --no-backup to skip the backup.")
        })?;
        println!("Backed up to {} before masking.", path.display());
    }

    let mut failed = 0;
    if !scan.plan.changes.is_empty() || scan.plan.compaction_due {
        let mut mask = |text: &str, cwd: Option<&str>| rules.mask(text, cwd);
        mask_database(&repo, &scan.plan, &mut mask)?;
    }
    failed += mask_backups(&scan.backups);
    let failed_prompts = mask_prompts(&scan.prompts);

    print_warnings(&scan, &rules);
    if scan
        .plan
        .changes
        .iter()
        .any(|c| c.store == crate::repository::COMMANDS)
    {
        println!("Agent summaries of sessions whose commands were masked now read as stale.");
    }
    for line in what_stays(scan.kept, false) {
        println!("{line}");
    }
    if failed + failed_prompts > 0 {
        return Err(format!(
            "{} not masked.",
            count(failed + failed_prompts, "file was", "files were")
        )
        .into());
    }
    Ok(())
}
