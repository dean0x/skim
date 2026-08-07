//! `skim doctor` — installation health check and provenance report.
//!
//! Scans $PATH for all skim installations, checks hook state, wrapper dir,
//! cache/analytics paths, and staleness vs. repository HEAD.
//!
//! Exit codes:
//!   0 = healthy (no drift detected)
//!   1 = drift (unpinned hook, path/version/commit mismatch, or ≥1 commit behind HEAD)

use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use crate::analytics::AnalyticsConfig;
use crate::cmd::session::AgentKind;

/// Timeout for each bounded subprocess invocation.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(5);

// ============================================================================
// Public entry points
// ============================================================================

/// Run the `skim doctor` subcommand.
pub(crate) fn run(args: &[String], _analytics: &AnalyticsConfig) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let mut drift = false;

    let compiled_version = env!("CARGO_PKG_VERSION");
    let compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown");

    // 1. Running binary
    let running_path = current_exe_canonical();
    println!("Running binary");
    match &running_path {
        Some(p) => println!(
            "  {} (v{compiled_version}, commit {compiled_commit})",
            p.display()
        ),
        None => println!("  (cannot determine running binary path)"),
    }
    println!();

    // 2. $PATH scan
    let path_entries = scan_path_for_skim(running_path.as_deref());
    let path_drift = print_path_section(&path_entries);
    if path_drift {
        drift = true;
    }
    println!();

    // 3. Hooks
    let hook_drift = print_hook_section(compiled_version, compiled_commit)?;
    if hook_drift {
        drift = true;
    }
    println!();

    // 4. Wrappers
    print_wrapper_section();
    println!();

    // 5. Cache & Analytics
    print_cache_section();
    println!();

    // 6. Staleness vs. repo HEAD
    let staleness_drift = print_staleness_section(compiled_commit);
    if staleness_drift {
        drift = true;
    }
    println!();

    if drift {
        println!("Status: DRIFT DETECTED — exit 1");
        Ok(ExitCode::FAILURE)
    } else {
        println!("Status: HEALTHY — exit 0");
        Ok(ExitCode::SUCCESS)
    }
}

/// Build the clap `Command` definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("doctor").about("Check skim installation health and report provenance drift")
}

// ============================================================================
// Running binary
// ============================================================================

fn current_exe_canonical() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
}

// ============================================================================
// $PATH scan
// ============================================================================

struct PathEntry {
    path: PathBuf,
    version: Option<String>,
    commit: Option<String>,
    /// True when this path is the currently running binary.
    is_running: bool,
    /// True when this is the first (winning) entry on $PATH.
    wins: bool,
}

/// Scan $PATH directories for `skim` executables and return ordered entries.
///
/// Deduplicates by canonical path so symlinks and wrapper-dir entries that
/// resolve to the same binary appear only once. The first executable found
/// on PATH is marked as the winner (standard Unix PATH semantics).
fn scan_path_for_skim(running_path: Option<&std::path::Path>) -> Vec<PathEntry> {
    let path_var = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut entries: Vec<PathEntry> = Vec::new();
    let mut winner_set = false;

    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("skim");
        if !is_executable(&candidate) {
            continue;
        }

        // Canonicalize to resolve symlinks and normalise paths.
        let canonical = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());

        // Skip duplicates (same canonical path already listed).
        let already_listed = entries.iter().any(|e| e.path == canonical);
        if already_listed {
            continue;
        }

        let is_running = running_path.map(|rp| rp == canonical).unwrap_or(false);

        let wins = !winner_set;
        if wins {
            winner_set = true;
        }

        let version = query_binary_version(&canonical);
        let commit = query_binary_commit(&canonical);

        entries.push(PathEntry {
            path: canonical,
            version,
            commit,
            is_running,
            wins,
        });
    }

    entries
}

/// Return true when `path` points to an executable regular file.
fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Query the version from an arbitrary skim binary via `--version`.
///
/// Clap outputs `skim X.Y.Z (COMMIT)\n`; we extract the `X.Y.Z` part.
fn query_binary_version(path: &std::path::Path) -> Option<String> {
    let output = run_bounded(
        {
            let path = path.to_path_buf();
            move || {
                std::process::Command::new(&path)
                    .arg("--version")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
            }
        },
        SUBPROCESS_TIMEOUT,
    )
    .flatten()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    parse_version_from_version_output(&text)
}

