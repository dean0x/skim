//! Git status compression — porcelain v2 parsing.

use std::process::ExitCode;

use crate::cmd::extract_output_format;
use crate::output::canonical::GitResult;
use crate::runner::CommandRunner;

/// Short option characters that, when present in a single-dash cluster, change
/// `git status` output format and conflict with the `--porcelain=v2` the handler
/// injects:
///
/// - `s` — selects short format (overrides porcelain)
/// - `z` — NUL-terminates records (breaks `output.lines()` line-based parse)
const CONFLICTING_SHORT_OPTS: &[char] = &['s', 'z'];

/// Returns `true` for flags that conflict with the `--porcelain=v2` flag the
/// handler injects.
///
/// Conflicting flags are those that change the output format to something other
/// than porcelain v2:
/// - `--short` / `--porcelain` / `--porcelain=*` / `--null` / `--long` (long forms)
/// - Short clusters containing any char in [`CONFLICTING_SHORT_OPTS`] (`s` or `z`)
///
/// `-b` alone is NOT conflicting — it enables branch display, same as the
/// `--branch` flag the handler already injects.
///
/// Callers must exclude pathspecs (tokens after `--`) before invoking this
/// predicate; see [`strip_conflicting_short_chars`] and [`run_status`].
fn is_conflicting_status_flag(s: &str) -> bool {
    // Long-form format flags
    if matches!(s, "--short" | "--porcelain" | "--null" | "--long") || s.starts_with("--porcelain=")
    {
        return true;
    }
    // Short flag clusters: single-dash only; check against the closed set {s, z}.
    if s.starts_with('-') && !s.starts_with("--") {
        return s[1..].chars().any(|c| CONFLICTING_SHORT_OPTS.contains(&c));
    }
    false
}

/// Transform a single arg token by removing conflicting short option chars.
///
/// - Long-form conflicting flags (`--short`, `--null`, `--long`, etc.) → `None`
///   (drop entirely).
/// - Short clusters: strip chars in [`CONFLICTING_SHORT_OPTS`], re-emit remainder.
///   - `-suno` → `Some("-uno")` (strip `s`, keep `uno`)
///   - `-sb`   → `Some("-b")`  (strip `s`, keep `b`)
///   - `-z`    → `None`        (nothing remains — drop entirely)
/// - Non-conflicting tokens are returned unchanged (`Some(token)`).
fn strip_conflicting_short_chars(token: &str) -> Option<String> {
    if !is_conflicting_status_flag(token) {
        return Some(token.to_string());
    }
    // Long flags: always drop entirely.
    if token.starts_with("--") {
        return None;
    }
    // Short cluster: strip only the conflicting chars, preserve the rest.
    let remainder: String = token[1..]
        .chars()
        .filter(|c| !CONFLICTING_SHORT_OPTS.contains(c))
        .collect();
    if remainder.is_empty() {
        None
    } else {
        Some(format!("-{remainder}"))
    }
}

