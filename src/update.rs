pub fn is_homebrew_install() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy();
        return path.contains("/Cellar/")
            || path.contains("/homebrew/")
            || path.contains("/linuxbrew/");
    }
    false
}

pub fn handle_update() -> Result<(), Box<dyn std::error::Error>> {
    println!("Current version: v{}", env!("CARGO_PKG_VERSION"));

    if is_homebrew_install() {
        println!();
        println!("Suvadu was installed via Homebrew. To update, run:");
        println!();
        if crate::util::color_enabled() {
            println!("  \x1b[36mbrew upgrade suvadu\x1b[0m");
        } else {
            println!("  brew upgrade suvadu");
        }
        println!();
        println!("Using 'suv update' with Homebrew installs can cause version conflicts.");
        return Ok(());
    }

    println!("Checking for updates...");
    println!();

    let (platform, platform_label) = match std::env::consts::OS {
        "macos" => ("macos", "macOS"),
        "linux" => ("linux", "Linux"),
        os => {
            return Err(format!(
                "Unsupported platform '{os}'. Only macOS and Linux are supported."
            )
            .into());
        }
    };

    let arch_suffix = if platform == "linux" && std::env::consts::ARCH == "aarch64" {
        "-aarch64"
    } else {
        ""
    };

    let base_url = format!("https://downloads.appachi.tech/{platform}");
    let archive_name = format!("suv-{platform}{arch_suffix}-latest.tar.gz");
    let archive_url = format!("{base_url}/{archive_name}");
    let checksum_url = format!("{base_url}/{archive_name}.sha256");

    let update_dir = tempfile::TempDir::new()?;
    let tarball_path = update_dir.path().join("suv-update.tar.gz");
    let binary_path = update_dir.path().join("suv");
    let tarball_str = tarball_path.to_string_lossy().to_string();
    let update_dir_str = update_dir.path().to_string_lossy().to_string();

    download_and_verify(
        platform_label,
        &archive_url,
        &checksum_url,
        &tarball_str,
        &update_dir_str,
        &binary_path,
        update_dir.path(),
    )?;

    install_binary(&binary_path)?;
    Ok(())
}

fn download_and_verify(
    platform_label: &str,
    archive_url: &str,
    checksum_url: &str,
    tarball_str: &str,
    update_dir_str: &str,
    binary_path: &std::path::Path,
    update_dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Downloading {platform_label} build from: {archive_url}");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "-m", "300", "-o", tarball_str, archive_url])
        .status()?;
    if !status.success() {
        return Err("Failed to download update. Please check your internet connection.".into());
    }

    verify_checksum(checksum_url, tarball_str)?;

    let status = std::process::Command::new("tar")
        .args(["--no-same-owner", "-xzf", tarball_str, "-C", update_dir_str])
        .status()?;
    if !status.success() {
        return Err("Failed to extract update archive.".into());
    }

    if !binary_path.exists() {
        return Err("Expected binary not found after extraction.".into());
    }
    let canonical_binary = binary_path.canonicalize()?;
    let canonical_dir = update_dir.canonicalize()?;
    if !canonical_binary.starts_with(&canonical_dir) {
        return Err("Extracted binary is outside temp directory. Aborting for security.".into());
    }

    println!("  Download complete");
    println!();
    Ok(())
}

fn verify_checksum(
    checksum_url: &str,
    tarball_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let checksum_result = std::process::Command::new("curl")
        .args(["-fsSL", "-m", "30", checksum_url])
        .output();

    match checksum_result {
        Ok(output) if output.status.success() => {
            let expected = String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            let actual_output = if cfg!(target_os = "macos") {
                std::process::Command::new("shasum")
                    .args(["-a", "256", tarball_str])
                    .output()?
            } else {
                std::process::Command::new("sha256sum")
                    .arg(tarball_str)
                    .output()?
            };
            let actual = String::from_utf8_lossy(&actual_output.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();

            if expected.is_empty() || actual.is_empty() || expected != actual {
                return Err(format!(
                    "Checksum verification failed!\n  Expected: {expected}\n  Got:      {actual}\n\
                     Aborting update for security. The download may be corrupted or tampered with."
                )
                .into());
            }
            println!(
                "  SHA256 checksum verified: {}",
                actual.get(..16).unwrap_or(&actual)
            );
            Ok(())
        }
        _ => Err("Could not fetch checksum for verification. Aborting update for security.".into()),
    }
}

fn install_binary(binary_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let binary_str = binary_path.to_string_lossy().to_string();
    let install_path =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("/usr/local/bin/suv"));
    let install_str = install_path.to_string_lossy();
    let install_dir = install_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("/usr/local/bin"));
    let symlink_path = install_dir.join("suvadu");

    print!("Install update? This will replace {install_str} [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    if input.trim().to_lowercase() != "y" {
        println!("Update cancelled.");
        return Ok(());
    }

    println!("Installing update (requires sudo)...");

    let status_bin = std::process::Command::new("sudo")
        .args(["cp", &binary_str, &*install_str])
        .status()?;
    let status_link = std::process::Command::new("sudo")
        .args(["ln", "-sf", &*install_str, &*symlink_path.to_string_lossy()])
        .status()?;

    if status_bin.success() && status_link.success() {
        println!("  Update successful!");
        println!();
        println!("Run 'suv version' to verify the new version.");
        Ok(())
    } else {
        Err("Failed to install update.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_not_homebrew_install() {
        let result = is_homebrew_install();
        assert!(
            !result,
            "Test binary should not be detected as a Homebrew install"
        );
    }
}
