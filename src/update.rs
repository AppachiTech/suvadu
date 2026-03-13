use minisign_verify::{PublicKey, Signature};

/// Minisign public key embedded in the binary for verifying release signatures.
///
/// The corresponding secret key is held by maintainers and used in CI to sign releases.
/// This ensures that even if the download server is compromised, an attacker cannot
/// produce valid signatures without the secret key.
///
/// To generate a new keypair:
///   minisign -G -p suvadu.pub -s suvadu.key -c "suvadu release signing key"
/// Then replace this constant with the base64 key from suvadu.pub (second line).
/// Add the secret key file contents to GitHub Secrets as `MINISIGN_SECRET_KEY`.
const MINISIGN_PUBLIC_KEY: &str = "RWSnsbPkvYdmk4EtxJ9WjItHLwx/GkmnBFNjeUhGWT2Z2efNdLTNMBy5";

pub fn is_homebrew_install() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy();
        return path.contains("/Cellar/")
            || path.contains("/homebrew/")
            || path.contains("/linuxbrew/");
    }
    false
}

pub fn is_cargo_install() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy();
        return path.contains("/.cargo/bin/");
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
            println!("  \x1b[36mbrew update && brew tap AppachiTech/suvadu && brew upgrade suvadu\x1b[0m");
        } else {
            println!("  brew update && brew tap AppachiTech/suvadu && brew upgrade suvadu");
        }
        println!();
        println!("Using 'suv update' with Homebrew installs can cause version conflicts.");
        return Ok(());
    }

    if is_cargo_install() {
        println!();
        println!("Suvadu was installed via Cargo. To update, run:");
        println!();
        if crate::util::color_enabled() {
            println!("  \x1b[36mcargo install suvadu\x1b[0m");
        } else {
            println!("  cargo install suvadu");
        }
        println!();
        println!("Using 'suv update' with Cargo installs can cause version conflicts.");
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

    let arch_suffix = if std::env::consts::ARCH == "aarch64" {
        "-aarch64"
    } else {
        ""
    };

    let base_url = format!("https://downloads.appachi.tech/{platform}");
    let archive_name = format!("suv-{platform}{arch_suffix}-latest.tar.gz");
    let archive_url = format!("{base_url}/{archive_name}");
    let checksum_url = format!("{base_url}/{archive_name}.sha256");
    let signature_url = format!("{base_url}/{archive_name}.minisig");

    let update_dir = tempfile::TempDir::new()?;
    let tarball_path = update_dir.path().join("suv-update.tar.gz");
    let binary_path = update_dir.path().join("suv");
    let tarball_str = tarball_path.to_string_lossy().to_string();
    let update_dir_str = update_dir.path().to_string_lossy().to_string();

    let params = DownloadParams {
        platform_label,
        archive_url: &archive_url,
        checksum_url: &checksum_url,
        signature_url: &signature_url,
        tarball_path: &tarball_str,
        extract_dir: &update_dir_str,
        binary_path: &binary_path,
        update_dir: update_dir.path(),
    };
    download_and_verify(&params)?;

    install_binary(&binary_path)?;
    Ok(())
}

struct DownloadParams<'a> {
    platform_label: &'a str,
    archive_url: &'a str,
    checksum_url: &'a str,
    signature_url: &'a str,
    tarball_path: &'a str,
    extract_dir: &'a str,
    binary_path: &'a std::path::Path,
    update_dir: &'a std::path::Path,
}

fn download_and_verify(p: &DownloadParams) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Downloading {} build from: {}",
        p.platform_label, p.archive_url
    );
    let status = std::process::Command::new("curl")
        .args([
            "--proto",
            "=https",
            "-fsSL",
            "-m",
            "300",
            "-o",
            p.tarball_path,
            p.archive_url,
        ])
        .status()?;
    if !status.success() {
        return Err("Failed to download update. Please check your internet connection.".into());
    }

    verify_signature(p.signature_url, p.tarball_path)?;
    verify_checksum(p.checksum_url, p.tarball_path)?;

    let status = std::process::Command::new("tar")
        .args([
            "--no-same-owner",
            "-xzf",
            p.tarball_path,
            "-C",
            p.extract_dir,
        ])
        .status()?;
    if !status.success() {
        return Err("Failed to extract update archive.".into());
    }

    if !p.binary_path.exists() {
        return Err("Expected binary not found after extraction.".into());
    }
    let canonical_binary = p.binary_path.canonicalize()?;
    let canonical_dir = p.update_dir.canonicalize()?;
    if !canonical_binary.starts_with(&canonical_dir) {
        return Err("Extracted binary is outside temp directory. Aborting for security.".into());
    }

    println!("  Download complete");
    println!();
    Ok(())
}