/// Run `git status` with compression.
///
/// Strips user-supplied format flags (see [`is_conflicting_status_flag`]) before
/// forwarding to git so they cannot conflict with the `--porcelain=v2` flag that
/// the handler injects for structured parsing. Short-flag clusters have only their
/// conflicting chars stripped — non-conflicting chars in the same cluster are
/// preserved (e.g., `-suno` → `-uno`). Pathspecs after `--` are never modified.
///
/// # C-7 / net-savings guard baseline
///
/// When the user typed a format-substituting flag like `--short` or `-s`, skim
/// replaces it with `--porcelain=v2 --branch` internally.  Comparing the
/// compressed output against the porcelain baseline would mask the expansion vs
/// what the **user** would have seen.  To satisfy the "never expand" promise
/// (#317) relative to the user's intent, we capture the user's literal command
/// output and pass it as the `raw_override` to `run_parsed_command` when any
/// substitution occurred.
///
/// On a large dirty repo the porcelain compression still wins — the guard is a
/// genuine comparison, not a skip.  On a small repo the guard correctly falls
/// through to raw `--short` output (~56 B) rather than emitting the expanded
/// porcelain summary (~138 B).
pub(super) fn run_status(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // Transform conflicting format flags:
    // - Before `--`: strip only the conflicting chars; drop token if nothing remains.
    // - At `--` and after: pass through verbatim (pathspecs, not flags).
    let mut past_terminator = false;
    let stripped_args: Vec<String> = args
        .iter()
        .filter_map(|a| {
            if past_terminator {
                return Some(a.clone());
            }
            if a.as_str() == "--" {
                past_terminator = true;
                return Some(a.clone());
            }
            strip_conflicting_short_chars(a.as_str())
        })
        .collect();

    let (filtered_args, output_format) = extract_output_format(&stripped_args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
    ]);
    full_args.extend_from_slice(&filtered_args);

    let label = super::build_analytics_label("status", args, show_stats, rec.enabled);

    // A1 (C-7 extended): Always capture the user's literal `git status <args>`
    // output as the guard baseline.  Previously this was only captured when
    // `has_conflicting` — but even when the user's flags are compatible, the
    // injected `--porcelain=v2 --branch` form can be considerably larger than
    // the default `git status` output (it outputs machine-readable header lines
    // for every branch tracking field).  Without pre-capturing, the guard
    // compares the skim summary against the injected form's bytes — a baseline
    // the user never asked for — and can wrongly compress output that is
    // actually larger than the user's literal command would have been.
    //
    // Best-effort: if the user's command fails (e.g., not in a git repo),
    // fall through to the normal porcelain baseline (`None`).
    let runner = CommandRunner::new();
    let mut user_args: Vec<String> = global_flags.to_vec();
    user_args.push("status".to_string());
    user_args.extend_from_slice(args);
    let arg_refs: Vec<&str> = user_args.iter().map(String::as_str).collect();
    let user_raw_override: Option<String> = runner
        .run("git", &arg_refs)
        .ok()
        .filter(|o| o.exit_code == Some(0))
        .map(|o| crate::output::strip_ansi(&o.stdout));

    // C-7: pass the user's literal command output as the raw override so
    // the net-savings guard compares against the right baseline.  The
    // credential-scrub path in run_parsed_command (PF-024) covers this call.
    super::run_parsed_command(
        &full_args,
        show_stats,
        rec,
        output_format,
        label,
        super::ParsedCommandOptions {
            combine_stderr: false,
            raw_override: user_raw_override,
            // ADR-015 / D1 declaration — `Lossy`.  The handler injects
            // `--porcelain=v2` and `parse_status` folds those records into
            // counted groups, so the envelope is a summary of what the user's
            // literal `git status` would have printed.  No 1:1 unit to count
            // against the raw output, so `elided` = None.
            completeness: crate::output::fidelity::Completeness::Lossy,
        },
        parse_status,
    )
}

/// State of the `# branch.ab` line from porcelain v2 output.
///
/// Tracking absence separately from zero counts is essential: when a remote
/// tracking branch exists but the remote ref is gone, git omits the
/// `# branch.ab` line entirely.  Treating that as `(0, 0)` would render the
/// branch as "in sync", masking the deletion.
///
/// Parsed at [`StatusCategories::classify_line`] time (parse-at-boundaries
/// rule) — not deferred to render time.
#[derive(Default)]
enum AheadBehind {
    /// No `# branch.ab` line was seen.  When an upstream is configured this
    /// indicates the remote ref is gone; the render path shows `[gone]` to
    /// mirror native `git status -sb`.
    #[default]
    Absent,
    /// Line was seen and both counts parsed successfully.
    Counts { ahead: u64, behind: u64 },
    /// Line was seen but could not be parsed — the raw payload is surfaced
    /// verbatim rather than silently claiming in-sync (PF-008 / fail-loud).
    Malformed(String),
}

/// Accumulated per-category file lists from a porcelain v2 status parse.
#[derive(Default)]
struct StatusCategories {
    branch: String,
    /// Upstream tracking branch from `# branch.upstream <name>`.
    upstream: String,
    /// Ahead/behind counts parsed from `# branch.ab +<ahead> -<behind>`.
    ///
    /// [`AheadBehind::Absent`] when no `# branch.ab` line was emitted by git
    /// (the remote ref is gone).  Parsed eagerly in [`classify_line`] so the
    /// render path never needs to re-parse a raw string.
    ahead_behind: AheadBehind,
    staged: Vec<String>,
    modified: Vec<String>,
    untracked: Vec<String>,
    renamed: Vec<String>,
    unmerged: Vec<String>,
}

impl StatusCategories {
    /// Classify and accumulate a single porcelain v2 output line.
    fn classify_line(&mut self, line: &str) {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            self.branch = head.to_string();
            return;
        }

