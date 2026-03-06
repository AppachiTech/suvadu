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
        eprintln!("Tag name cannot be empty.");
        return Ok(());
    }

    match repo.create_tag(&name, description) {
        Ok(_) => println!("✓ Tag '{}' created", name.to_lowercase()),
        Err(e) => {
            if let db::DbError::Sqlite(rusqlite::Error::SqliteFailure(err, _)) = &e {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    eprintln!("Error: Tag '{name}' already exists.");
                    return Ok(());
                }
            }
            eprintln!("Error creating tag: {e}");
        }
    }
    Ok(())
}

fn handle_tag_list(repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
    let tags = repo.get_tags()?;
    if tags.is_empty() {
        println!("No tags found.");
    } else {
        // Calculate widths
        let max_name = tags.iter().map(|t| t.name.len()).max().unwrap_or(4).max(4);
        let max_desc = tags
            .iter()
            .map(|t| t.description.as_deref().unwrap_or("").len())
            .max()
            .unwrap_or(11)
            .max(11);

        let w_name = max_name + 2;
        let w_desc = max_desc + 2;

        let sep = format!("+{}+{}+", "-".repeat(w_name), "-".repeat(w_desc));

        println!("{sep}");
        println!("| {:<w_name$} | {:<w_desc$} |", "NAME", "DESCRIPTION");
        println!("{sep}");

        for tag in tags {
            println!(
                "| {:<w_name$} | {:<w_desc$} |",
                tag.name,
                tag.description.as_deref().unwrap_or("")
            );
        }
        println!("{sep}");
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
                if let db::DbError::Validation(ref msg) = e {
                    eprintln!("Error: {msg}");
                } else {
                    eprintln!("Error creating tag: {e}");
                }
                return Ok(());
            }
        }
    };

    let sid = session_id
        .or_else(|| std::env::var("SUVADU_SESSION_ID").ok())
        .unwrap_or_default();

    if sid.is_empty() {
        eprintln!("No session ID provided or found in env.");
        return Ok(());
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

    if let Some(t) = tag {
        let updated_name = new_name.unwrap_or(&t.name);
        let updated_desc = description.or(t.description.as_deref());

        match repo.update_tag(t.id, updated_name, updated_desc) {
            Ok(()) => println!("✓ Tag '{}' updated", t.name),
            Err(e) => eprintln!("Error updating tag: {e}"),
        }
    } else {
        eprintln!("Tag '{name}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;
    use crate::models::Session;
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Repository) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = init_db(&db_path).unwrap();
        let repo = Repository::new(conn);
        (temp_dir, repo)
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
        // Empty string name — should not create a tag
        handle_tag_create(&repo, Some(String::new()), None).unwrap();
        let tags = repo.get_tags().unwrap();
        assert!(tags.is_empty());
    }

    #[test]
    fn test_tag_create_duplicate() {
        let (_dir, repo) = setup_test_db();
        handle_tag_create(&repo, Some("work".to_string()), None).unwrap();
        // Creating again with same name (case insensitive) should not panic
        handle_tag_create(&repo, Some("WORK".to_string()), None).unwrap();

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

    #[test]
    fn test_tag_associate_no_session_id() {
        let (_dir, repo) = setup_test_db();
        repo.create_tag("work", None).unwrap();
        // No session ID provided and env var not set — should not error
        std::env::remove_var("SUVADU_SESSION_ID");
        handle_tag_associate(&repo, "work", None).unwrap();
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
        // Should not error when updating nonexistent tag
        handle_tag_update(&repo, "nonexistent", Some("new"), None).unwrap();
    }
}
