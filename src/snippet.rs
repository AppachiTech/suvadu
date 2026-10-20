//! Parameterised bookmarks: `{{name}}` and `{{name:default}}` placeholders
//! inside a bookmarked command, filled in when the bookmark is recalled.
//!
//! The syntax, in full:
//!
//! - `{{name}}` is a placeholder. A name starts with a letter or `_` and
//!   continues with letters, digits, `_` or `-`. Spaces just inside the
//!   braces are allowed (`{{ name }}`).
//! - `{{name:default}}` gives it a default: everything between the first
//!   `:` and the closing `}}`, verbatim.
//! - A placeholder used more than once is asked for once and filled
//!   everywhere. Its default is the first one given.
//! - `\{{` is a literal `{{` (the backslash is dropped). Anything else that
//!   is not a well-formed placeholder — `{{` with no closing `}}`, or
//!   braces around something that is not a name, like Go templates'
//!   `{{.State.Status}}` — is left exactly as typed.
//!
//! Values are inserted verbatim: the user types shell syntax, so a value
//! containing spaces or `$` needs the quoting the shell would need. Filled
//! values are never stored.

use std::collections::HashMap;

/// One distinct placeholder in a template, in order of first appearance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Placeholder {
    pub name: String,
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Text(String),
    Slot(String),
}

/// A bookmarked command split into literal text and placeholder slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
    placeholders: Vec<Placeholder>,
}

const fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

const fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Parse the inside of `{{ ... }}`: a name, optionally `:default`.
fn parse_inner(inner: &str) -> Option<(String, Option<String>)> {
    let (name_part, default) = inner
        .split_once(':')
        .map_or((inner, None), |(name, default)| {
            (name, Some(default.to_string()))
        });
    let name = name_part.trim();
    let mut chars = name.chars();
    let first = chars.next()?;
    if !is_name_start(first) || !chars.all(is_name_char) {
        return None;
    }
    // `{{ name :x}}` is fine, but whitespace before the name ends only at
    // the braces, never inside the name.
    Some((name.to_string(), default))
}

impl Template {
    /// Parse `command`. Never fails: anything malformed stays literal text.
    pub fn parse(command: &str) -> Self {
        let mut segments: Vec<Segment> = Vec::new();
        let mut placeholders: Vec<Placeholder> = Vec::new();
        let mut text = String::new();
        let mut rest = command;

        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("\\{{") {
                text.push_str("{{");
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix("{{") {
                if let Some(close) = after.find("}}") {
                    if let Some((name, default)) = parse_inner(&after[..close]) {
                        if !text.is_empty() {
                            segments.push(Segment::Text(std::mem::take(&mut text)));
                        }
                        match placeholders.iter_mut().find(|p| p.name == name) {
                            Some(existing) => {
                                if existing.default.is_none() {
                                    existing.default = default;
                                }
                            }
                            None => placeholders.push(Placeholder {
                                name: name.clone(),
                                default,
                            }),
                        }
                        segments.push(Segment::Slot(name));
                        rest = &after[close + 2..];
                        continue;
                    }
                }
                // Not a placeholder: keep the braces and carry on after
                // them, so a later well-formed placeholder still counts.
                text.push_str("{{");
                rest = after;
                continue;
            }
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                text.push(c);
            }
            rest = chars.as_str();
        }
        if !text.is_empty() {
            segments.push(Segment::Text(text));
        }
        Self {
            segments,
            placeholders,
        }
    }

    /// The distinct placeholders, in order of first appearance.
    pub fn placeholders(&self) -> &[Placeholder] {
        &self.placeholders
    }

    pub const fn has_placeholders(&self) -> bool {
        !self.placeholders.is_empty()
    }

    /// Fill every slot from `values`, falling back to each placeholder's
    /// default. Errors with the names that have neither, in order.
    pub fn fill(&self, values: &HashMap<String, String>) -> Result<String, Vec<String>> {
        let missing: Vec<String> = self
            .placeholders
            .iter()
            .filter(|p| !values.contains_key(&p.name) && p.default.is_none())
            .map(|p| p.name.clone())
            .collect();
        if !missing.is_empty() {
            return Err(missing);
        }
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(t) => out.push_str(t),
                Segment::Slot(name) => {
                    let value = values.get(name).map(String::as_str).or_else(|| {
                        self.placeholders
                            .iter()
                            .find(|p| &p.name == name)
                            .and_then(|p| p.default.as_deref())
                    });
                    out.push_str(value.unwrap_or_default());
                }
            }
        }
        Ok(out)
    }

    /// Fill from values given in placeholder order (as the picker's popup
    /// collects them). Every placeholder gets the matching value.
    pub fn fill_in_order(&self, values: &[String]) -> String {
        let map: HashMap<String, String> = self
            .placeholders
            .iter()
            .zip(values)
            .map(|(p, v)| (p.name.clone(), v.clone()))
            .collect();
        self.fill(&map).unwrap_or_default()
    }
}

