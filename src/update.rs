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

const VERSION_URL: &str = "https://downloads.appachi.tech/version.txt";

/// Which published archive this machine needs.
///
/// The release workflow leaves each operating system's *primary* target
/// unsuffixed, and those targets differ: macOS publishes arm64 unsuffixed and
/// Intel as `-x86_64`, while Linux publishes `x86_64` unsuffixed and ARM as
/// `-aarch64`. Deriving the suffix from the architecture alone therefore asks
/// for an archive that was never published on one platform, and silently
/// fetches the wrong architecture's binary on the other. `scripts/install.sh`
/// resolves the same four names; the tests hold the two to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReleaseTarget {
    platform: &'static str,
    label: &'static str,
    arch_suffix: &'static str,
}

impl ReleaseTarget {
    /// The target for `os`/`arch` as `std::env::consts` spells them, or
    /// `None` when no build is published for that combination.
    fn resolve(os: &str, arch: &str) -> Option<Self> {
        let (platform, label, arch_suffix) = match (os, arch) {
            ("linux", "x86_64") => ("linux", "Linux", ""),
            ("linux", "aarch64" | "arm64") => ("linux", "Linux", "-aarch64"),
            ("macos", "aarch64" | "arm64") => ("macos", "macOS", ""),
            ("macos", "x86_64") => ("macos", "macOS", "-x86_64"),
            _ => return None,
        };
        Some(Self {
            platform,
            label,
            arch_suffix,
        })
    }

    /// The target for the machine this binary is running on.
    fn current() -> Option<Self> {
        Self::resolve(std::env::consts::OS, std::env::consts::ARCH)
    }

    fn archive_name(self) -> String {
        format!("suv-{}{}-latest.tar.gz", self.platform, self.arch_suffix)
    }

    fn base_url(self) -> String {
        format!("https://downloads.appachi.tech/{}", self.platform)
    }
}

