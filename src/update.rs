pub fn is_homebrew_install() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy();
        // Homebrew on Apple Silicon: /opt/homebrew/...
        // Homebrew on Intel Mac: /usr/local/Cellar/...
        // Linuxbrew: /home/linuxbrew/.linuxbrew/...
        return path.contains("/Cellar/")
            || path.contains("/homebrew/")
            || path.contains("/linuxbrew/");
    }
    false
}

#[allow(clippy::too_many_lines)]
pub fn handle_update() -> Result<(), Box<dyn std::error::Error>> {
    println!("Current version: v{}", env!("CARGO_PKG_VERSION"));

    if is_homebrew_install() {
        println!();
        println!("Suvadu was installed via Homebrew. To update, run:");
        println!();
        println!("  \x1b[36mbrew upgrade suvadu\x1b[0m");
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

    // On Linux, detect architecture for ARM64 support
    let arch_suffix = if platform == "linux" && std::env::consts::ARCH == "aarch64" {
        "-aarch64"
    } else {
        ""
    };

    let base_url = format!("https://downloads.appachi.tech/{platform}");
    let archive_name = format!("suv-{platform}{arch_suffix}-latest.tar.gz");
    let archive_url = format!("{base_url}/{archive_name}");
    let checksum_url = format!("{base_url}/{archive_name}.sha256");

    // Use tempfile::TempDir for secure temp directory (0700 permissions, auto-cleanup on drop).
    let update_dir = tempfile::TempDir::new()?;

    let tarball_path = update_dir.path().join("suv-update.tar.gz");
    let binary_path = update_dir.path().join("suv");

    let tarball_str = tarball_path.to_string_lossy().to_string();
    let binary_str = binary_path.to_string_lossy().to_string();
    let update_dir_str = update_dir.path().to_string_lossy().to_string();

    // 1. Download archive
    println!("Downloading {platform_label} build from: {archive_url}");
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "-m", "300", "-o", &tarball_str, &archive_url])
        .status()?;

    if !status.success() {
        return Err("Failed to download update. Please check your internet connection.".into());
    }

    // 2. Download and verify checksum
    let checksum_result = std::process::Command::new("curl")
        .args(["-fsSL", "-m", "30", &checksum_url])
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
                    .args(["-a", "256", &tarball_str])
                    .output()?
            } else {
                std::process::Command::new("sha256sum")
                    .arg(&tarball_str)
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
        }
        _ => {
            return Err(
                "Could not fetch checksum for verification. Aborting update for security.".into(),
            );
        }
    }

    // 3. Extract (--no-same-owner prevents ownership issues)
    let status = std::process::Command::new("tar")
        .args([
            "--no-same-owner",
            "-xzf",
            &tarball_str,
            "-C",
            &update_dir_str,
        ])
        .status()?;

    if !status.success() {
        return Err("Failed to extract update archive.".into());
    }

    // Validate extracted binary is within temp dir (defense against tar path traversal)
    if !binary_path.exists() {
        return Err("Expected binary not found after extraction.".into());
    }
    let canonical_binary = binary_path.canonicalize()?;
    let canonical_dir = update_dir.path().canonicalize()?;
    if !canonical_binary.starts_with(&canonical_dir) {
        return Err("Extracted binary is outside temp directory. Aborting for security.".into());
    }

    println!("  Download complete");
    println!();

    // Determine install path from current executable location
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

    // Install binary
    let status_bin = std::process::Command::new("sudo")
        .args(["cp", &binary_str, &*install_str])
        .status()?;

    // Create/update symlink
    let status_link = std::process::Command::new("sudo")
        .args(["ln", "-sf", &*install_str, &*symlink_path.to_string_lossy()])
        .status()?;

    // TempDir auto-cleans on drop

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
        // In the test environment, the binary is built by cargo in the target/ directory,
        // so it should NOT be detected as a Homebrew install.
        let result = is_homebrew_install();
        assert!(
            !result,
            "Test binary should not be detected as a Homebrew install"
        );
    }
}
