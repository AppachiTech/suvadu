use crate::models::SearchField;
use crate::repository;
use crate::util;

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn handle_history(
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    here: bool,
    cwd: Option<&str>,
    limit: usize,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        cwd.map(String::from)
    };

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    let entries = repo.get_entries_filtered(
        limit,
        0,
        &repository::QueryFilter {
            after: after_ms,
            before: before_ms,
            tag_id,
            exit_code,
            query: None,
            prefix_match: false,
            executor,
            cwd: cwd_filter.as_deref(),
            field: SearchField::Command,
            exclude_agents: false,
            cwd_prefix: false,
        },
    )?;

    if json {
        for entry in &entries {
            println!("{}", serde_json::to_string(entry)?);
        }
    } else {
        let home = util::dirs_home();
        let color = util::color_enabled();

        for entry in &entries {
            let time = chrono::DateTime::from_timestamp_millis(entry.started_at).map_or_else(
                || "????-??-?? ??:??:??".into(),
                |dt| {
                    dt.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                },
            );

            let dir = util::shorten_path(&entry.cwd, &home);
            let duration = util::format_duration_ms(entry.duration_ms);

            let (status, status_color) = match entry.exit_code {
                Some(0) => ("\u{2713}".to_string(), "32"),
                Some(code) => (format!("\u{2717}{code}"), "31"),
                None => ("\u{2022}".to_string(), "33"),
            };

            if color {
                println!(
                    "{time}  \x1b[{status_color}m{status:<4}\x1b[0m \x1b[2m{duration:>7}\x1b[0m  {dir:<20}  {cmd}",
                    cmd = entry.command
                );
            } else {
                println!(
                    "{time}  {status:<4} {duration:>7}  {dir:<20}  {cmd}",
                    cmd = entry.command
                );
            }
        }

        if entries.is_empty() {
            eprintln!("No commands found.");
        }
    }

    Ok(())
}