        if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
            self.upstream = upstream.to_string();
            return;
        }

        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            self.ahead_behind = parse_ahead_behind(ab);
            return;
        }

        if line.starts_with('#') {
            return;
        }

        if line.starts_with('?') {
            // Untracked: "? <path>"
            self.untracked
                .push(line.get(2..).unwrap_or_default().to_string());
            return;
        }

        if line.starts_with('u') {
            // Unmerged: "u <xy> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
            self.unmerged.push(extract_last_path(line));
            return;
        }

        if line.starts_with('2') {
            // Renamed/copied: "2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X_score> <path>\t<origPath>"
            self.renamed.push(extract_renamed_path(line));
            return;
        }

        if line.starts_with('1') {
            // Tracked changed: "1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            // XY: index change (X) and worktree change (Y)
            let xy = extract_xy(line);
            let path = extract_last_path(line);

            let x = xy.chars().next().unwrap_or('.');
            let y = xy.chars().nth(1).unwrap_or('.');

            if x != '.' {
                self.staged.push(format!("{}{}", stage_prefix(x), path));
            }
            if y != '.' {
                self.modified
                    .push(format!("{}{}", worktree_prefix(y), path));
            }
        }
    }

    /// Build a compressed GitResult from the accumulated categories.
    ///
    /// Shows ALL files — no cap per GRANITE #618 lesson.
    fn build_result(self) -> GitResult {
        let mut details: Vec<String> = Vec::new();

        if !self.branch.is_empty() {
            let branch_line = if self.upstream.is_empty() {
                // No upstream configured: just show the head name.
                format!("branch: {}", self.branch)
            } else {
                // Upstream present: mirror native `git status -sb` header format
                // "## main...origin/main [ahead 1]" exactly.
                //
                // The three-state model:
                //   Absent         → upstream ref is gone  → "[gone]"
                //   Counts(0, 0)   → in sync with upstream → no bracket
                //   Counts(a, b)   → diverged              → "[ahead A]" / "[behind B]" / both
                //   Malformed(raw) → parse failed          → "[{raw}]" (fail loud, PF-008)
                let suffix = match self.ahead_behind {
                    AheadBehind::Absent => " [gone]".to_string(),
                    AheadBehind::Counts { ahead, behind } => match (ahead, behind) {
                        (0, 0) => String::new(),
                        (a, 0) => format!(" [ahead {a}]"),
                        (0, b) => format!(" [behind {b}]"),
                        (a, b) => format!(" [ahead {a}, behind {b}]"),
                    },
                    AheadBehind::Malformed(raw) => format!(" [{raw}]"),
                };
                format!("branch: {}...{}{}", self.branch, self.upstream, suffix)
            };
            details.push(branch_line);
        }
        for f in &self.staged {
            details.push(format!("staged: {f}"));
        }
        for f in &self.modified {
            details.push(format!("modified: {f}"));
        }
        for f in &self.untracked {
            details.push(format!("untracked: {f}"));
        }
        for f in &self.renamed {
            details.push(format!("renamed: {f}"));
        }
        for f in &self.unmerged {
            details.push(format!("unmerged: {f}"));
        }

        let total_changes = self.staged.len()
            + self.modified.len()
            + self.untracked.len()
            + self.renamed.len()
            + self.unmerged.len();

        let summary = if total_changes == 0 {
            "clean".to_string()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !self.staged.is_empty() {
                parts.push(format!("{} staged", self.staged.len()));
            }
            if !self.modified.is_empty() {
                parts.push(format!("{} modified", self.modified.len()));
            }
            if !self.untracked.is_empty() {
                parts.push(format!("{} untracked", self.untracked.len()));
            }
            if !self.renamed.is_empty() {
                parts.push(format!("{} renamed", self.renamed.len()));
            }
            if !self.unmerged.is_empty() {
                parts.push(format!("{} unmerged", self.unmerged.len()));
            }
            parts.join(", ")
        };

        GitResult::new("status".to_string(), summary, details).with_tier("full")
    }
}

/// Parse porcelain v2 status output into a compressed GitResult.
fn parse_status(output: &str) -> GitResult {
    let mut cats = StatusCategories::default();
    for line in output.lines() {
        cats.classify_line(line);
    }
    cats.build_result()
}

/// Extract XY field from porcelain v2 tracked entry line.
/// Format: "1 <XY> <rest...>"
fn extract_xy(line: &str) -> String {
    line.split_whitespace().nth(1).unwrap_or("..").to_string()
}