/// Parse version from clap `--version` output: `skim X.Y.Z (sha)\n`.
///
/// Strips the `skim ` prefix and takes everything before ` (` or end of line.
fn parse_version_from_version_output(text: &str) -> Option<String> {
    let after_name = text.trim().strip_prefix("skim ")?;
    let version = after_name.split(" (").next()?.trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Query the git commit from an arbitrary skim binary via `--commit`.
///
/// `--commit` is a hidden early-exit flag added in B4: it prints the
/// compiled `SKIM_GIT_COMMIT` and exits 0. Old binaries without the flag
/// return non-zero; we treat that as "unknown commit".
fn query_binary_commit(path: &std::path::Path) -> Option<String> {
    let output = run_bounded(
        {
            let path = path.to_path_buf();
            move || {
                std::process::Command::new(&path)
                    .arg("--commit")
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
            }
        },
        SUBPROCESS_TIMEOUT,
    )
    .flatten()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() || text == "unknown" {
        None
    } else {
        Some(text)
    }
}

/// Print the $PATH section and return true if drift is detected.
///
/// Drift: the first skim entry on $PATH is not the running binary.
fn print_path_section(entries: &[PathEntry]) -> bool {
    println!("$PATH  ({} skim found)", entries.len());

    if entries.is_empty() {
        println!("  (no skim found on $PATH)");
        return false;
    }

    let mut drift = false;

    for entry in entries {
        let marker = if entry.wins { "→" } else { "  " };
        let version_str = entry.version.as_deref().unwrap_or("?");
        let commit_str = entry.commit.as_deref().unwrap_or("?");

        let mut tags = Vec::new();
        if entry.is_running {
            tags.push("[running]");
        }
        if entry.wins && !entry.is_running {
            tags.push("[WINS — not the running binary]");
            drift = true;
        }
        let tag = if tags.is_empty() {
            String::new()
        } else {
            format!("  {}", tags.join(" "))
        };

        println!(
            "  {} {}  v{}  commit {}{}",
            marker,
            entry.path.display(),
            version_str,
            commit_str,
            tag,
        );
    }

    drift
}

// ============================================================================
// Hooks
// ============================================================================

/// Print the hooks section and return true if any drift is detected.
fn print_hook_section(compiled_version: &str, compiled_commit: &str) -> anyhow::Result<bool> {
    println!("Hooks");
    let mut any_drift = false;

    for &agent in AgentKind::all_supported() {
        let facts = match crate::cmd::init::hook_facts(agent) {
            Ok(f) => f,
            Err(e) => {
                // Detection failure is not itself drift — report and continue.
                println!("  –  {}  (detection error: {e})", agent.cli_name());
                continue;
            }
        };

        if !facts.hook_installed {
            println!("  –  {}  not installed", agent.cli_name());
            continue;
        }

        let hook_version = facts.hook_version.as_deref().unwrap_or("?");
        let hook_commit_str = facts.hook_commit.as_deref().unwrap_or("?");
        let script = facts.hook_script_path.display().to_string();

        if !facts.hook_uses_pinned_binary {
            // Unpinned hook format — drift.
            any_drift = true;
            println!(
                "  ✗ {}  installed (v{hook_version})  unpinned — run `skim init --yes` to pin",
                agent.cli_name()
            );
        } else if !facts.hook_is_current {
            // Pinned but version or commit mismatch — drift.
            any_drift = true;
            let pin = facts.hook_binary_pin.as_deref().unwrap_or("?");
            let version_ok = facts.hook_version.as_deref() == Some(compiled_version);
            let commit_ok = facts.hook_commit.as_deref() == Some(compiled_commit);
            let reason = if !version_ok {
                format!("version mismatch (hook: {hook_version}, binary: {compiled_version})")
            } else if !commit_ok {
                format!("commit mismatch (hook: {hook_commit_str}, binary: {compiled_commit})")
            } else {
                "stale".to_string()
            };
            println!(
                "  ✗ {}  installed  pin: {pin}  [{reason}]  — run `skim init --yes` to update",
                agent.cli_name()
            );
        } else {
            // Fully current.
            let pin = facts.hook_binary_pin.as_deref().unwrap_or("?");
            println!(
                "  ✓ {}  installed (v{hook_version}, commit {hook_commit_str})  pin: {pin}",
                agent.cli_name()
            );
        }

        println!("       script: {script}");
    }

    Ok(any_drift)
}

// ============================================================================
// Wrappers
// ============================================================================

fn print_wrapper_section() {
    println!("Wrappers  (~/.skim/bin)");

    let wrappers_dir = match crate::cmd::skim_wrappers_dir() {
        Some(d) => d,
        None => {
            println!("  (cannot determine home directory)");
            return;
        }
    };

    if !wrappers_dir.exists() {
        println!("  –  not installed  (run `skim init --wrappers` to install)");
        return;
    }

    let count = std::fs::read_dir(wrappers_dir)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().ok().is_some_and(|ft| ft.is_symlink()))
                .count()
        })
        .unwrap_or(0);

    println!("  ✓  {}  ({count} symlinks)", wrappers_dir.display());
}