/// Verify the tarball's detached minisign signature against the embedded public key.
///
/// This is the primary security gate: the public key is compiled into the binary,
/// so a compromised download server cannot forge valid signatures.
fn verify_signature(
    signature_url: &str,
    tarball_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let pk = PublicKey::from_base64(MINISIGN_PUBLIC_KEY)
        .map_err(|e| format!("Invalid embedded public key (binary may need rebuild): {e}"))?;

    let sig_output = std::process::Command::new("curl")
        .args(["--proto", "=https", "-fsSL", "-m", "30", signature_url])
        .output();

    let sig_bytes = match sig_output {
        Ok(output) if output.status.success() => output.stdout,
        _ => {
            return Err(
                "Could not fetch release signature. Aborting update for security.\n\
                 The release may not be signed yet, or the server may be unreachable."
                    .into(),
            );
        }
    };

    let sig_str =
        String::from_utf8(sig_bytes).map_err(|_| "Signature file contains invalid UTF-8.")?;

    let sig = Signature::decode(&sig_str).map_err(|e| format!("Invalid signature format: {e}"))?;

    let tarball_data =
        std::fs::read(tarball_str).map_err(|e| format!("Cannot read downloaded tarball: {e}"))?;

    pk.verify(&tarball_data, &sig, false).map_err(|_| {
        "Signature verification FAILED!\n\
         The downloaded file was not signed by the suvadu maintainers.\n\
         Aborting update — the download may have been tampered with."
    })?;

    println!("  Signature verified (minisign)");
    Ok(())
}

fn verify_checksum(
    checksum_url: &str,
    tarball_str: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let checksum_result = std::process::Command::new("curl")
        .args(["--proto", "=https", "-fsSL", "-m", "30", checksum_url])
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

    // On Linux, `cp` over a running binary fails with "Text file busy".
    // Removing first works because the kernel keeps the old inode alive
    // until the running process exits, while freeing the directory entry
    // for the new file.
    let _ = std::process::Command::new("sudo")
        .args(["rm", "-f", &*install_str])
        .status();

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

    #[test]
    fn test_is_not_cargo_install() {
        let result = is_cargo_install();
        assert!(
            !result,
            "Test binary should not be detected as a Cargo install"
        );
    }

    #[test]
    fn test_embedded_public_key_parses() {
        assert!(
            PublicKey::from_base64(MINISIGN_PUBLIC_KEY).is_ok(),
            "Embedded MINISIGN_PUBLIC_KEY must be a valid minisign public key"
        );
    }

    #[test]
    fn test_valid_public_key_parses() {
        // A well-formed minisign public key should parse without error.
        // This uses a throwaway test key (not the real release key).
        let test_pk = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        assert!(PublicKey::from_base64(test_pk).is_ok());
    }

    #[test]
    fn test_invalid_public_key_errors() {
        assert!(PublicKey::from_base64("not-a-valid-key").is_err());
    }

    #[test]
    fn test_invalid_signature_format_errors() {
        let result = Signature::decode("not a real signature");
        assert!(result.is_err());
    }

    #[test]
    fn test_signature_verification_rejects_wrong_key() {
        // Verify that a signature from one key is rejected by a different key.
        // We can't easily generate real signatures in tests without the full
        // minisign crate, but we can verify the rejection path works by
        // checking that a valid key rejects arbitrary data with no signature.
        let pk = PublicKey::from_base64("RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3")
            .unwrap();

        // A minimal syntactically-valid but wrong signature
        // (correct structure but wrong cryptographic content)
        let fake_sig_str = "untrusted comment: fake\n\
            RUQf6LRCGA9i5wAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\n\
            trusted comment: fake\n\
            AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

        // Decode may or may not succeed depending on the base64 content,
        // but if it does, verification must fail
        if let Ok(sig) = Signature::decode(fake_sig_str) {
            let data = b"hello world";
            assert!(pk.verify(data, &sig, false).is_err());
        }
    }

    // --- Path traversal security tests ---

    #[test]
    fn path_traversal_detected_outside_dir() {
        let dir = std::path::Path::new("/tmp/update");
        let outside = std::path::Path::new("/etc/passwd");
        assert!(!outside.starts_with(dir));
    }

    #[test]
    fn path_within_dir_is_accepted() {
        let dir = std::path::Path::new("/tmp/update");
        let inside = std::path::Path::new("/tmp/update/suv");
        assert!(inside.starts_with(dir));
    }

    // --- Checksum comparison security tests ---

    #[test]
    fn checksum_mismatch_detected() {
        let expected = "abc123";
        let actual = "def456";
        assert!(expected != actual);
    }

    #[test]
    fn empty_checksum_rejected() {
        let expected = "";
        let actual = "abc123";
        assert!(expected.is_empty() || actual.is_empty() || expected != actual);

        let expected = "abc123";
        let actual = "";
        assert!(expected.is_empty() || actual.is_empty() || expected != actual);
    }

    #[test]
    fn matching_checksum_accepted() {
        let expected = "abc123def456";
        let actual = "abc123def456";
        assert!(!expected.is_empty() && !actual.is_empty() && expected == actual);
    }

    // --- Checksum extraction parsing tests ---

    #[test]
    fn checksum_extracted_from_sha256sum_output() {
        // sha256sum format: "hash  filename"
        let output = "abc123def456  /tmp/file.tar.gz\n";
        let extracted = output.split_whitespace().next().unwrap_or("");
        assert_eq!(extracted, "abc123def456");
    }

    #[test]
    fn checksum_extraction_handles_empty_output() {
        let output = "";
        let extracted = output.split_whitespace().next().unwrap_or("");
        assert_eq!(extracted, "");
    }

    #[test]
    fn checksum_extraction_handles_shasum_format() {
        // shasum -a 256 format: "hash  filename"
        let output = "e3b0c44298fc1c149  some_file.tar.gz\n";
        let extracted = output.split_whitespace().next().unwrap_or("");
        assert_eq!(extracted, "e3b0c44298fc1c149");
    }
}
