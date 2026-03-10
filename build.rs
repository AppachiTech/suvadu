fn main() {
    let now = chrono::Local::now();
    println!("cargo:rustc-env=BUILD_DATE={}", now.format("%Y-%m-%d"));

    // Embed short git hash if available
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=BUILD_HASH={hash}");
}