/// Parse `key=value` pairs from `--set`. Splits at the first `=`, so a
/// value may itself contain `=`; an empty value is allowed.
pub fn parse_assignments(pairs: &[String]) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("'{pair}' is not key=value"));
        };
        if key.is_empty() {
            return Err(format!("'{pair}' has no placeholder name before '='"));
        }
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(t: &Template) -> Vec<(&str, Option<&str>)> {
        t.placeholders()
            .iter()
            .map(|p| (p.name.as_str(), p.default.as_deref()))
            .collect()
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_plain_command_has_no_placeholders_and_fills_to_itself() {
        let t = Template::parse("git status -sb");
        assert!(!t.has_placeholders());
        assert_eq!(t.fill(&HashMap::new()).unwrap(), "git status -sb");
    }

    #[test]
    fn names_and_defaults_are_read_in_order() {
        let t = Template::parse("ssh {{user:root}}@{{host}} -p {{port:22}}");
        assert_eq!(
            names(&t),
            vec![("user", Some("root")), ("host", None), ("port", Some("22"))]
        );
        let filled = t.fill(&values(&[("host", "db1")])).unwrap();
        assert_eq!(filled, "ssh root@db1 -p 22");
    }

    #[test]
    fn spaces_inside_the_braces_are_allowed_and_defaults_are_verbatim() {
        let t = Template::parse("echo {{ greeting :hello world}}");
        assert_eq!(names(&t), vec![("greeting", Some("hello world"))]);
        assert_eq!(t.fill(&HashMap::new()).unwrap(), "echo hello world");
    }

    #[test]
    fn a_default_may_contain_colons_and_be_empty() {
        let t = Template::parse("curl {{url:http://localhost:8080}} {{flags:}}");
        assert_eq!(
            names(&t),
            vec![("url", Some("http://localhost:8080")), ("flags", Some(""))]
        );
        assert_eq!(
            t.fill(&HashMap::new()).unwrap(),
            "curl http://localhost:8080 "
        );
    }

    #[test]
    fn a_repeated_placeholder_is_asked_once_and_filled_everywhere() {
        let t = Template::parse("cp {{f}} {{f}}.bak && echo {{f:x}}");
        assert_eq!(names(&t), vec![("f", Some("x"))]);
        assert_eq!(
            t.fill(&values(&[("f", "a.txt")])).unwrap(),
            "cp a.txt a.txt.bak && echo a.txt"
        );
        assert_eq!(t.fill(&HashMap::new()).unwrap(), "cp x x.bak && echo x");
    }

    #[test]
    fn an_escaped_opening_is_a_literal_double_brace() {
        let t = Template::parse(r"echo \{{name}} {{name}}");
        assert_eq!(names(&t), vec![("name", None)]);
        assert_eq!(
            t.fill(&values(&[("name", "x")])).unwrap(),
            "echo {{name}} x"
        );
    }

    #[test]
    fn malformed_braces_are_left_literal() {
        for command in [
            "echo {{unterminated",
            "docker inspect -f '{{.State.Status}}' web",
            "echo {{json .}}",
            "echo {{}}",
            "echo {{1st}}",
            "echo {{a b}}",
            "awk '{print $1}'",
        ] {
            let t = Template::parse(command);
            assert!(!t.has_placeholders(), "{command}");
            assert_eq!(t.fill(&HashMap::new()).unwrap(), command, "{command}");
        }
    }

    #[test]
    fn a_malformed_opening_does_not_hide_a_later_placeholder() {
        let t = Template::parse("echo {{ {{name}}");
        assert_eq!(names(&t), vec![("name", None)]);
        assert_eq!(t.fill(&values(&[("name", "x")])).unwrap(), "echo {{ x");
    }

    #[test]
    fn values_are_inserted_verbatim_without_shell_quoting() {
        let t = Template::parse("grep {{pattern}} src");
        assert_eq!(
            t.fill(&values(&[("pattern", "'a b' $HOME")])).unwrap(),
            "grep 'a b' $HOME src"
        );
    }

    #[test]
    fn missing_values_without_defaults_are_named_in_order() {
        let t = Template::parse("{{b}} {{a:1}} {{c}}");
        assert_eq!(
            t.fill(&HashMap::new()).unwrap_err(),
            vec!["b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn an_explicit_empty_value_wins_over_the_default() {
        let t = Template::parse("ls {{flags:-la}}");
        assert_eq!(t.fill(&values(&[("flags", "")])).unwrap(), "ls ");
    }

    #[test]
    fn non_ascii_text_around_placeholders_survives() {
        let t = Template::parse("echo é{{x}}ü");
        assert_eq!(t.fill(&values(&[("x", "ß")])).unwrap(), "echo éßü");
    }

    #[test]
    fn fill_in_order_matches_values_to_placeholders() {
        let t = Template::parse("{{a}}-{{b}}-{{a}}");
        assert_eq!(t.fill_in_order(&["1".into(), "2".into()]), "1-2-1");
    }

    #[test]
    fn assignments_split_at_the_first_equals() {
        let parsed = parse_assignments(&["q=a=b".to_string(), "empty=".to_string()]).unwrap();
        assert_eq!(parsed["q"], "a=b");
        assert_eq!(parsed["empty"], "");
        assert!(parse_assignments(&["novalue".to_string()]).is_err());
        assert!(parse_assignments(&["=x".to_string()]).is_err());
    }
}
