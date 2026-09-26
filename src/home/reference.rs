//! The command reference inside Home: the grouped overview `suv --help`
//! prints, then every public command's own help, rendered by clap from the
//! same definitions — never a second, hand-kept copy.

use clap::CommandFactory;

/// One page of the reference: the overview (empty path) or a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    pub path: Vec<String>,
    pub title: String,
}

/// The overview, then every public command depth-first in the order the
/// CLI defines them. Hidden hooks and protocol commands are left out, and
/// so are clap's generated `help` subcommands.
pub fn topics() -> Vec<Topic> {
    fn walk(cmd: &clap::Command, path: &[String], out: &mut Vec<Topic>) {
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            let mut sub_path = path.to_vec();
            sub_path.push(sub.get_name().to_string());
            out.push(Topic {
                title: format!("suv {}", sub_path.join(" ")),
                path: sub_path.clone(),
            });
            walk(sub, &sub_path, out);
        }
    }
    let mut out = vec![Topic {
        path: Vec::new(),
        title: "Overview".to_string(),
    }];
    walk(&crate::cli::Cli::command(), &[], &mut out);
    out
}

/// The help for one topic: the grouped overview for the empty path,
/// otherwise the command's long help (`suv <path> --help`), rendered by
/// clap as plain text. Hidden or unknown commands are an error to show.
pub fn command_help(path: &[&str]) -> Result<String, String> {
    if path.is_empty() {
        return Ok(crate::cli::overview());
    }
    let unknown = || format!("There is no public command `suv {}`.", path.join(" "));
    let mut root = crate::cli::Cli::command().bin_name("suv");
    root.build();
    let mut current = &mut root;
    for part in path {
        current = current
            .find_subcommand_mut(part)
            .filter(|c| !c.is_hide_set())
            .ok_or_else(unknown)?;
    }
    Ok(current.render_long_help().to_string())
}

/// Where the reference opens for a command path; the overview when the path
/// is not a topic.
pub fn topic_index(topics: &[Topic], path: &[&str]) -> usize {
    topics
        .iter()
        .position(|t| t.path.iter().map(String::as_str).eq(path.iter().copied()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_start_with_the_overview_then_follow_the_command_tree() {
        let topics = topics();
        assert_eq!(topics[0].path, Vec::<String>::new());
        assert_eq!(topics[0].title, "Overview");
        let titles: Vec<&str> = topics.iter().map(|t| t.title.as_str()).collect();
        let at = |t: &str| {
            titles
                .iter()
                .position(|x| *x == t)
                .unwrap_or_else(|| panic!("{t}"))
        };
        assert!(at("suv tag") < at("suv tag create"));
        assert!(at("suv tag create") < at("suv tag list"));
        assert!(at("suv search") < at("suv stats"));
        assert!(titles.contains(&"suv agent report"));
        assert!(titles.contains(&"suv skills sync"));
    }

    #[test]
    fn hidden_commands_are_not_topics() {
        for topic in topics() {
            let first = topic.path.first().map_or("", String::as_str);
            assert!(
                !matches!(first, "add" | "get" | "mcp-serve") && !first.starts_with("hook-"),
                "{first}"
            );
            assert!(!topic.path.iter().any(|p| p == "help"), "{:?}", topic.path);
        }
    }

    #[test]
    fn the_overview_is_exactly_what_suv_help_prints() {
        assert_eq!(command_help(&[]).unwrap(), crate::cli::overview());
    }

    #[test]
    fn a_command_topic_is_its_own_long_help() {
        let search = command_help(&["search"]).unwrap();
        assert!(search.contains("Usage: suv search"), "{search}");
        assert!(
            search.contains("Matching modes"),
            "the after-help is included"
        );
        assert!(!search.contains('\x1b'), "plain text");
        let nested = command_help(&["tag", "create"]).unwrap();
        assert!(nested.contains("Usage: suv tag create"), "{nested}");
    }

    #[test]
    fn unknown_or_hidden_commands_are_a_readable_error() {
        let err = command_help(&["nope"]).unwrap_err();
        assert!(err.contains("suv nope"), "{err}");
        assert!(command_help(&["mcp-serve"]).is_err());
        assert!(command_help(&["tag", "nope"]).is_err());
    }

    #[test]
    fn a_feature_opens_the_reference_at_its_command() {
        let topics = topics();
        let search = topic_index(&topics, &["search"]);
        assert_eq!(topics[search].title, "suv search");
        assert_eq!(topic_index(&topics, &["agent", "prompts"]), {
            topics
                .iter()
                .position(|t| t.title == "suv agent prompts")
                .unwrap()
        });
        assert_eq!(topic_index(&topics, &["no-such-command"]), 0);
    }
}
