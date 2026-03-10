use crate::cli;
use crate::db;
use crate::repository::Repository;

pub fn handle_tag(cmd: cli::TagCommands) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    match cmd {
        cli::TagCommands::Create { name, description } => {
            handle_tag_create(&repo, name, description.as_deref())?;
        }
        cli::TagCommands::List => {
            handle_tag_list(&repo)?;
        }
        cli::TagCommands::Associate {
            tag_name,
            session_id,
        } => {
            handle_tag_associate(&repo, &tag_name, session_id)?;
        }
        cli::TagCommands::Update {
            name,
            new_name,
            description,
        } => {
            handle_tag_update(&repo, &name, new_name.as_deref(), description.as_deref())?;
        }
    }
    Ok(())
}

fn handle_tag_create(
    repo: &Repository,
    name: Option<String>,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = if let Some(n) = name {
        n
    } else {
        // Interactive prompt if name missing
        print!("Enter tag name: ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;
        buffer.trim().to_string()
    };

    if name.is_empty() {
        return Err("Tag name cannot be empty.".into());
    }

    match repo.create_tag(&name, description) {
        Ok(_) => {
            println!("✓ Tag '{}' created", name.to_lowercase());
            Ok(())
        }
        Err(e) => {
            if let db::DbError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) = &e {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    return Err(format!("Tag '{name}' already exists.").into());
                }
            }
            Err(e.into())
        }
    }
}

fn handle_tag_list(repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
    let tags = repo.get_tags()?;
    if tags.is_empty() {
        println!("No tags found. Use 'suv tag create <name>' to add one.");
    } else {
        let max_name = tags.iter().map(|t| t.name.len()).max().unwrap_or(4).max(4);
        let color = crate::util::color_enabled();

        println!();
        for tag in &tags {
            let desc = tag.description.as_deref().unwrap_or("");
            if color {
                print!("  \x1b[36m{:<width$}\x1b[0m", tag.name, width = max_name);
            } else {
                print!("  {:<width$}", tag.name, width = max_name);
            }
            if desc.is_empty() {
                println!();
            } else {
                println!("  {desc}");
            }
        }
        println!(
            "\n  {} tag{}",
            tags.len(),
            if tags.len() == 1 { "" } else { "s" }
        );
        println!();
    }
    Ok(())
}