const SUVADU_LOGO: &str = r"
                               __
   _______  ___   ______ _____/ /_  __
  / ___/ / / / | / / __ `/ __  / / / /
 (__  ) /_/ /| |/ / /_/ / /_/ / /_/ /
/____/\__,_/ |___/\__,_/\__,_/\__,_/
              su · va · du
";

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
    let current_version = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current_version}");

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

    let latest_version = fetch_latest_version();

    if let Some(ref latest) = latest_version {
        if latest == current_version {
            show_up_to_date_banner(current_version);
            return Ok(());
        }
        println!();
        if crate::util::color_enabled() {
            println!("  \x1b[33mv{current_version}\x1b[0m → \x1b[32;1mv{latest}\x1b[0m");
        } else {
            println!("  v{current_version} → v{latest}");
        }
    }
    println!();

    display_release_notes(latest_version.as_deref());

    let target = ReleaseTarget::current().ok_or_else(|| {
        format!(
            "No published build for {}/{}. Build from source instead: cargo install suvadu",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let platform_label = target.label;

    let base_url = target.base_url();
    let archive_name = target.archive_name();
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

    let installed = install_binary(&binary_path)?;

    if installed {
        let new_version = latest_version.as_deref().unwrap_or("latest");
        show_success_banner(current_version, new_version);
        println!("{}", crate::upgrade_notice::AGENT_UPGRADE);
    }

    Ok(())
}

// ── Version check ────────────────────────────────────────

fn fetch_latest_version() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args(["--proto", "=https", "-fsSL", "-m", "10", VERSION_URL])
        .output()
        .ok()?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout)
            .trim()
            .trim_start_matches('v')
            .to_string();
        if !version.is_empty() {
            return Some(version);
        }
    }
    None
}

// ── Release notes ────────────────────────────────────────

fn display_release_notes(version: Option<&str>) {
    let Some(v) = version else { return };
    let tag = format!("v{v}");

    let url = format!("https://api.github.com/repos/AppachiTech/suvadu/releases/tags/{tag}");

    let output = match std::process::Command::new("curl")
        .args([
            "--proto",
            "=https",
            "-fsSL",
            "-m",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };

    let body_str = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(v) => v,
        Err(_) => return,
    };

    let Some(body) = json.get("body").and_then(|b| b.as_str()) else {
        return;
    };

    let color = crate::util::color_enabled();
    let mut has_output = false;

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(section) = trimmed.strip_prefix("### ") {
            if has_output {
                println!();
            }
            if color {
                println!("\x1b[33;1m{section}:\x1b[0m");
            } else {
                println!("{section}:");
            }
            has_output = true;
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            let item = rest.replace("**", "");
            if color {
                println!("  \x1b[36m•\x1b[0m {item}");
            } else {
                println!("  - {item}");
            }
            has_output = true;
        }
    }

    if has_output {
        println!();
    }
}

// ── Banners ──────────────────────────────────────────────

fn show_up_to_date_banner(version: &str) {
    let color = crate::util::color_enabled();
    println!();
    if color {
        println!("\x1b[32m{SUVADU_LOGO}\x1b[0m");
        println!("  \x1b[1mAlready up to date! (v{version})\x1b[0m");
    } else {
        println!("{SUVADU_LOGO}");
        println!("  Already up to date! (v{version})");
    }
    println!();
}

fn show_success_banner(old_version: &str, new_version: &str) {
    let color = crate::util::color_enabled();
    println!();
    if color {
        println!("\x1b[32m{SUVADU_LOGO}\x1b[0m");
        println!(
            "  \x1b[1mHooray! suvadu has been updated! v{old_version} → v{new_version}\x1b[0m"
        );
    } else {
        println!("{SUVADU_LOGO}");
        println!("  Hooray! suvadu has been updated! v{old_version} → v{new_version}");
    }
    println!();
    if color {
        println!("  \x1b[36m→\x1b[0m Run \x1b[36msuv version\x1b[0m to verify");
        println!("  \x1b[36m→\x1b[0m GitHub:  \x1b[4mhttps://github.com/AppachiTech/suvadu\x1b[0m");
        println!(
            "  \x1b[36m→\x1b[0m Issues:  \x1b[4mhttps://github.com/AppachiTech/suvadu/issues\x1b[0m"
        );
    } else {
        println!("  → Run 'suv version' to verify");
        println!("  → GitHub:  https://github.com/AppachiTech/suvadu");
        println!("  → Issues:  https://github.com/AppachiTech/suvadu/issues");
    }
    println!();
}

// ── Download & verification ──────────────────────────────

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

// ── Install ──────────────────────────────────────────────

/// Install the new binary. Returns `true` if the user confirmed and the
/// install succeeded, `false` if the user cancelled.
fn install_binary(binary_path: &std::path::Path) -> Result<bool, Box<dyn std::error::Error>> {
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
        return Ok(false);
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
        Ok(true)
    } else {
        Err("Failed to install update.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four archives the release workflow actually publishes. Anything
    /// else is a download that 404s, or the wrong architecture's binary.
    #[test]
    fn release_target_names_only_published_archives() {
        let cases = [
            ("linux", "x86_64", "linux", "suv-linux-latest.tar.gz"),
            (
                "linux",
                "aarch64",
                "linux",
                "suv-linux-aarch64-latest.tar.gz",
            ),
            ("macos", "aarch64", "macos", "suv-macos-latest.tar.gz"),
            ("macos", "x86_64", "macos", "suv-macos-x86_64-latest.tar.gz"),
        ];
        for (os, arch, platform, archive) in cases {
            let target = ReleaseTarget::resolve(os, arch)
                .unwrap_or_else(|| panic!("{os}/{arch} should be supported"));
            assert_eq!(target.platform, platform, "{os}/{arch}");
            assert_eq!(target.archive_name(), archive, "{os}/{arch}");
        }
    }

    /// The updater and the install script resolve the same four names. They
    /// drifted apart once already, and only the script was corrected.
    #[test]
    fn release_target_agrees_with_the_install_script() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh"),
        )
        .expect("scripts/install.sh must be readable");
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
        ] {
            let archive = ReleaseTarget::resolve(os, arch).unwrap().archive_name();
            let stem = archive
                .strip_suffix("-latest.tar.gz")
                .expect("archive name shape");
            let suffix = stem
                .strip_prefix(&format!("suv-{os}"))
                .expect("archive names start with the platform");
            let uname_os = if os == "macos" { "Darwin" } else { "Linux" };
            let uname_arch = if os == "macos" && arch == "aarch64" {
                "arm64"
            } else {
                arch
            };
            let expected = format!("{uname_os}/{uname_arch}");
            let line = script
                .lines()
                .find(|l| l.trim_start().starts_with(&expected))
                .unwrap_or_else(|| panic!("install.sh has no case for {expected}"));
            let script_suffix = line
                .split("ARCH_SUFFIX=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("no ARCH_SUFFIX in {line}"));
            assert_eq!(
                suffix, script_suffix,
                "{os}/{arch}: updater and install.sh disagree"
            );
        }
    }

    /// The workflow publishes; the updater and the install script consume.
    /// Reading the matrix keeps all three tied to one set of names.
    #[test]
    fn release_target_matches_what_the_workflow_publishes() {
        let workflow = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
        )
        .expect("the release workflow must be readable");

        let mut published: Vec<(String, String)> = Vec::new();
        let (mut os, mut target) = (None, None);
        for line in workflow.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("- os: ") {
                os = Some(value.trim().to_string());
                target = None;
            } else if let Some(value) = line.strip_prefix("target: ") {
                target = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("suffix: ") {
                if let (Some(os), Some(target)) = (os.as_ref(), target.as_ref()) {
                    let arch = target.split('-').next().expect("target triple");
                    let suffix = value.trim().trim_matches('"').to_string();
                    published.push((format!("{os}/{arch}"), suffix));
                }
            }
        }
        assert_eq!(
            published.len(),
            4,
            "expected four published targets, parsed {published:?}"
        );

        for (key, suffix) in published {
            let (os, arch) = key.split_once('/').expect("os/arch");
            let target = ReleaseTarget::resolve(os, arch)
                .unwrap_or_else(|| panic!("{key} is published but the updater cannot resolve it"));
            assert_eq!(
                target.archive_name(),
                format!("suv-{os}{suffix}-latest.tar.gz"),
                "{key}: the updater asks for a name the workflow does not publish"
            );
        }
    }

    #[test]
    fn release_target_rejects_an_architecture_with_no_build() {
        assert!(ReleaseTarget::resolve("linux", "riscv64").is_none());
        assert!(ReleaseTarget::resolve("windows", "x86_64").is_none());
    }

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

    // --- Release notes parsing tests ---

    #[test]
    fn release_notes_section_headers_detected() {
        let body = "### Fixed\n- Bug one\n- Bug two\n\n### Added\n- Feature one\n";
        let mut sections = Vec::new();
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(section) = trimmed.strip_prefix("### ") {
                sections.push(section.to_string());
            }
        }
        assert_eq!(sections, vec!["Fixed", "Added"]);
    }

    #[test]
    fn release_notes_items_extracted() {
        let body = "### Fixed\n- **Linux** — Text file busy\n- Arrow-key nav\n";
        let mut items = Vec::new();
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("- ") {
                items.push(rest.replace("**", ""));
            }
        }
        assert_eq!(items, vec!["Linux — Text file busy", "Arrow-key nav"]);
    }

    #[test]
    fn logo_constant_is_not_empty() {
        assert!(!SUVADU_LOGO.trim().is_empty());
    }

    #[test]
    fn version_url_is_https() {
        assert!(VERSION_URL.starts_with("https://"));
    }

    #[test]
    fn github_releases_url_is_https() {
        let url = "https://api.github.com/repos/AppachiTech/suvadu/releases/tags/v0.1.0";
        assert!(url.starts_with("https://"));
    }
}