// ============================================================================
// Cache & Analytics
// ============================================================================

fn print_cache_section() {
    println!("Cache & Analytics");

    let cache_env = crate::cmd::hook_log::CacheEnv::from_process();
    match cache_env.resolve_cache_dir() {
        Some(ref dir) => println!("  Cache dir:    {}", dir.display()),
        None => println!("  Cache dir:    (unknown)"),
    }

    // Analytics DB: SKIM_ANALYTICS_DB takes precedence over SKIM_CACHE_DIR.
    let db_from_env = std::env::var("SKIM_ANALYTICS_DB")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let db_path = db_from_env.as_deref().map(PathBuf::from).or_else(|| {
        cache_env
            .resolve_cache_dir()
            .map(|d| d.join("analytics.db"))
    });

    match db_path {
        Some(ref p) => {
            let exists_marker = if p.exists() {
                " (exists)"
            } else {
                " (not found)"
            };
            let source_marker = if db_from_env.is_some() {
                "  [SKIM_ANALYTICS_DB]"
            } else {
                ""
            };
            println!(
                "  Analytics DB: {}{exists_marker}{source_marker}",
                p.display()
            );
        }
        None => println!("  Analytics DB: (unknown)"),
    }
}

// ============================================================================
// Staleness vs. repo HEAD
// ============================================================================

