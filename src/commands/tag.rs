use crate::cli;
use crate::db;
use crate::repository::Repository;

pub fn handle_tag(cmd: cli::TagCommands) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = db::get_db_path()?;
    let conn = db::init_db(&db_path)?;
    let repo = Repository::new(conn);

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
    let tags = repo.get_tags()?;
    let existing_tag = tags.iter().find(|t| t.name == tag_name.to_lowercase());

    let tag_id = if let Some(t) = existing_tag {
        t.id
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
