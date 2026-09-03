fn main() {
    // Rerun if HEAD changes (branch switch, commit).
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    // Also rerun when the refs directory changes (branch tip updates).
    println!("cargo:rerun-if-changed=../../.git/refs");

    let short_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=SKIM_GIT_COMMIT={short_sha}");
}