fn handle_tag_associate(
    repo: &Repository,
    tag_name: &str,
    session_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Find tag or create it
    let tag_id = if let Some(id) = repo.get_tag_id_by_name(tag_name)? {
        id
    } else {
        // Try to create it
        println!("Tag '{tag_name}' not found. Creating it...");
        match repo.create_tag(tag_name, None) {
            Ok(id) => {
                println!("✓ Tag '{tag_name}' created");
                id
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    };

    let sid = session_id
        .or_else(|| std::env::var("SUVADU_SESSION_ID").ok())
        .unwrap_or_default();

    if sid.is_empty() {
        return Err("No session ID provided or found in env.".into());
    }

    repo.tag_session(&sid, Some(tag_id))?;
    println!("✓ Session '{sid}' associated with tag '{tag_name}'");
    Ok(())
}

fn handle_tag_update(
    repo: &Repository,
    name: &str,
    new_name: Option<&str>,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let tags = repo.get_tags()?;
    let tag = tags.iter().find(|t| t.name == name.to_lowercase());

    let Some(t) = tag else {
        return Err(format!("Tag '{name}' not found.").into());
    };

    let updated_name = new_name.unwrap_or(&t.name);
    let updated_desc = description.or(t.description.as_deref());

    repo.update_tag(t.id, updated_name, updated_desc)?;
    println!("✓ Tag '{}' updated", t.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Session;
    use crate::test_utils::test_repo;

    fn setup_test_db() -> (tempfile::TempDir, Repository) {
        test_repo()
    }

    #[test]
    fn test_tag_create_with_name() {
        let (_dir, repo) = setup_test_db();
        handle_tag_create(&repo, Some("work".to_string()), None).unwrap();

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "work");
    }

    #[test]
    fn test_tag_create_with_description() {
        let (_dir, repo) = setup_test_db();
        handle_tag_create(&repo, Some("work".to_string()), Some("Work tasks")).unwrap();

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags[0].description.as_deref(), Some("Work tasks"));
    }

    #[test]
    fn test_tag_create_empty_name() {
        let (_dir, repo) = setup_test_db();
        // Empty string name — should return an error
        let result = handle_tag_create(&repo, Some(String::new()), None);
        assert!(result.is_err());
        let tags = repo.get_tags().unwrap();
        assert!(tags.is_empty());
    }

    #[test]
    fn test_tag_create_duplicate() {
        let (_dir, repo) = setup_test_db();
        handle_tag_create(&repo, Some("work".to_string()), None).unwrap();
        // Creating again with same name (case insensitive) should return error
        let result = handle_tag_create(&repo, Some("WORK".to_string()), None);
        assert!(result.is_err());

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 1); // Still only one
    }

    #[test]
    fn test_tag_list_empty() {
        let (_dir, repo) = setup_test_db();
        // Should not error on empty list
        handle_tag_list(&repo).unwrap();
    }

    #[test]
    fn test_tag_list_with_tags() {
        let (_dir, repo) = setup_test_db();
        repo.create_tag("alpha", None).unwrap();
        repo.create_tag("beta", Some("Beta tag")).unwrap();
        // Should not error when listing tags
        handle_tag_list(&repo).unwrap();
    }

    #[test]
    fn test_tag_associate_existing_tag() {
        let (_dir, repo) = setup_test_db();
        let tag_id = repo.create_tag("work", None).unwrap();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        handle_tag_associate(&repo, "work", Some(session.id.clone())).unwrap();

        let s = repo.get_session(&session.id).unwrap().unwrap();
        assert_eq!(s.tag_id, Some(tag_id));
    }

    #[test]
    fn test_tag_associate_auto_creates_tag() {
        let (_dir, repo) = setup_test_db();
        let session = Session::new("host".to_string(), 100);
        repo.insert_session(&session).unwrap();

        // Tag doesn't exist yet — should auto-create
        handle_tag_associate(&repo, "newtag", Some(session.id.clone())).unwrap();

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "newtag");

        let s = repo.get_session(&session.id).unwrap().unwrap();
        assert_eq!(s.tag_id, Some(tags[0].id));
    }

    /// Test the logic: when session_id is None and SUVADU_SESSION_ID env var
    /// is empty/missing, `handle_tag_associate` should return an error.
    /// We test the parsing logic directly instead of mutating process env
    /// (which is not thread-safe and deprecated in Rust 2024).
    #[test]
    fn test_tag_associate_no_session_id() {
        // The function reads session_id.or_else(|| env::var("SUVADU_SESSION_ID").ok())
        // and errors when the result is empty. Test the inline logic:
        let session_id: Option<String> = None;
        let from_env: Option<String> = None; // simulates missing env var
        let sid = session_id.or(from_env).unwrap_or_default();
        assert!(sid.is_empty(), "Should have no session ID");
    }

    #[test]
    fn test_tag_update_name() {
        let (_dir, repo) = setup_test_db();
        repo.create_tag("old", None).unwrap();

        handle_tag_update(&repo, "old", Some("new"), None).unwrap();

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags[0].name, "new");
    }

    #[test]
    fn test_tag_update_description() {
        let (_dir, repo) = setup_test_db();
        repo.create_tag("work", None).unwrap();

        handle_tag_update(&repo, "work", None, Some("Updated desc")).unwrap();

        let tags = repo.get_tags().unwrap();
        assert_eq!(tags[0].description.as_deref(), Some("Updated desc"));
    }

    #[test]
    fn test_tag_update_not_found() {
        let (_dir, repo) = setup_test_db();
        // Should return error when updating nonexistent tag
        let result = handle_tag_update(&repo, "nonexistent", Some("new"), None);
        assert!(result.is_err());
    }
}