/// Extract the path from a porcelain v2 line using fixed field counts.
///
/// Type 1 entries: "1 XY sub mH mI mW hH hI <path>" (8 fields before path)
/// Unmerged entries: "u XY sub m1 m2 m3 mW h1 h2 h3 <path>" (10 fields before path)
///
/// Uses `splitn` with the correct field count so paths with spaces are preserved.
fn extract_last_path(line: &str) -> String {
    let field_count = if line.starts_with('u') {
        // Unmerged: 10 fixed fields + path
        11
    } else {
        // Type 1: 8 fixed fields + path
        9
    };

    let fields: Vec<&str> = line.splitn(field_count, ' ').collect();
    fields.last().unwrap_or(&"").to_string()
}

/// Extract the renamed path from a porcelain v2 type 2 entry.
/// Format: "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
fn extract_renamed_path(line: &str) -> String {
    // Porcelain v2 type-2 entries always contain a tab separator:
    // "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
    let Some(tab_pos) = line.find('\t') else {
        // Malformed input: no tab found; fall back to the path portion after the prefix
        return line.get(2..).unwrap_or_default().to_string();
    };
    let before_tab = &line[..tab_pos];
    let after_tab = &line[tab_pos + 1..];
    // Field 10 (0-indexed 9) is the new path; use splitn to preserve spaces in path
    let new_path = before_tab.splitn(10, ' ').last().unwrap_or_default();
    format!("{after_tab} -> {new_path}")
}

/// Map index status character to a display prefix.
fn stage_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'A' => "A ",
        'D' => "D ",
        'R' => "R ",
        'C' => "C ",
        _ => "",
    }
}

/// Map worktree status character to a display prefix.
fn worktree_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'D' => "D ",
        _ => "",
    }
}