/// Print the staleness section and return true if drift is detected.
///
/// Staleness algorithm (B4):
///   1. Skip when `SKIM_GIT_COMMIT` is "unknown" (tarball/non-git build).
///   2. `git cat-file -e <sha>^{commit}` — if absent, the binary was built
///      from a different repository (not a behind-count).
///   3. `git rev-list <sha>..HEAD --count` — number of commits between the
///      compiled SHA and HEAD.
///
/// Every git invocation is bounded by [`SUBPROCESS_TIMEOUT`].
fn print_staleness_section(compiled_commit: &str) -> bool {
    println!("Staleness  (binary vs. repo HEAD)");

    if compiled_commit == "unknown" {
        println!("  –  commit not embedded (tarball/non-git build) — cannot check staleness");
        return false;
    }

    // Gate: git must be present.
    let git_ok = run_bounded(
        || {
            std::process::Command::new("git")
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        },
        SUBPROCESS_TIMEOUT,
    )
    .unwrap_or(false);

    if !git_ok {
        println!("  –  git not found on $PATH — cannot check staleness");
        return false;
    }

    // Gate: must be inside a git repository.
    let in_repo = run_bounded(
        || {
            std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        },
        SUBPROCESS_TIMEOUT,
    )
    .unwrap_or(false);

    if !in_repo {
        println!(
            "  –  not inside a git repository — run `skim doctor` from the skim source directory"
        );
        return false;
    }

    // Critical: check object exists FIRST (B4 staleness algorithm).
    // `git cat-file -e <sha>^{commit}` exits 0 iff the object is a commit in this repo.
    // If it fails → binary was built from a different repository, not a count-behind case.
    let commit_exists = run_bounded(
        {
            let sha = compiled_commit.to_string();
            move || {
                let obj_ref = format!("{sha}^{{commit}}");
                std::process::Command::new("git")
                    .args(["cat-file", "-e", &obj_ref])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            }
        },
        SUBPROCESS_TIMEOUT,
    )
    .unwrap_or(false);

    if !commit_exists {
        println!(
            "  ✗  SHA {compiled_commit} not found in this repo — \
             built from a different repository"
        );
        return true;
    }

    // Count commits between compiled SHA and HEAD.
    let count_str = run_bounded(
        {
            let sha = compiled_commit.to_string();
            move || {
                let range = format!("{sha}..HEAD");
                std::process::Command::new("git")
                    .args(["rev-list", &range, "--count"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
        },
        SUBPROCESS_TIMEOUT,
    )
    .flatten();

    match count_str.as_deref() {
        Some("0") | Some("") => {
            println!("  ✓  up to date  (commit {compiled_commit} is HEAD)");
            false
        }
        Some(n) => {
            let drift = n.parse::<u64>().map(|v| v >= 1).unwrap_or(true);
            if drift {
                println!(
                    "  ✗  {n} commit(s) behind HEAD  \
                     (rebuild: cargo build -p rskim --release)"
                );
            } else {
                println!("  ✓  up to date  (commit {compiled_commit} is HEAD)");
            }
            drift
        }
        None => {
            println!("  –  could not determine staleness (git rev-list failed)");
            false
        }
    }
}

// ============================================================================
// Bounded subprocess helper
// ============================================================================

/// Run `f` in a background thread and wait at most `timeout` for the result.
///
/// Returns `None` when the timeout fires before `f` returns. The background
/// thread is abandoned (not killed) — acceptable for short-lived diagnostics.
///
/// This satisfies the reliability rule "every subprocess has an explicit bound":
/// the caller never waits more than `timeout`, even if the subprocess stalls.
fn run_bounded<F, T>(f: F, timeout: Duration) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).ok()
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim doctor");
    println!();
    println!("  Check skim installation health and report provenance drift");
    println!();
    println!("Usage: skim doctor");
    println!();
    println!("Reports:");
    println!("  Running binary path, version, and commit");
    println!("  All skim entries on $PATH with version and commit (→ marks the winner)");
    println!("  Hook installation state for each supported agent");
    println!("      Shows: path, version, commit, pinned binary, current/stale status");
    println!("  Wrapper directory (~/.skim/bin) status and symlink count");
    println!("  Cache directory and analytics DB paths");
    println!("  Staleness vs. HEAD (run from the skim source directory for accurate results)");
    println!();
    println!("Exit codes:");
    println!("  0 = healthy");
    println!("  1 = drift (unpinned hook, version/commit mismatch, or ≥1 commit behind HEAD)");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ANALYTICS: AnalyticsConfig = AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };

    #[test]
    fn test_doctor_run_no_crash() {
        // Smoke test: doctor must not panic or return an error.
        let result = run(&[], &TEST_ANALYTICS);
        assert!(
            result.is_ok(),
            "doctor should not return an error: {:?}",
            result
        );
    }

    #[test]
    fn test_doctor_help_flag() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_version_from_version_output_standard() {
        assert_eq!(
            parse_version_from_version_output("skim 2.11.0 (abc1234)\n"),
            Some("2.11.0".to_string())
        );
    }

    #[test]
    fn test_parse_version_from_version_output_no_commit() {
        assert_eq!(
            parse_version_from_version_output("skim 2.11.0\n"),
            Some("2.11.0".to_string())
        );
    }

    #[test]
    fn test_parse_version_from_version_output_wrong_prefix() {
        // Not a skim binary — no "skim " prefix.
        assert_eq!(parse_version_from_version_output("other 1.0\n"), None);
    }

    #[test]
    fn test_parse_version_from_version_output_empty() {
        assert_eq!(parse_version_from_version_output(""), None);
    }

    #[test]
    fn test_run_bounded_returns_value() {
        let result = run_bounded(|| 42u32, Duration::from_secs(2));
        assert_eq!(result, Some(42));
    }

    #[test]
    fn test_run_bounded_timeout_returns_none() {
        // 60-second sleep, 50ms timeout → must return None without blocking.
        let result = run_bounded(
            || {
                std::thread::sleep(Duration::from_secs(60));
                42u32
            },
            Duration::from_millis(50),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn test_print_path_section_empty_no_drift() {
        let drift = print_path_section(&[]);
        assert!(!drift, "empty PATH scan must not report drift");
    }

    #[test]
    fn test_print_path_section_running_wins_no_drift() {
        let entries = vec![PathEntry {
            path: PathBuf::from("/usr/local/bin/skim"),
            version: Some("2.11.0".to_string()),
            commit: Some("abc1234".to_string()),
            is_running: true,
            wins: true,
        }];
        let drift = print_path_section(&entries);
        assert!(!drift, "running binary winning must not report drift");
    }

    #[test]
    fn test_print_path_section_other_wins_drift() {
        let entries = vec![PathEntry {
            path: PathBuf::from("/other/bin/skim"),
            version: Some("2.11.0".to_string()),
            commit: Some("def5678".to_string()),
            is_running: false,
            wins: true,
        }];
        let drift = print_path_section(&entries);
        assert!(drift, "non-running binary winning must report drift");
    }
}
