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

        let info = query_binary_info(&canonical);

        entries.push(PathEntry {
            path: canonical,
            version: info.version,
            commit: info.commit,
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

/// Combined binary info resolved in a single `--version` invocation.
struct BinaryInfo {
    version: Option<String>,
    commit: Option<String>,
}

/// Query version and commit from an arbitrary skim binary.
///
/// Strategy (single subprocess):
///   1. Invoke `--version`. Clap always outputs `skim X.Y.Z (COMMIT)\n`.
///      Parse both the version and the SHA from that single call.
///   2. If `--version` succeeded but produced no parenthetical (e.g. tarball/
///      crates.io builds baked without `SKIM_GIT_COMMIT`), fall back to `--commit`.
///      The `--commit` flag is hidden and was introduced in B4; old binaries reject
///      it, so a non-zero exit means "commit legitimately unknown".
///
/// This avoids spawning the child twice per PATH entry. Pre-B4 binaries that do
/// not support `--commit` still report the correct version+commit through
/// `--version` as long as they were built with `SKIM_GIT_COMMIT` set (i.e. any
/// git-based build, regardless of branch).
fn query_binary_info(path: &std::path::Path) -> BinaryInfo {
    // --- Step 1: --version (works on every skim binary) ---------------------
    let version_output = run_bounded(
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
    .flatten();

    let (version, commit_from_version) = match version_output {
        Some(ref o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let v = parse_version_from_version_output(&text);
            let c = parse_commit_from_version_output(&text);
            (v, c)
        }
        _ => (None, None),
    };

    // --- Step 2: --commit fallback (B4+ binaries only) ----------------------
    // Only invoked when --version didn't yield a commit (e.g. tarball builds
    // that omit the parenthetical). Old binaries return non-zero → None.
    let commit = if commit_from_version.is_some() {
        commit_from_version
    } else {
        query_binary_commit_flag(path)
    };

    BinaryInfo { version, commit }
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

/// Parse the git SHA from clap `--version` output: `skim X.Y.Z (sha)\n`.
///
/// Returns `None` when no parenthetical is present (tarball/non-git builds).
/// Does not panic on malformed input; treats any unparseable text as `None`.
fn parse_commit_from_version_output(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Format: "skim X.Y.Z (sha)"
    let after_name = trimmed.strip_prefix("skim ")?;
    let paren_pos = after_name.find(" (")?;
    let rest = after_name.get(paren_pos + 2..)?;
    let sha = rest.strip_suffix(')')?;
    let sha = sha.trim();
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

/// Query the git commit from an arbitrary skim binary via `--commit`.
///
/// `--commit` is a hidden early-exit flag added in B4: it prints the
/// compiled `SKIM_GIT_COMMIT` and exits 0. Old binaries without the flag
/// return non-zero; we treat that as "unknown commit".
///
/// Prefer `query_binary_info` which calls this only as a fallback after
/// `--version` yields no parenthetical.
fn query_binary_commit_flag(path: &std::path::Path) -> Option<String> {
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

/// Compute the hook status line and drift verdict for one agent.
///
/// Returns `(is_drift, status_line)`.
///
/// The four [`crate::cmd::integrity::ScriptIntegrity`] states are checked
/// **before** any pin/currency branches so that the verdict is always derived
/// from the SHA-256 manifest (an independent artefact) rather than from the
/// hook script bytes themselves — which is what a tamper modifies (PF-016).
///
/// Design decisions:
/// - `Tampered`   → drift (`✗`), names the suppression coupling.
/// - `Unreadable` → drift (`✗`), names the suppression coupling.
/// - `NoManifest` → advisory only (`⚠`), **not** drift — users who installed
///   before manifests existed have done nothing wrong and must not have their
///   `skim doctor` exit-0 broken (applies ADR-004 backward-compat intent).
/// - `Verified`   → fall through to existing pin/currency logic.
fn hook_status_line(
    facts: &crate::cmd::init::HookFacts,
    agent_cli_name: &str,
    compiled_version: &str,
    compiled_commit: &str,
) -> (bool, String) {
    use crate::cmd::integrity::ScriptIntegrity;

    if !facts.hook_installed {
        return (false, format!("  –  {agent_cli_name}  not installed"));
    }

    let hook_version = facts.hook_version.as_deref().unwrap_or("?");
    let hook_commit_str = facts.hook_commit.as_deref().unwrap_or("?");

    // Gate on integrity first — verdict is derived from the manifest, not from
    // the hook bytes that a tamper would modify.
    //
    // `no_manifest_advisory` captures the advisory message for `NoManifest` so
    // the function can fall through to pin/currency checks and append the note
    // to whatever verdict those produce (rather than returning early and silently
    // skipping drift detection — Group 3 fix).
    let no_manifest_advisory: Option<String> = match facts.script_integrity {
        ScriptIntegrity::Tampered => {
            return (
                true,
                format!(
                    "  ✗ {agent_cli_name}  installed — script tampered (hash mismatch); \
                     note: a failed integrity check also silences drift detection on this \
                     agent's hook channel — run `skim init --agent {agent_cli_name}` to reinstall"
                ),
            );
        }
        ScriptIntegrity::Unreadable => {
            return (
                true,
                format!(
                    "  ✗ {agent_cli_name}  installed — hook script unreadable (cannot hash); \
                     run `skim init --agent {agent_cli_name}` to reinstall"
                ),
            );
        }
        ScriptIntegrity::NoManifest => {
            // Advisory only — a pre-manifest install is not tampering.
            // Not setting drift so that existing installations do not regress
            // to exit 1 just because they predate the manifest feature.
            //
            // Fall through to the pin/currency checks below rather than returning
            // early: an install without a manifest still needs drift detection for
            // the pin and version state (#471 / Group 3).  The advisory is appended
            // to whatever verdict those checks produce.
            Some(format!(
                "⚠ no integrity manifest (pre-manifest install); \
                 run `skim init --agent {agent_cli_name}` to add tamper detection"
            ))
        }
        ScriptIntegrity::Verified => {
            // Hash verified — fall through to pin/currency checks below.
            None
        }
    };

    // Helper: append the no-manifest advisory (if any) to a status line.
    let append_advisory = |msg: String| -> String {
        if let Some(ref note) = no_manifest_advisory {
            format!("{msg}\n     {note}")
        } else {
            msg
        }
    };

    // NoManifest / Verified: check pin format and currency.
    if !facts.hook_uses_pinned_binary {
        return (
            true,
            append_advisory(format!(
                "  ✗ {agent_cli_name}  installed (v{hook_version})  unpinned — \
                 run `./target/release/skim init --yes` to pin"
            )),
        );
    }

    if !facts.hook_is_current || !facts.pin_is_current {
        let pin = facts.hook_binary_pin.as_deref().unwrap_or("?");
        let version_ok = facts.hook_version.as_deref() == Some(compiled_version);
        let commit_ok = facts.hook_commit.as_deref() == Some(compiled_commit);
        // Report the most specific cause first.
        let reason = if !commit_ok {
            format!("commit mismatch (hook: {hook_commit_str}, binary: {compiled_commit})")
        } else if !version_ok {
            format!("version mismatch (hook: {hook_version}, binary: {compiled_version})")
        } else if !facts.pin_is_current {
            // Version and commit match but the pin path differs: two clones at
            // the same commit.  Show both paths so the user can identify which
            // binary is running and which the hook points to.
            let running = std::env::current_exe()
                .ok()
                .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "?".to_string());
            format!("binary pin mismatch (hook: {pin}, running: {running})")
        } else {
            "stale".to_string()
        };
        return (
            true,
            append_advisory(format!(
                "  ✗ {agent_cli_name}  installed  pin: {pin}  [{reason}]  — \
                 run `./target/release/skim init --yes` to update"
            )),
        );
    }

    // Fully current (with or without manifest).
    let pin = facts.hook_binary_pin.as_deref().unwrap_or("?");
    (
        false,
        append_advisory(format!(
            "  ✓ {agent_cli_name}  installed (v{hook_version}, commit {hook_commit_str})  pin: {pin}"
        )),
    )
}

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

        let (drift, line) =
            hook_status_line(&facts, agent.cli_name(), compiled_version, compiled_commit);
        if drift {
            any_drift = true;
        }
        println!("{line}");

        // Script path is only meaningful for installed hooks.
        if facts.hook_installed {
            println!("       script: {}", facts.hook_script_path.display());
        }
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

    // ---- parse_commit_from_version_output ----

    #[test]
    fn test_parse_commit_from_version_output_standard() {
        // Happy path: "skim X.Y.Z (sha)" emits both version and commit.
        assert_eq!(
            parse_commit_from_version_output("skim 2.11.0 (d5695c1)\n"),
            Some("d5695c1".to_string())
        );
    }

    #[test]
    fn test_parse_commit_from_version_output_no_parenthetical() {
        // Tarball/crates.io build: no parenthetical → commit is legitimately unknown.
        assert_eq!(parse_commit_from_version_output("skim 2.11.0\n"), None);
    }

    #[test]
    fn test_parse_commit_from_version_output_wrong_prefix() {
        // Non-skim binary output must not panic and must return None.
        assert_eq!(
            parse_commit_from_version_output("other 1.0.0 (abc)\n"),
            None
        );
    }

    #[test]
    fn test_parse_commit_from_version_output_empty() {
        assert_eq!(parse_commit_from_version_output(""), None);
    }

    #[test]
    fn test_parse_commit_from_version_output_empty_parens() {
        // Malformed: empty parens must not produce an empty string commit.
        assert_eq!(parse_commit_from_version_output("skim 2.11.0 ()\n"), None);
    }

    // ---- hook_status_line: all four ScriptIntegrity states ----

    fn make_installed_facts(
        integrity: crate::cmd::integrity::ScriptIntegrity,
    ) -> crate::cmd::init::HookFacts {
        crate::cmd::init::HookFacts {
            hook_installed: true,
            hook_version: Some("2.11.0".to_string()),
            hook_commit: Some("abc1234".to_string()),
            hook_binary_pin: Some("/usr/local/bin/skim".to_string()),
            hook_uses_pinned_binary: true,
            hook_is_current: true,
            pin_is_current: true,
            hook_script_path: std::path::PathBuf::from("/some/path/skim-rewrite.sh"),
            script_integrity: integrity,
        }
    }

    #[test]
    fn test_hook_status_line_tampered_is_drift_and_actionable() {
        let facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::Tampered);
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "abc1234");

        assert!(drift, "Tampered must report drift");
        assert!(
            line.contains("tampered"),
            "message must name the tamper: {line}"
        );
        // The suppression coupling must be named.
        assert!(
            line.contains("silences drift detection"),
            "message must name the drift-detection suppression coupling: {line}"
        );
        assert!(
            line.contains("skim init"),
            "message must include skim init fix command: {line}"
        );
        assert!(
            line.contains("claude-code"),
            "message must name the agent for the fix command: {line}"
        );
    }

    #[test]
    fn test_hook_status_line_unreadable_is_drift_and_actionable() {
        let facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::Unreadable);
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "abc1234");

        assert!(drift, "Unreadable must report drift");
        assert!(
            line.contains("unreadable"),
            "message must name the unreadable state: {line}"
        );
        // Fix 4b: Unreadable does NOT silence drift detection (only Tampered does).
        // The message must NOT claim this.
        assert!(
            !line.contains("silences drift detection"),
            "Unreadable message must NOT claim drift detection is silenced (only Tampered does): {line}"
        );
        assert!(
            line.contains("skim init"),
            "message must include skim init fix command: {line}"
        );
    }

    #[test]
    fn test_hook_status_line_no_manifest_is_advisory_not_drift() {
        let facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::NoManifest);
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "abc1234");

        assert!(
            !drift,
            "NoManifest must NOT report drift (pre-manifest install is backward-compatible): {line}"
        );
        // Should still advise the user.
        assert!(
            line.contains("skim init"),
            "advisory should mention skim init to add detection: {line}"
        );
    }

    /// The whole point of routing `NoManifest` through the pin/currency checks
    /// (rather than returning early) is that pin drift is still reported when the
    /// manifest is absent.  This pins that composition: drift must be `true`, the
    /// pin verdict must be present, and the advisory must be appended — not
    /// substituted for it.
    #[test]
    fn test_hook_status_line_no_manifest_still_reports_pin_drift() {
        let mut facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::NoManifest);
        facts.hook_uses_pinned_binary = false;
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "abc1234");

        assert!(
            drift,
            "an unpinned hook is drift whether or not a manifest exists: {line}"
        );
        assert!(
            line.contains("unpinned"),
            "the pin verdict must survive the advisory composition: {line}"
        );
        assert!(
            line.contains("no integrity manifest"),
            "the advisory must be appended to the pin verdict: {line}"
        );
    }

    /// Same composition on the currency branch: a stale hook with no manifest
    /// must report the version/commit mismatch AND the advisory.
    #[test]
    fn test_hook_status_line_no_manifest_still_reports_currency_drift() {
        let mut facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::NoManifest);
        facts.hook_is_current = false;
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "zzz9999");

        assert!(drift, "a stale hook is drift: {line}");
        assert!(
            line.contains("commit mismatch"),
            "the currency verdict must survive the advisory composition: {line}"
        );
        assert!(
            line.contains("no integrity manifest"),
            "the advisory must be appended to the currency verdict: {line}"
        );
    }

    #[test]
    fn test_hook_status_line_verified_current_is_healthy() {
        let facts = make_installed_facts(crate::cmd::integrity::ScriptIntegrity::Verified);
        let (drift, line) = hook_status_line(&facts, "claude-code", "2.11.0", "abc1234");

        assert!(!drift, "Verified+current must not report drift");
        assert!(
            line.contains('✓'),
            "healthy state must show checkmark: {line}"
        );
    }

    #[test]
    fn test_hook_status_line_not_installed() {
        let facts = crate::cmd::init::HookFacts {
            hook_installed: false,
            hook_version: None,
            hook_commit: None,
            hook_binary_pin: None,
            hook_uses_pinned_binary: false,
            hook_is_current: false,
            pin_is_current: false,
            hook_script_path: std::path::PathBuf::from("/some/path/skim-rewrite.sh"),
            script_integrity: crate::cmd::integrity::ScriptIntegrity::NoManifest,
        };
        let (drift, line) = hook_status_line(&facts, "gemini", "2.11.0", "abc1234");

        assert!(!drift, "not-installed must not be drift");
        assert!(
            line.contains("not installed"),
            "not-installed must say so: {line}"
        );
    }

    // ---- query_binary_info acquisition-path test ----
    //
    // This is the regression test whose absence caused Defect 1.
    // It drives the acquisition path against a stub binary that:
    //   - emits  "skim 2.11.0 (d5695c1)\n"  on  --version
    //   - exits non-zero on  --commit  (simulating a pre-B4 binary)
    //
    // Before the fix, query_binary_commit called --commit, got non-zero,
    // and returned None, yielding "commit ?" in the output. The fix extracts
    // the commit from --version and never falls through to --commit.

    #[cfg(unix)]
    #[test]
    fn test_query_binary_info_commit_from_version_no_commit_flag() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let stub = dir.path().join("skim-stub");

        // Write a shell stub: emits version+commit on --version, fails on --commit.
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then\n\
               echo 'skim 2.11.0 (d5695c1)'\n\
               exit 0\n\
             fi\n\
             # Simulate pre-B4 binary: unknown flags → non-zero exit\n\
             echo 'error: unexpected argument' >&2\n\
             exit 1\n",
        )
        .unwrap();

        let meta = std::fs::metadata(&stub).unwrap();
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();

        let info = query_binary_info(&stub);

        assert_eq!(
            info.version.as_deref(),
            Some("2.11.0"),
            "version must be resolved from --version output"
        );
        assert_eq!(
            info.commit.as_deref(),
            Some("d5695c1"),
            "commit must be resolved from --version parenthetical even when --commit fails (pre-B4 binary)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_query_binary_info_no_parenthetical_falls_back_to_commit_flag() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let stub = dir.path().join("skim-stub2");

        // Stub: --version has no parenthetical; --commit returns a sha.
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then\n\
               echo 'skim 2.11.0'\n\
               exit 0\n\
             fi\n\
             if [ \"$1\" = '--commit' ]; then\n\
               echo 'abc1234'\n\
               exit 0\n\
             fi\n\
             exit 1\n",
        )
        .unwrap();

        let meta = std::fs::metadata(&stub).unwrap();
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();

        let info = query_binary_info(&stub);

        assert_eq!(info.version.as_deref(), Some("2.11.0"));
        assert_eq!(
            info.commit.as_deref(),
            Some("abc1234"),
            "must fall back to --commit when --version has no parenthetical"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_query_binary_info_non_skim_binary_returns_none() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let stub = dir.path().join("not-skim");

        // A realistic non-skim binary: --version output lacks "skim " prefix,
        // and unknown flags (like --commit) are rejected with non-zero exit.
        // Both conditions must produce None for version and commit.
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then\n\
               echo 'some-other-tool 1.2.3 (xyz)'\n\
               exit 0\n\
             fi\n\
             # Unknown flag → non-zero exit (realistic non-skim behavior)\n\
             exit 1\n",
        )
        .unwrap();

        let meta = std::fs::metadata(&stub).unwrap();
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();

        let info = query_binary_info(&stub);

        assert!(
            info.version.is_none(),
            "non-skim binary must yield version: None (output doesn't start with 'skim ')"
        );
        assert!(
            info.commit.is_none(),
            "non-skim binary must yield commit: None (--version has no 'skim ' prefix, --commit rejected)"
        );
    }
}