/// Parse a `# branch.ab` value (format: `+<ahead> -<behind>`) into an
/// [`AheadBehind`] state.
///
/// Returns [`AheadBehind::Malformed`] with the raw payload when either field
/// cannot be parsed, surfacing it verbatim rather than silently claiming
/// in-sync (fail-loud principle, PF-008 class).
fn parse_ahead_behind(ab: &str) -> AheadBehind {
    let mut parts = ab.split_whitespace();
    let ahead_str = parts.next().unwrap_or("");
    let behind_str = parts.next().unwrap_or("");
    match (
        ahead_str.trim_start_matches('+').parse::<u64>(),
        behind_str.trim_start_matches('-').parse::<u64>(),
    ) {
        (Ok(ahead), Ok(behind)) => AheadBehind::Counts { ahead, behind },
        _ => AheadBehind::Malformed(ab.to_string()),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::sync::LazyLock;

    /// Matches diff stat lines: " file | 42 +++++---"
    static STAT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+(\d+)\s+([+-]+)").unwrap());

    /// Matches diff stat summary lines: "3 files changed, ..."
    static SUMMARY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+)\s+files?\s+changed").unwrap());

    /// Matches binary diff stat lines: " file.bin | Bin 0 -> 1234 bytes"
    static BINARY_STAT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+Bin\s+").unwrap());

    /// Parse `git diff --stat` output into a compressed GitResult.
    ///
    /// Retained for testing and potential future use (e.g., `--mode stat`).
    fn parse_diff_stat(output: &str) -> GitResult {
        let mut file_stats: Vec<String> = Vec::new();
        let mut summary_line = String::new();

        for line in output.lines() {
            if let Some(caps) = STAT_RE.captures(line) {
                let file = caps.get(1).map_or("", |m| m.as_str()).trim();
                let count = caps.get(2).map_or("", |m| m.as_str());
                let changes = caps.get(3).map_or("", |m| m.as_str());
                file_stats.push(format!("{file} | {count} {changes}"));
                continue;
            }

            // Binary files appear as "file.bin | Bin 0 -> 1234 bytes"
            if let Some(caps) = BINARY_STAT_RE.captures(line) {
                let file = caps.get(1).map_or("", |m| m.as_str()).trim();
                file_stats.push(format!("{file} | Bin"));
                continue;
            }

            if SUMMARY_RE.is_match(line) {
                summary_line = line.trim().to_string();
            }
        }

        if summary_line.is_empty() && file_stats.is_empty() {
            return GitResult::new("diff".to_string(), "no changes".to_string(), vec![])
                .with_tier("full");
        }

        if summary_line.is_empty() {
            summary_line = format!("{} files changed", file_stats.len());
        }

        GitResult::new("diff".to_string(), summary_line, file_stats).with_tier("full")
    }

    // ========================================================================
    // parse_status tests
    // ========================================================================

    #[test]
    fn test_parse_status_clean() {
        let output = "# branch.oid abc123\n# branch.head main\n";
        let result = parse_status(output);
        assert_eq!(result.summary, "clean");
        // Details should contain branch info only (no upstream on this fixture)
        assert!(result.details.iter().any(|d| d.contains("branch: main")));
    }

    /// Fix 1: `# branch.upstream` and `# branch.ab` lines must appear in the
    /// branch detail, not be silently discarded.  This catches the root cause of
    /// the `git status -sb` branch-header info-loss on clean trees.
    ///
    /// Full-string assert pins the exact format declared by the "mirror native
    /// `git status -sb` header format exactly" comment (testing-04).
    #[test]
    fn test_parse_status_upstream_and_ahead_behind() {
        let output = include_str!("../../../tests/fixtures/cmd/git/status_clean_ahead.txt");
        let result = parse_status(output);
        assert_eq!(result.summary, "clean", "clean tree with upstream");
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        // Full-string assert — field-order swaps or separator changes must fail.
        assert_eq!(
            branch_detail, "branch: feature/my-branch...origin/feature/my-branch [ahead 3]",
            "branch detail must mirror native git status -sb format exactly"
        );
    }

    /// Branch-detail render (in-sync case): when both ahead and behind are 0,
    /// the bracket is omitted entirely — matching native `git status -sb`
    /// `## main...origin/main` with no `[...]` suffix.
    ///
    /// Full-string assert pins the exact format (testing-04).
    #[test]
    fn test_parse_status_upstream_both_zero_no_bracket() {
        let output = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let result = parse_status(output);
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        // Both counts zero → no bracket; exact format match.
        assert_eq!(
            branch_detail, "branch: main...origin/main",
            "in-sync case must omit bracket entirely"
        );
    }

    /// Branch-detail render (diverged case): when ahead > 0 AND behind > 0,
    /// both components must appear in the bracket — `[ahead A, behind B]`.
    ///
    /// Full-string assert pins the exact format (testing-04).
    #[test]
    fn test_parse_status_upstream_both_nonzero_bracket() {
        let output = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -3\n";
        let result = parse_status(output);
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        assert_eq!(
            branch_detail, "branch: main...origin/main [ahead 2, behind 3]",
            "diverged case must show both components"
        );
    }

    /// Branch-detail render (behind-only case): when only behind > 0, only the
    /// `[behind B]` bracket must appear — no `ahead` component.
    ///
    /// Full-string assert pins the exact format (testing-04).
    #[test]
    fn test_parse_status_upstream_behind_only_bracket() {
        let output = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -5\n";
        let result = parse_status(output);
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        assert_eq!(
            branch_detail, "branch: main...origin/main [behind 5]",
            "behind-only case must show only [behind N]"
        );
    }

    /// Branch-detail render (gone-upstream case, architecture-02): when an
    /// upstream is configured but git emits no `# branch.ab` line — which
    /// happens when the remote ref has been deleted — the render must show
    /// `[gone]`, exactly matching native `git status -sb` output.
    ///
    /// With the old `ahead_behind: String` model, absent ab defaulted to `""`
    /// which `build_ahead_behind_bracket` treated as `(0, 0)` → no bracket,
    /// falsely implying "in sync with upstream".
    #[test]
    fn test_parse_status_gone_upstream() {
        // Upstream present, no # branch.ab line (upstream ref deleted on remote).
        let output = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n";
        let result = parse_status(output);
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        assert_eq!(
            branch_detail, "branch: main...origin/main [gone]",
            "gone-upstream case must render [gone] to match native git status -sb"
        );
    }

    /// Branch-detail render (malformed-ab case, security-07 / reliability-02):
    /// when `# branch.ab` is present but has an unexpected shape, the raw
    /// payload must be surfaced in brackets rather than silently claiming
    /// in-sync.  Silent zero default violates PF-008 fail-loud.
    #[test]
    fn test_parse_status_malformed_ab_surfaces_raw_payload() {
        // A hypothetical future porcelain format change or corrupted output.
        let output = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab unexpected-format\n";
        let result = parse_status(output);
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        // The raw payload must be surfaced — NOT silently rendered as in-sync.
        assert_eq!(
            branch_detail, "branch: main...origin/main [unexpected-format]",
            "malformed ab must surface raw payload, never silently claim in-sync"
        );
        // Explicit negative: no silent in-sync render (the PF-008 failure mode).
        assert!(
            !branch_detail.ends_with("origin/main"),
            "malformed ab must NOT render as bare in-sync (no bracket)"
        );
    }

    /// Fix 1: `git status -sb` — the `-sb` cluster must be detected as
    /// conflicting (contains `s` = short format) so it is stripped before
    /// forwarding to git, preventing the short-format `##` branch line from
    /// overriding the porcelain v2 output and causing info-loss.
    #[test]
    fn test_short_cluster_with_s_is_conflicting() {
        for flag in &["-s", "-sb", "-bs", "-sbu", "-us"] {
            assert!(
                is_conflicting_status_flag(flag),
                "{flag:?} contains 's' (short format) — must be conflicting"
            );
        }
        // `-b` alone: NOT conflicting — branch flag, same as our --branch injection.
        assert!(
            !is_conflicting_status_flag("-b"),
            "-b alone must NOT be conflicting"
        );
        // `-u` alone: NOT conflicting — untracked-files flag.
        assert!(
            !is_conflicting_status_flag("-u"),
            "-u alone must NOT be conflicting"
        );
    }

    #[test]
    fn test_parse_status_dirty() {
        let output = include_str!("../../../tests/fixtures/cmd/git/status_dirty.txt");
        let result = parse_status(output);

        // Verify summary contains counts
        assert!(
            result.summary.contains("staged"),
            "expected 'staged' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("modified"),
            "expected 'modified' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("untracked"),
            "expected 'untracked' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("renamed"),
            "expected 'renamed' in summary, got: {}",
            result.summary
        );

        // Fix 1: branch detail must mirror native git -sb format exactly.
        // Full-string assert — field-order swaps or separator changes must fail (testing-04).
        let branch_detail = result
            .details
            .iter()
            .find(|d| d.starts_with("branch:"))
            .expect("must have a branch detail line");
        assert_eq!(
            branch_detail, "branch: main...origin/main [ahead 1]",
            "branch detail must mirror native git status -sb format exactly"
        );
    }

    #[test]
    fn test_parse_status_shows_all_files() {
        // Generate 25 untracked files — ensure no cap
        let mut output = String::from("# branch.head main\n");
        for i in 0..25 {
            output.push_str(&format!("? file_{i}.txt\n"));
        }

        let result = parse_status(&output);

        // All 25 should appear in details (no 5/5/3 cap like GRANITE #618)
        let untracked_count = result
            .details
            .iter()
            .filter(|d| d.starts_with("untracked:"))
            .count();
        assert_eq!(
            untracked_count, 25,
            "expected all 25 untracked files, got {untracked_count}"
        );
    }

    // ========================================================================
    // parse_diff_stat tests
    // ========================================================================

    #[test]
    fn test_parse_diff_stat() {
        let output = include_str!("../../../tests/fixtures/cmd/git/diff_stat.txt");
        let result = parse_diff_stat(output);

        assert!(
            result.summary.contains("3 files changed"),
            "expected '3 files changed' in summary, got: {}",
            result.summary
        );
        assert_eq!(result.details.len(), 3, "expected 3 file stat entries");
    }

    #[test]
    fn test_parse_diff_stat_empty() {
        let result = parse_diff_stat("");
        assert_eq!(result.summary, "no changes");
        assert!(result.details.is_empty());
    }

    // ========================================================================
    // Paths with spaces in git status
    // ========================================================================

    #[test]
    fn test_extract_last_path_with_spaces() {
        // Type 1 entry with space in path: 8 fixed fields + path
        let line = "1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs";
        assert_eq!(extract_last_path(line), "src/my file.rs");
    }

    #[test]
    fn test_parse_status_path_with_spaces() {
        let output = "# branch.head main\n\
                      1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs\n";
        let result = parse_status(output);
        assert!(
            result.details.iter().any(|d| d.contains("my file.rs")),
            "expected path with spaces in details, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // Unmerged entries in status
    // ========================================================================

    #[test]
    fn test_parse_status_unmerged_entries() {
        let output = "# branch.head main\n\
                      u UU N... 100644 100644 100644 100644 abc1234 def5678 ghi9012 src/conflict.rs\n";
        let result = parse_status(output);
        assert!(
            result.summary.contains("unmerged"),
            "expected 'unmerged' in summary, got: {}",
            result.summary
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("unmerged:") && d.contains("conflict.rs")),
            "expected unmerged detail for conflict.rs, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // Fallback guarantee: parse_status must not panic on unexpected input
    // (Step 7e)
    // ========================================================================

    #[test]
    fn test_parse_status_garbage_input_no_panic() {
        // Feed unexpected garbage — must produce a valid result, never panic.
        // The parser may interpret "unexpected garbage input" as an unmerged line
        // (because the line starts with 'u'). The key contract is: no panic, valid result.
        let result = parse_status("unexpected garbage input");
        // Must return a well-formed GitResult (non-empty summary, valid details vec).
        assert!(!result.summary.is_empty(), "Summary must not be empty");
        // Details may or may not be populated depending on how the line is classified.
        // We just verify the result is structurally valid (no panic, fields accessible).
        let _ = result.details.len();
    }

    #[test]
    fn test_parse_status_null_bytes_no_panic() {
        // Feed null bytes and other control characters — must not panic.
        let result = parse_status("\x00\x01\x02\x03 binary garbage");
        assert!(!result.summary.is_empty(), "summary must be non-empty");
    }

    #[test]
    fn test_parse_status_partial_v2_line_no_panic() {
        // Truncated porcelain v2 lines — handler must degrade gracefully.
        let output = "# branch.head main\n1\n1 M\n1 M. \n";
        let result = parse_status(output);
        assert!(
            result.details.iter().any(|d| d.contains("branch: main")),
            "branch line should still be parsed"
        );
        // Truncated type-1 lines should produce no panics, possibly no detail entries.
    }

    // ========================================================================
    // Flag-stripping predicate
    // ========================================================================

    /// Test helper that mirrors `run_status`'s flag-stripping logic: partial-strip
    /// of conflicting short-cluster chars with `--` terminator support.
    fn strip_conflicting_flags(args: &[&str]) -> Vec<String> {
        let mut past_terminator = false;
        args.iter()
            .filter_map(|&a| {
                if past_terminator {
                    return Some(a.to_string());
                }
                if a == "--" {
                    past_terminator = true;
                    return Some(a.to_string());
                }
                strip_conflicting_short_chars(a)
            })
            .collect()
    }

    #[test]
    fn test_flag_stripping_removes_conflicting_flags() {
        // Tokens whose only content is conflicting chars are dropped entirely.
        assert!(
            strip_conflicting_flags(&["-s"]).is_empty(),
            "-s alone must be dropped entirely"
        );
        assert!(
            strip_conflicting_flags(&["--short"]).is_empty(),
            "--short must be dropped entirely"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain"]).is_empty(),
            "--porcelain must be dropped entirely"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain=v1"]).is_empty(),
            "--porcelain=v1 must be dropped entirely"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain=v2"]).is_empty(),
            "--porcelain=v2 must be dropped entirely"
        );
        // Short clusters: only the conflicting char(s) are stripped; non-conflicting
        // chars in the same cluster are preserved.
        // Fix 1: `-sb` and `-bs` contain `s` (conflicting) but also `b` (branch flag,
        // same semantic as our injected --branch) — only `s` is stripped.
        assert_eq!(
            strip_conflicting_flags(&["-sb"]),
            vec!["-b"],
            "-sb must strip only 's' and preserve '-b'"
        );
        assert_eq!(
            strip_conflicting_flags(&["-bs"]),
            vec!["-b"],
            "-bs must strip only 's' and preserve '-b'"
        );
    }

    #[test]
    fn test_flag_stripping_preserves_non_conflicting_flags() {
        let input = ["--branch", "--", "path/to/file"];
        let result = strip_conflicting_flags(&input);
        assert_eq!(
            result,
            vec!["--branch", "--", "path/to/file"],
            "non-conflicting flags must be preserved"
        );
    }

    #[test]
    fn test_flag_stripping_mixed_input() {
        // Conflicting flags before `--` are stripped; args after `--` are pathspecs
        // and must be preserved verbatim even when they look like format flags.
        let input = [
            "-s",
            "--branch",
            "--short",
            "--",
            "--porcelain=v1", // after `--` → pathspec, NOT a flag; must be preserved
            "path/to/file",
        ];
        let result = strip_conflicting_flags(&input);
        assert_eq!(
            result,
            vec!["--branch", "--", "--porcelain=v1", "path/to/file"],
            "args after `--` must pass through verbatim as pathspecs, never stripped"
        );
    }

    // ========================================================================
    // C-7 / net-savings guard baseline — flag detection
    // ========================================================================

    /// `is_conflicting_status_flag` must return true for all flags that trigger
    /// skim's substitution of `--porcelain=v2 --branch`, so that the guard uses
    /// the user's literal command output as the "raw" baseline (C-7 requirement).
    #[test]
    fn test_conflicting_flag_detection_for_c7_baseline() {
        // All substitution-triggering flags must be detected.
        for flag in &[
            "-s",
            "-sb", // short cluster with 's'
            "-bs", // short cluster with 's'
            "-z",  // NUL termination — new
            "--short",
            "--porcelain",
            "--porcelain=v1",
            "--porcelain=v2",
            "--null", // alias for -z — new
            "--long", // human long format — new
        ] {
            assert!(
                is_conflicting_status_flag(flag),
                "{flag:?} must trigger C-7 raw-override path"
            );
        }
        // Non-conflicting flags must NOT trigger the override.
        for flag in &["--branch", "--untracked-files", "-u", "-b", "--", "HEAD"] {
            assert!(
                !is_conflicting_status_flag(flag),
                "{flag:?} must NOT trigger C-7 raw-override path"
            );
        }
    }

    // ========================================================================
    // Binary files in diff stat
    // ========================================================================

    #[test]
    fn test_parse_diff_stat_binary_files() {
        let output = " src/main.rs   | 15 +++++++++------\n\
                       image.png     | Bin 0 -> 1234 bytes\n\
                       2 files changed, 10 insertions(+), 5 deletions(-)\n";
        let result = parse_diff_stat(output);
        assert_eq!(
            result.details.len(),
            2,
            "expected 2 file stat entries (1 text + 1 binary), got: {:?}",
            result.details
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("image.png") && d.contains("Bin")),
            "expected binary file entry, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // New flag-detection and cluster-rewriting tests
    // (security-04..06 / architecture-09 / reliability-08 / regression-05)
    // ========================================================================

    /// `-z` alone: NUL-terminates records, corrupts `output.lines()` parse.
    /// Must be detected as conflicting and dropped entirely when stripped.
    #[test]
    fn test_z_flag_detected_and_stripped() {
        assert!(
            is_conflicting_status_flag("-z"),
            "-z must be detected as conflicting (NUL termination)"
        );
        assert!(
            strip_conflicting_flags(&["-z"]).is_empty(),
            "-z alone must be dropped entirely when stripped"
        );
    }

    /// `-sb`: `s` is the conflicting char; `-b` (branch) is semantically valid
    /// and must survive stripping so git still sees the branch flag.
    #[test]
    fn test_sb_cluster_detected_b_preserved() {
        assert!(
            is_conflicting_status_flag("-sb"),
            "-sb must be detected as conflicting (contains 's')"
        );
        assert_eq!(
            strip_conflicting_flags(&["-sb"]),
            vec!["-b"],
            "-sb must strip 's' and preserve '-b'"
        );
    }

    /// `-suno`: 's' conflicts; 'u','n','o' carry untracked-files semantics that
    /// must reach git unmodified.
    #[test]
    fn test_suno_cluster_strips_only_s() {
        assert!(
            is_conflicting_status_flag("-suno"),
            "-suno must be detected as conflicting (contains 's')"
        );
        assert_eq!(
            strip_conflicting_flags(&["-suno"]),
            vec!["-uno"],
            "-suno must strip only 's' and preserve '-uno'"
        );
    }

    /// `--null` and `--long`: long-form flags that change output format — must be
    /// detected as conflicting and dropped entirely (never partially stripped).
    #[test]
    fn test_null_and_long_flags_detected() {
        assert!(
            is_conflicting_status_flag("--null"),
            "--null must be detected as conflicting"
        );
        assert!(
            is_conflicting_status_flag("--long"),
            "--long must be detected as conflicting"
        );
        assert!(
            strip_conflicting_flags(&["--null"]).is_empty(),
            "--null must be dropped entirely"
        );
        assert!(
            strip_conflicting_flags(&["--long"]).is_empty(),
            "--long must be dropped entirely"
        );
    }

    /// A pathspec literally named `-s` (or any token that looks like a flag)
    /// appearing after `--` must NOT be stripped — the `--` terminator ends the
    /// flag region.  Stripping it would silently change which paths git reports.
    #[test]
    fn test_pathspec_after_terminator_untouched() {
        let input = ["--", "-s", "path/to/file"];
        let result = strip_conflicting_flags(&input);
        assert_eq!(
            result,
            vec!["--", "-s", "path/to/file"],
            "tokens after '--' must be preserved verbatim as pathspecs"
        );
    }
}
