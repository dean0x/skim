//! `gh run watch` streaming output compression.
//!
//! Parses the live workflow run stream from `gh run watch`, emitting compressed
//! status lines as each job transitions through its lifecycle.
//!
//! # State machine
//!
//! The parser tracks per-job state in a HashMap (capped at [`MAX_STREAM_JOBS`]).
//! Each job entry records the job name and current status.  When a status
//! transition is detected (job start, completion, failure), a compressed line
//! is emitted.
//!
//! State machine rules:
//! 1. New job line (`In progress`, `Queued`, `Waiting`) → emit `⏳ {name}`.
//! 2. Job completion (`Completed`, `Success`) → emit `✓ {name}`.
//! 3. Job failure (`Failure`, `Failed`) → emit `✗ {name} [FAILED]`.
//! 4. Progress/noise lines (dots, percentages, unchanged status) → suppressed.
//! 5. Error lines → passed through.
//! 6. Job lines beyond [`MAX_STREAM_JOBS`] → suppressed AND disclosed at EOF
//!    with a loud elision marker (#317; ADR-011 class 1, unconditional).
//!
//! # Job identity (the map key)
//!
//! `gh run watch` re-prints the entire job list on every refresh, and each
//! entry carries a ticking ` in <duration>` and a trailing ` (ID <n>)`.  The
//! HashMap is keyed on a [`JobKey`]: the name with both frame-varying tails
//! removed, PLUS the ` (ID <n>)` discriminator kept as a separate field.
//!
//! Both halves are load-bearing, and each answers the opposite failure:
//!
//! - **dropping the ticking duration** is what makes one job ONE key across
//!   frames.  Keying on the raw entry minted a fresh key per frame, which
//!   re-emitted every job on every frame and drove the map into the cap.
//! - **retaining the ID** is what keeps two DIFFERENT jobs two keys.  A
//!   reusable-workflow call, or a matrix whose leg does not appear in the job
//!   name, prints several entries under one name; folding the ID into the name
//!   collapsed them onto a single key, so every one after the first was never
//!   emitted at all.  That trades a noise defect for a LOSS defect, which
//!   #317 does not permit at any severity.
//!
//! The rule the key encodes, in one line: the same job across frames
//! collapses, different jobs never do.
//!
//! # Non-retention design (AD-STR-4)
//!
//! No history buffer is maintained.  Only the current step state is tracked.
//! Parsers must be stateless across lines except for the jobs HashMap.
//!
//! # DESIGN NOTE (AD-GRW-1) — ANSI strip in reader, not parser
//!
//! `gh run watch` uses `\r` cursor rewrites for in-place status updates.
//! The streaming harness splits on `\n` and strips trailing `\r` after
//! splitting, so the parser never sees `\r`.  ANSI escape codes are stripped
//! by `strip_ansi` in the streaming reader (see `streaming.rs`) before lines
//! reach this parser.  The parser does NOT call `strip_ansi` itself.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use super::streaming::{
    StreamConfig, StreamTotals, StreamingParser, run_streamed_spawned, run_streamed_stdin,
};

// ============================================================================
// Constants
// ============================================================================

/// Maximum number of concurrent jobs tracked in the streaming state.
///
/// gh run watch may expand matrices to many jobs.  Capping at 64 prevents
/// unbounded HashMap growth on pathological matrix configurations.
///
/// The cap is a hard bound on what the reader is shown, so reaching it is
/// disclosed: see [`RunWatchParser::hidden_jobs`] and `finalize`.
pub(super) const MAX_STREAM_JOBS: usize = 64;

/// Maximum number of distinct OVER-CAP job identities retained for counting.
///
/// The cap disclosure has to report how many JOBS the reader did not see, and
/// a per-line counter structurally cannot: `gh run watch` re-prints every job
/// on every refresh, so one hidden job contributes one suppressed line per
/// frame and the figure grows with POLL COUNT rather than with loss — a
/// 100-job matrix over 20 frames discloses ~720 where 36 jobs are missing.
/// That is a frame count wearing a loss count's label.
///
/// Counting identities instead means retaining them, and retained identities
/// are state, so they take their own explicit bound.  GitHub Actions caps a
/// workflow run at 256 jobs, so 256 covers every legal matrix with the 64
/// tracked ones to spare.  Past it the count degrades to a FLOOR and
/// `finalize` says `at least` rather than overstating its own precision — it
/// never grows without limit and never silently under-reports.
const MAX_HIDDEN_JOBS: usize = 256;

/// Maximum byte length of a `gh` elapsed-time token (`45s`, `1m20s`, `1h2m3s`).
///
/// An explicit upper bound on [`is_duration_token`]'s scan: anything longer is
/// not a duration, so the check exits without walking the tail of a line whose
/// contents skim does not control.
const MAX_DURATION_TOKEN_LEN: usize = 16;

// ============================================================================
// Public entry point
// ============================================================================

/// Run `gh run watch` with streaming compression.
///
/// Supports two modes:
/// - **Pipe mode** (`gh run watch | skim gh run watch`): when stdin is
///   piped and no args are provided, reads from stdin via
///   [`run_streamed_stdin`].
/// - **Spawn mode** (`skim gh run watch <id>`): spawns `gh run watch
///   [args]` as a child process via [`run_streamed_spawned`].
///
/// `--exit-status` flag is propagated to `gh` in spawn mode; non-zero
/// workflow exit is forwarded as the process exit code.
pub(super) fn run_watch(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    // architecture-14: `gh run watch` streams real-time status updates over an
    // extended period.  There is no well-defined JSON envelope for a streaming
    // response, so `--json` cannot be honoured.  Fail loudly per the MUST
    // "fail loud, never silently" design constraint rather than silently
    // returning plain text.
    if ctx.json_output {
        eprintln!(
            "skim: `gh run watch --json` is not supported — \
             `gh run watch` streams live status updates and has no JSON output shape.\n\
             Omit --json to get compressed streaming text output."
        );
        return Ok(ExitCode::FAILURE);
    }

    let parser = Box::new(RunWatchParser::new());

    let label = super::super::build_streaming_label(
        "infra",
        "gh",
        "run watch",
        args,
        ctx.show_stats,
        ctx.analytics_enabled,
    );

    let cfg = StreamConfig {
        analytics_enabled: ctx.analytics_enabled,
        label,
        session_id: ctx.session_id.clone(),
    };

    // Pipe mode: stdin is piped and no run-ID args were given (AD-STR-2).
    if crate::cmd::should_read_stdin(args) {
        return Ok(run_streamed_stdin(parser, cfg));
    }

    // Spawn mode: build `gh run watch [args]` and stream its output.
    let mut gh_args = vec!["run".to_string(), "watch".to_string()];
    gh_args.extend_from_slice(args);

    Ok(run_streamed_spawned(parser, "gh", &gh_args, cfg))
}

// ============================================================================
// Parser implementation
// ============================================================================

// ============================================================================
// Job-name canonicalisation
// ============================================================================

/// `true` when `s` is a `gh`-style elapsed-time token.
///
/// Accepts ASCII digits and the unit letters `h`/`m`/`s`/`d` only, requires at
/// least one digit, and requires the token to END in a unit — so `45s`,
/// `1m20s` and `1h2m3s` match while a job name fragment like `staging` or
/// `progress` does not.  Pure, allocation-free, and bounded by
/// [`MAX_DURATION_TOKEN_LEN`].
fn is_duration_token(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_DURATION_TOKEN_LEN {
        return false;
    }
    let mut has_digit = false;
    for byte in s.bytes() {
        match byte {
            b'0'..=b'9' => has_digit = true,
            b'h' | b'm' | b's' | b'd' => {}
            _ => return false,
        }
    }
    has_digit && s.ends_with(['h', 'm', 's', 'd'])
}

/// Split a trailing ` (ID <digits>)` token off a job entry.
///
/// Returns the name without the token, plus the digits themselves when one was
/// present.  Only a parenthesised run of ASCII digits introduced by the
/// literal `" (ID "` is recognised, so a matrix leg like
/// `build (ubuntu-latest)` — which also ends in `)` — is left intact and
/// yields `None`.
///
/// The ID is RETURNED rather than discarded because it is the only field that
/// tells two jobs printed under one name apart; see [`JobKey`].  Both halves
/// are borrowed slices; nothing is allocated.
fn split_trailing_id(name: &str) -> (&str, Option<&str>) {
    const ID_OPEN: &str = " (ID ";
    let trimmed = name.trim_end();
    if !trimmed.ends_with(')') {
        return (name, None);
    }
    let Some(open) = trimmed.rfind(ID_OPEN) else {
        return (name, None);
    };
    // `trimmed` ends with ')', so it is non-empty and `len - 1` is in bounds
    // and on a char boundary; `get` keeps the slice total regardless.
    let Some(inner) = trimmed.get(open + ID_OPEN.len()..trimmed.len() - 1) else {
        return (name, None);
    };
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return (name, None);
    }
    (trimmed[..open].trim_end(), Some(inner))
}

/// Strip a trailing ` in <duration>` token from a job entry.
///
/// The tail after the last `" in "` must be a full [`is_duration_token`], so a
/// job genuinely named `check in staging` keeps its name.  Returns a borrowed
/// slice; nothing is allocated.
fn strip_trailing_elapsed(name: &str) -> &str {
    const IN_SEP: &str = " in ";
    let trimmed = name.trim_end();
    let Some(at) = trimmed.rfind(IN_SEP) else {
        return name;
    };
    let Some(tail) = trimmed.get(at + IN_SEP.len()..) else {
        return name;
    };
    if !is_duration_token(tail) {
        return name;
    }
    trimmed[..at].trim_end()
}

/// The DISPLAY name of a `gh run watch` job entry: both frame-varying tails
/// removed.
///
/// Every frame re-prints the whole job list with a ticking elapsed time, so
/// `build (ubuntu-latest) in 7s (ID 987654321)` and
/// `build (ubuntu-latest) in 14s (ID 987654321)` are the SAME job and must
/// render under one name.
///
/// This is what the READER is shown.  It is deliberately not the map key on
/// its own — see [`JobKey`], which pairs it with the ID discriminator so two
/// jobs sharing a name cannot collapse onto one entry.  The ID is split off
/// before the duration because `gh` prints them in that order
/// (`… in 1m2s (ID 42)`).
fn canonical_job_name(name: &str) -> &str {
    strip_trailing_elapsed(split_trailing_id(name).0)
}

/// The identity of one job for the life of the stream.
///
/// `name` is what the reader is shown ([`canonical_job_name`]).  `id` is the
/// ` (ID <n>)` discriminator `gh` prints in the live form, retained so that
/// two jobs sharing a name stay two entries: a reusable-workflow call, or a
/// matrix whose leg is not in the name, is exactly that input, and collapsing
/// it hides a job outright.
///
/// `id` is `None` for entries that carry no ID — the pipe / `gh run view`
/// form (`build  Completed`) — where the name is all the identity on offer.
/// A stream that mixes the two forms for one job yields two keys and so
/// re-emits it; that is the NOISE failure, which is recoverable by reading,
/// unlike the loss failure this field exists to prevent.
///
/// Stored as a `String` rather than parsed into an integer: the value is only
/// ever compared and re-printed, so parsing it would add a failure mode and
/// an overflow question in exchange for nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct JobKey {
    name: String,
    id: Option<String>,
}

/// Split a job entry into the identity the parser keys on.
///
/// The name half routes through [`canonical_job_name`] rather than repeating
/// the tail-stripping rule, so that rule has exactly one spelling.  Paying for
/// a second [`split_trailing_id`] scan of a short line buys that: this runs
/// once per output line of a human-paced stream, not in a hot path.
fn job_key(entry: &str) -> JobKey {
    JobKey {
        name: canonical_job_name(entry).to_string(),
        id: split_trailing_id(entry).1.map(str::to_string),
    }
}

/// Job status as tracked by the streaming parser.
#[derive(Debug, Clone, PartialEq)]
enum JobStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
}

/// Streaming parser for `gh run watch` output.
///
/// Tracks job state transitions and emits one summary line per meaningful
/// state change.  Progress dots and unchanged status lines are suppressed.
pub(super) struct RunWatchParser {
    jobs: HashMap<JobKey, JobStatus>,
    totals: StreamTotals,
    any_failure: bool,
    /// Distinct JOBS the [`MAX_STREAM_JOBS`] cap hid from the reader.
    ///
    /// Identities, not lines.  Every frame re-prints every job, so one hidden
    /// job yields one suppressed line per poll; a line counter would disclose
    /// a number that tracks poll count instead of loss, under a label that
    /// says loss.  These are retained ONLY to be counted — nothing reads a
    /// member back — and the set is bounded by [`MAX_HIDDEN_JOBS`].
    /// Disclosed once by `finalize` (#317).
    hidden_jobs: HashSet<JobKey>,
    /// `true` once a NEW hidden job could not be retained because
    /// [`MAX_HIDDEN_JOBS`] was full, making `hidden_jobs.len()` a FLOOR rather
    /// than the exact count.  `finalize` says `at least` when this is set.
    hidden_overflow: bool,
}

impl RunWatchParser {
    pub(super) fn new() -> Self {
        Self {
            jobs: HashMap::new(),
            totals: StreamTotals::default(),
            any_failure: false,
            hidden_jobs: HashSet::new(),
            hidden_overflow: false,
        }
    }

    /// Attempt to parse a job status line from `gh run watch` output.
    ///
    /// `gh run watch` emits lines like:
    /// - `  ✓ build (ubuntu-latest)  Completed`
    /// - `  * build (ubuntu-latest)  In progress`
    /// - `  X test  Failed`
    ///
    /// Status detection and name extraction are kept separate:
    /// 1. Match the leading whitespace-delimited token to determine status.
    /// 2. Strip only the status word(s) that correspond to the detected status
    ///    from the trailing end of the name — never strip unrelated words (e.g.
    ///    do not strip "In progress" when the detected status is `Completed`).
    ///    This prevents job names like `"X-ray test"` from being misclassified
    ///    or truncated.
    /// 3. Reduce what remains to a [`JobKey`], so the returned value is the
    ///    job's identity across frames rather than this frame's snapshot of
    ///    it — and so two jobs printed under one name stay two.
    fn try_parse_job_line(&self, line: &str) -> Option<(JobKey, JobStatus)> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Match on the first whitespace-delimited token to detect the status
        // glyph.  Using split_once prevents `trim_start_matches('X')` from
        // eating the first character of job names that begin with 'X' (e.g.
        // "X-ray test").
        let (status, rest) = match trimmed.split_once(char::is_whitespace) {
            Some(("✓", rest)) | Some(("Pass", rest)) => (JobStatus::Completed, rest),
            Some(("✗", rest)) | Some(("X", rest)) | Some(("Fail", rest)) => {
                (JobStatus::Failed, rest)
            }
            Some(("*", rest)) => (JobStatus::InProgress, rest),
            _ if trimmed.contains("In progress") => (JobStatus::InProgress, trimmed),
            _ if trimmed.contains("Queued") || trimmed.contains("Waiting") => {
                (JobStatus::Queued, trimmed)
            }
            _ => return None,
        };

        // Strip ONLY the status word(s) matching the detected status from the
        // trailing end.  Unconditional stripping of all status words would
        // mangle job names that happen to end with a different status word
        // (e.g. a job legitimately named "build In progress" when status is
        // Completed would have its name corrupted).
        let status_suffixes: &[&str] = match status {
            JobStatus::Completed => &["Completed", "Success"],
            JobStatus::Failed => &["Failed", "Failure"],
            JobStatus::InProgress => &["In progress"],
            JobStatus::Queued => &["Queued", "Waiting"],
        };
        let mut name: &str = rest.trim();
        for suffix in status_suffixes {
            if let Some(stripped) = name.strip_suffix(suffix) {
                name = stripped.trim();
                break;
            }
        }

        // Split off the per-frame tails so the same job keeps one identity
        // across frames, while KEEPING the ID so two jobs sharing a name keep
        // two.  Runs AFTER the status-suffix strip because the tails appear in
        // opposite orders: the pipe form ends with the status word
        // (`build Completed`), the live form ends with the ID
        // (`build in 1m2s (ID 42)`).
        let key = job_key(name);

        if key.name.is_empty() {
            return None;
        }

        Some((key, status))
    }

    /// The label one state transition is emitted under.
    ///
    /// Normally the job's own name.  Two DISTINCT jobs can share a name — a
    /// reusable-workflow call, or a matrix whose leg is not in the name — and
    /// since they are now two keys, both are emitted; printing the bare name
    /// twice would read as one job repeating rather than as two jobs.  The
    /// ` (ID <n>)` discriminator is therefore appended, but ONLY when another
    /// tracked job already holds this name, so the overwhelmingly common case
    /// pays no bytes for a disambiguation it does not need.
    ///
    /// This is a stream, so the first of two same-named jobs is emitted before
    /// the second is known and keeps the bare name; only later lines carry
    /// IDs.  Every individual line is still unambiguous, and an already-written
    /// line cannot be recalled.
    ///
    /// The scan is bounded by [`MAX_STREAM_JOBS`] (at most 64 comparisons) and
    /// runs only on a state transition, never per line.
    fn label_for(&self, key: &JobKey) -> String {
        match &key.id {
            Some(id)
                if self
                    .jobs
                    .keys()
                    .any(|other| other.name == key.name && other.id != key.id) =>
            {
                format!("{} (ID {id})", key.name)
            }
            _ => key.name.clone(),
        }
    }
}

impl StreamingParser for RunWatchParser {
    /// Process one line from `gh run watch` output.
    ///
    /// Returns a compressed summary line on meaningful status transitions,
    /// `None` for noise (progress dots, unchanged status, empty lines).
    fn on_line(&mut self, line: &str) -> Option<String> {
        self.totals.raw_bytes += line.len() + 1;

        // Pass through error lines.
        if line.contains("error:") || line.contains("Error:") {
            let out = line.to_string();
            self.totals.compressed_bytes += out.len() + 1;
            return Some(out);
        }

        // Try to parse a job status transition.
        if let Some((key, new_status)) = self.try_parse_job_line(line) {
            // Cap at MAX_STREAM_JOBS.  The cap is an unavoidable bound on an
            // unbounded stream, so it is LATCHED and disclosed at EOF rather
            // than applied silently (#317).
            //
            // What is latched is the job's IDENTITY, not this line: the same
            // over-cap job re-appears on every frame, so counting lines would
            // disclose poll count in a field labelled as loss.  The retained
            // set carries its own bound ([`MAX_HIDDEN_JOBS`]); past it the
            // count degrades to a floor instead of growing without limit.
            if self.jobs.len() >= MAX_STREAM_JOBS && !self.jobs.contains_key(&key) {
                if self.hidden_jobs.len() < MAX_HIDDEN_JOBS {
                    self.hidden_jobs.insert(key);
                } else if !self.hidden_jobs.contains(&key) {
                    self.hidden_overflow = true;
                }
                return None;
            }

            let old_status = self.jobs.get(&key).cloned();
            let changed = old_status.as_ref() != Some(&new_status);

            if changed {
                self.jobs.insert(key.clone(), new_status.clone());
                // Resolved AFTER the insert so the collision scan sees this
                // job too; `label_for` excludes the key itself by `id`.
                let name = self.label_for(&key);

                let output = match &new_status {
                    JobStatus::Completed => format!("✓ {name}"),
                    JobStatus::Failed => {
                        self.any_failure = true;
                        format!("✗ {name} [FAILED]")
                    }
                    JobStatus::InProgress => format!("⏳ {name}"),
                    JobStatus::Queued => format!("⏸ {name} [queued]"),
                };
                self.totals.compressed_bytes += output.len() + 1;
                return Some(output);
            }
        }

        None // Suppress noise
    }

    /// Emit a final summary line at EOF, plus the cap disclosure if one is due.
    ///
    /// # `{completed}/{total}`, not `{total}/{total}`
    ///
    /// The no-failure branch previously reported `{total}/{total}`, which
    /// claims every job succeeded the moment the parser knows how many there
    /// are — a stream that ends with jobs still queued or in progress (the
    /// normal shape when the reader detaches, or when `gh` is interrupted)
    /// reported them all as successes.  `completed` is the count actually
    /// observed reaching `Completed`, and is what both branches now report.
    ///
    /// # Cap disclosure (#317, ADR-011 class 1)
    ///
    /// When [`MAX_STREAM_JOBS`] hid jobs, the reader was shown strictly less
    /// than the raw tool, so the marker is LOSS-BEARING: it is ADR-011 class 1
    /// and therefore UNCONDITIONAL — never `SKIM_DEBUG`-gated.  It is built by
    /// `output::elision_marker_unbounded` so it carries the mandated
    /// `SKIM_PASSTHROUGH=1` remedy, and it uses the *unbounded* constructor
    /// because the run's true job TOTAL is unknowable here — a job the cap hid
    /// on every frame is a job whose bare existence is all skim ever learned.
    ///
    /// # The figure is a count of JOBS, matching the label it wears
    ///
    /// It used to be the number of suppressed status LINES, which is a
    /// different quantity: `gh run watch` re-prints the whole job list on
    /// every refresh, so a hidden job contributes one line per poll and the
    /// figure scaled with poll count — a 100-job matrix over 20 frames
    /// disclosed roughly 720 where 36 jobs were actually missing.  A number
    /// that moves when nothing was lost is worse than no number, because the
    /// marker's whole purpose is to size the loss.  `hidden_jobs` counts
    /// identities, so the same run now discloses 36 whatever the poll count.
    ///
    /// Past [`MAX_HIDDEN_JOBS`] distinct hidden jobs the count is a FLOOR, and
    /// the marker says `at least` rather than claiming a precision it lost.
    ///
    /// The marker rides on stdout beside the summary, matching the sibling
    /// bound in this module (`streaming::read_line_lossy` appends the 64 KiB
    /// line-cap marker inline the same way).
    fn finalize(self: Box<Self>) -> Option<String> {
        let completed = self
            .jobs
            .values()
            .filter(|s| **s == JobStatus::Completed)
            .count();
        let failed = self
            .jobs
            .values()
            .filter(|s| **s == JobStatus::Failed)
            .count();
        let total = self.jobs.len();
        let hidden = self.hidden_jobs.len();

        let mut out = if total == 0 {
            String::new()
        } else if failed > 0 {
            format!("Run complete: {completed}/{total} succeeded, {failed} FAILED")
        } else {
            format!("Run complete: {completed}/{total} succeeded")
        };

        if hidden > 0 {
            if !out.is_empty() {
                out.push('\n');
            }
            let floor = if self.hidden_overflow {
                "at least "
            } else {
                ""
            };
            let noun = if hidden == 1 { "job" } else { "jobs" };
            out.push_str(&crate::output::elision_marker_unbounded(
                &format!("the {MAX_STREAM_JOBS}-job cap ({floor}{hidden} {noun} hidden)"),
                "jobs",
            ));
        }

        if out.is_empty() { None } else { Some(out) }
    }

    fn totals(&self) -> StreamTotals {
        self.totals
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> RunWatchParser {
        RunWatchParser::new()
    }

    #[test]
    fn test_completed_job_emits_checkmark() {
        let mut p = make_parser();
        let out = p.on_line("  ✓ build (ubuntu-latest)  Completed");
        assert!(out.is_some(), "should emit on completion");
        let line = out.unwrap();
        assert!(line.starts_with('✓'), "line: {line}");
        assert!(line.contains("build"), "line: {line}");
    }

    #[test]
    fn test_failed_job_emits_failure() {
        let mut p = make_parser();
        let out = p.on_line("  X test Failed");
        assert!(out.is_some());
        let line = out.unwrap();
        assert!(line.contains("FAILED"), "line: {line}");
        assert!(p.any_failure);
    }

    #[test]
    fn test_in_progress_job_emits_hourglass() {
        let mut p = make_parser();
        let out = p.on_line("  * build In progress");
        assert!(out.is_some());
        let line = out.unwrap();
        assert!(line.contains('⏳'), "line: {line}");
    }

    #[test]
    fn test_noise_suppressed() {
        let mut p = make_parser();
        // Empty lines and irrelevant text are suppressed.
        assert!(p.on_line("").is_none());
        assert!(p.on_line("...").is_none());
        assert!(p.on_line("GitHub Actions").is_none());
    }

    #[test]
    fn test_no_duplicate_transition() {
        let mut p = make_parser();
        // First in-progress transition emits.
        assert!(p.on_line("  * build In progress").is_some());
        // Same status again → suppressed.
        assert!(p.on_line("  * build In progress").is_none());
    }

    #[test]
    fn test_finalize_all_success() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  ✓ test Completed");
        let summary = Box::new(p).finalize().unwrap();
        assert!(summary.contains("2/2 succeeded"), "summary: {summary}");
    }

    #[test]
    fn test_finalize_with_failures() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  X test Failed");
        let summary = Box::new(p).finalize().unwrap();
        assert!(summary.contains("FAILED"), "summary: {summary}");
    }

    /// Characterisation pin for the failure branch of `finalize`, written
    /// BEFORE the `{completed}/{total}` fix to the success branch.
    ///
    /// The failure branch already reported `completed` correctly; the success
    /// branch reported `total/total`.  Fixing one must not disturb the other,
    /// so the exact string the failure branch produces is pinned here: three
    /// jobs, two completed, one failed → `2/3 succeeded, 1 FAILED`.  A change
    /// that "unifies" the two branches by regressing this one fails here
    /// rather than in review.
    #[test]
    fn test_finalize_failure_branch_reports_completed_not_total() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  ✓ docs Completed");
        p.on_line("  X test Failed");
        let summary = Box::new(p).finalize().expect("three jobs must summarise");
        assert_eq!(summary, "Run complete: 2/3 succeeded, 1 FAILED");
    }

    /// The failure branch must not count in-progress or queued jobs as
    /// succeeded either: one completed, one failed, one still running is
    /// `1/3 succeeded, 1 FAILED` — not `2/3`.
    #[test]
    fn test_finalize_failure_branch_excludes_unfinished_jobs() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  X test Failed");
        p.on_line("  * deploy In progress");
        let summary = Box::new(p).finalize().expect("three jobs must summarise");
        assert_eq!(summary, "Run complete: 1/3 succeeded, 1 FAILED");
    }

    #[test]
    fn test_finalize_empty_no_output() {
        let p = make_parser();
        assert!(Box::new(p).finalize().is_none());
    }

    #[test]
    fn test_max_jobs_cap() {
        let mut p = make_parser();
        // Fill up to MAX_STREAM_JOBS.
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        // Next job should be suppressed (cap reached).
        let out = p.on_line("  * overflow_job In progress");
        assert!(out.is_none(), "should suppress when cap reached");
    }

    // ---- Cap disclosure (#317 / ADR-011 class 1) ----

    /// The cap is a hard bound on what the reader sees, so it must never be
    /// silent.  The marker is loss-bearing (class 1): unconditional, carrying
    /// the exact number of HIDDEN JOBS and the `SKIM_PASSTHROUGH=1` remedy.
    ///
    /// The count is over jobs, not over suppressed lines: four over-cap lines
    /// here name only THREE distinct jobs, because `overflow_a` appears twice.
    /// A line counter reports 4.
    #[test]
    fn test_cap_suppression_is_disclosed_with_exact_counts() {
        let mut p = make_parser();
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        // Three distinct jobs beyond the cap, one of them repeated.
        assert!(p.on_line("  * overflow_a In progress").is_none());
        assert!(p.on_line("  * overflow_b In progress").is_none());
        assert!(p.on_line("  * overflow_a In progress").is_none());
        assert!(p.on_line("  * overflow_c In progress").is_none());

        let out = Box::new(p).finalize().expect("capped run must summarise");
        assert!(
            out.contains("[skim]"),
            "cap must emit an elision marker: {out}"
        );
        assert!(
            out.contains("3 jobs hidden"),
            "marker must carry the exact hidden-JOB count, not the line count: {out}"
        );
        assert!(
            !out.contains("4 jobs hidden") && !out.contains("status lines"),
            "4 is the suppressed-LINE count; the marker must not report it: {out}"
        );
        assert!(
            !out.contains("at least"),
            "the count is exact below MAX_HIDDEN_JOBS: {out}"
        );
        assert!(
            out.contains("64-job cap"),
            "marker must name the bound: {out}"
        );
        assert!(
            out.contains("SKIM_PASSTHROUGH=1"),
            "class-1 marker must carry the remedy: {out}"
        );
        assert!(
            out.starts_with("Run complete:"),
            "summary must still lead: {out}"
        );
    }

    /// The defect this pins: `gh run watch` re-prints every job on every
    /// refresh, so a hidden job costs one suppressed LINE per frame.  Counting
    /// lines made the disclosed figure a function of poll count.
    ///
    /// Scaled-down worked example, same shape as the reported 100-job/20-frame
    /// case: `MAX_STREAM_JOBS + 6` jobs over 20 frames.  Six jobs are hidden;
    /// the frame-line count would be 6 x 20 = 120.  The marker must say 6, and
    /// must say it identically whether the stream ran for 1 frame or 20.
    #[test]
    fn test_cap_disclosure_counts_jobs_not_frames() {
        const HIDDEN: usize = 6;
        const FRAMES: usize = 20;

        let render = |frames: usize| {
            let mut p = make_parser();
            for _ in 0..frames {
                for i in 0..MAX_STREAM_JOBS + HIDDEN {
                    p.on_line(&format!("  * job{i} in 3s (ID {i}) In progress"));
                }
            }
            Box::new(p).finalize().expect("capped run must summarise")
        };

        let many = render(FRAMES);
        assert!(
            many.contains(&format!("{HIDDEN} jobs hidden")),
            "must disclose hidden jobs ({HIDDEN}), not frame lines \
             ({}): {many}",
            HIDDEN * FRAMES
        );
        assert!(
            !many.contains(&format!("{} jobs hidden", HIDDEN * FRAMES)),
            "the frame-line product must never be disclosed as a job count: {many}"
        );

        // Poll count must not move the figure at all.
        assert_eq!(
            render(1),
            many,
            "the disclosure must be a function of loss, not of poll count"
        );
    }

    /// The retained hidden-identity set has its own bound, so the count can
    /// stop being exact.  When it does, the marker must say `at least` rather
    /// than present a floor as a total.
    #[test]
    fn test_hidden_count_degrades_to_a_floor_past_its_own_bound() {
        let mut p = make_parser();
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        // One more distinct over-cap job than the hidden set can retain.
        for i in 0..MAX_HIDDEN_JOBS + 1 {
            assert!(p.on_line(&format!("  * overflow{i} In progress")).is_none());
        }

        let out = Box::new(p).finalize().expect("capped run must summarise");
        assert!(
            out.contains(&format!("at least {MAX_HIDDEN_JOBS} jobs hidden")),
            "past its own bound the count is a floor and must be labelled one: {out}"
        );
        assert!(
            out.contains("SKIM_PASSTHROUGH=1"),
            "class-1 marker must still carry the remedy: {out}"
        );
    }

    /// Singular, so the marker does not read `1 jobs hidden`.
    #[test]
    fn test_single_hidden_job_uses_the_singular_noun() {
        let mut p = make_parser();
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        assert!(p.on_line("  * overflow In progress").is_none());
        assert!(p.on_line("  * overflow In progress").is_none());

        let out = Box::new(p).finalize().expect("capped run must summarise");
        assert!(out.contains("1 job hidden"), "singular noun: {out}");
    }

    /// The marker fires ONLY when the cap actually suppressed something.  A
    /// run that stays under the cap must not pay for a notice about a bound it
    /// never reached.
    #[test]
    fn test_no_cap_marker_when_cap_never_reached() {
        let mut p = make_parser();
        p.on_line("  ✓ build Completed");
        p.on_line("  ✓ test Completed");
        let out = Box::new(p).finalize().unwrap();
        assert!(!out.contains("[skim]"), "no cap reached: {out}");
        assert_eq!(out, "Run complete: 2/2 succeeded");
    }

    // ---- finalize counts completions, not job slots ----

    /// `finalize` must report how many jobs actually COMPLETED.  Before the
    /// fix the no-failure branch printed `{total}/{total}`, so a stream that
    /// ended with every job still in progress reported them all as successes.
    #[test]
    fn test_finalize_does_not_count_unfinished_jobs_as_succeeded() {
        let mut p = make_parser();
        p.on_line("  * build In progress");
        p.on_line("  * test In progress");
        p.on_line("  ✓ lint Completed");
        let summary = Box::new(p).finalize().expect("three jobs must summarise");
        assert_eq!(summary, "Run complete: 1/3 succeeded");
    }

    /// A run that ends with nothing completed must say so, not claim a clean
    /// sweep.  This is the shape the live 70-job matrix produced: every job
    /// still in progress, summarised as `64/64 succeeded`.
    #[test]
    fn test_finalize_reports_zero_when_nothing_completed() {
        let mut p = make_parser();
        p.on_line("  * build In progress");
        p.on_line("  * test In progress");
        let summary = Box::new(p).finalize().expect("two jobs must summarise");
        assert_eq!(summary, "Run complete: 0/2 succeeded");
    }

    // ---- Job-name canonicalisation ----

    /// The defect this fixes: one job re-printed across frames with a ticking
    /// elapsed time must occupy ONE map slot and emit ONE line per real
    /// transition — not one per frame.
    #[test]
    fn test_ticking_frames_do_not_mint_a_new_job_per_frame() {
        let mut p = make_parser();
        let first = p.on_line("* build (ubuntu-latest) in 7s (ID 987654321)");
        assert!(first.is_some(), "first sighting must emit");
        assert_eq!(first.unwrap(), "⏳ build (ubuntu-latest)");

        // Nine more frames, each with a different elapsed time.
        for secs in [14, 21, 28, 35, 42, 49, 56, 63, 70] {
            assert!(
                p.on_line(&format!(
                    "* build (ubuntu-latest) in {secs}s (ID 987654321)"
                ))
                .is_none(),
                "frame at {secs}s must be suppressed as unchanged"
            );
        }

        // The real transition still emits, exactly once.
        let done = p.on_line("✓ build (ubuntu-latest) in 1m17s (ID 987654321)");
        assert_eq!(done.unwrap(), "✓ build (ubuntu-latest)");
        assert!(
            p.on_line("✓ build (ubuntu-latest) in 1m17s (ID 987654321)")
                .is_none()
        );

        let summary = Box::new(p).finalize().unwrap();
        assert_eq!(summary, "Run complete: 1/1 succeeded");
    }

    #[test]
    fn test_canonical_job_name_strips_id_and_elapsed() {
        assert_eq!(
            canonical_job_name("build (ubuntu-latest) in 1m2s (ID 987654321)"),
            "build (ubuntu-latest)"
        );
        assert_eq!(canonical_job_name("test in 45s"), "test");
        assert_eq!(canonical_job_name("test (ID 42)"), "test");
        assert_eq!(canonical_job_name("test"), "test");
    }

    /// Canonicalisation must not eat legitimate name content.  A matrix leg
    /// ends in `)` without being an ID, and a job name may contain the word
    /// `in` followed by something that is not a duration.
    #[test]
    fn test_canonical_job_name_preserves_legitimate_names() {
        for name in [
            "build (ubuntu-latest)",
            "check in staging",
            "deploy (ID prod)",
            "release (ID )",
            "migrate in database",
            "X-ray test",
            "build In progress",
        ] {
            assert_eq!(canonical_job_name(name), name, "must not rewrite {name:?}");
        }
    }

    #[test]
    fn test_duration_token_recognition() {
        for good in ["45s", "1m20s", "1h2m3s", "2d", "0s"] {
            assert!(is_duration_token(good), "must accept {good}");
        }
        for bad in [
            "",
            "staging",
            "s",
            "ms",
            "12",
            "1m20",
            "1x2s",
            "-5s",
            "1234567890123456789s",
        ] {
            assert!(!is_duration_token(bad), "must reject {bad}");
        }
    }

    /// Only the LAST ` (ID …)` is removed; an earlier parenthesised group that
    /// happens to look like one stays in the name.
    #[test]
    fn test_canonical_job_name_strips_only_the_trailing_id() {
        assert_eq!(
            canonical_job_name("job (ID 5) (ID 6)"),
            "job (ID 5)",
            "only the trailing ID token is a frame-varying tail"
        );
    }

    // ---- Job identity: the ID discriminator (regression) ----

    /// The defect this pins: canonicalising the ` (ID <n>)` tail away made two
    /// DIFFERENT jobs share one map key, so every one after the first was
    /// never emitted at all.  A reusable-workflow call, and a matrix whose leg
    /// does not appear in the job name, both print exactly this shape.
    ///
    /// This is a LOSS defect, not the noise defect the canonicalisation was
    /// added for: re-emitting a job is recoverable by reading, a job the
    /// reader never sees is not (#317).
    #[test]
    fn test_same_name_different_id_are_two_jobs() {
        let mut p = make_parser();

        let first = p.on_line("* build in 7s (ID 111)");
        let second = p.on_line("* build in 7s (ID 222)");

        assert!(first.is_some(), "first job must emit");
        assert!(
            second.is_some(),
            "a second job under the same name must emit too, not be swallowed"
        );

        // The de-duplication benefit is preserved: each still collapses across
        // its OWN frames.
        assert!(p.on_line("* build in 14s (ID 111)").is_none());
        assert!(p.on_line("* build in 21s (ID 222)").is_none());

        assert!(p.on_line("✓ build in 30s (ID 111)").is_some());
        assert!(p.on_line("✓ build in 31s (ID 222)").is_some());

        let summary = Box::new(p).finalize().expect("two jobs must summarise");
        assert_eq!(
            summary, "Run complete: 2/2 succeeded",
            "both jobs must be tracked and counted"
        );
    }

    /// Two jobs under one name are two lines, so the lines must be tellable
    /// apart.  The first keeps the bare name — it is emitted before the
    /// collision exists, and this is a stream — and the second carries its ID.
    #[test]
    fn test_same_name_jobs_are_labelled_distinguishably() {
        let mut p = make_parser();
        let first = p.on_line("* build in 7s (ID 111)").expect("first emits");
        let second = p.on_line("* build in 7s (ID 222)").expect("second emits");

        assert_eq!(first, "⏳ build");
        assert_eq!(second, "⏳ build (ID 222)");
        assert_ne!(
            first, second,
            "two jobs must not render as one line repeated"
        );
    }

    /// The overwhelmingly common case pays nothing for the discriminator: a
    /// job whose name is unique renders bare, so the ID costs bytes only where
    /// it resolves a real ambiguity.
    #[test]
    fn test_unique_name_never_carries_the_id() {
        let mut p = make_parser();
        let out = p.on_line("* build in 7s (ID 111)").expect("emits");
        assert_eq!(out, "⏳ build", "no collision → no discriminator");
    }

    #[test]
    fn test_split_trailing_id_returns_the_discriminator() {
        assert_eq!(split_trailing_id("build (ID 42)"), ("build", Some("42")));
        assert_eq!(
            split_trailing_id("build (ubuntu-latest)"),
            ("build (ubuntu-latest)", None)
        );
        assert_eq!(split_trailing_id("build"), ("build", None));
        assert_eq!(split_trailing_id("release (ID )"), ("release (ID )", None));
        assert_eq!(
            split_trailing_id("job (ID 5) (ID 6)"),
            ("job (ID 5)", Some("6"))
        );
    }

    /// The hidden-job count is over identities, so two same-named over-cap
    /// jobs are TWO hidden jobs.  The key shape that keeps them apart in the
    /// map is the same thing that lets the disclosure see both.
    #[test]
    fn test_hidden_count_separates_same_named_jobs() {
        let mut p = make_parser();
        for i in 0..MAX_STREAM_JOBS {
            p.on_line(&format!("  ✓ job{i} Completed"));
        }
        assert!(p.on_line("* build in 7s (ID 111)").is_none());
        assert!(p.on_line("* build in 7s (ID 222)").is_none());

        let out = Box::new(p).finalize().expect("capped run must summarise");
        assert!(
            out.contains("2 jobs hidden"),
            "two identities, not one: {out}"
        );
    }

    #[test]
    fn test_error_line_passes_through() {
        let mut p = make_parser();
        let out = p.on_line("error: workflow run failed");
        assert!(out.is_some());
        assert!(out.unwrap().contains("error:"));
    }

    #[test]
    fn test_already_finished_run_emits_nothing() {
        // An already-finished run may emit no job lines at all.
        let p = make_parser();
        assert!(
            Box::new(p).finalize().is_none(),
            "empty state must produce no summary"
        );
    }

    // ---- should_read_stdin helper (canonical location: crate::cmd) ----

    #[test]
    fn test_should_read_stdin_returns_false_when_args_present() {
        // When args are non-empty, stdin mode must not be selected regardless
        // of terminal state (the tty check is moot at the unit level; we test
        // the args gate here).
        let args: Vec<String> = vec!["12345".to_string()];
        // We can't mock IsTerminal in a unit test, but we can verify the
        // logic short-circuits on args.is_empty().  The helper must return
        // false whenever args is non-empty because the IS_TERMINAL check is
        // AND-joined with args.is_empty().
        // Both branches must be true for stdin mode; a non-empty args slice
        // makes the result false regardless of terminal state.
        //
        // Note: `should_read_stdin` returns false in unit tests because the
        // test binary's stdin IS a terminal (cargo test does not pipe stdin).
        // That is the intended behaviour — no false positives in unit tests.
        assert!(
            !crate::cmd::should_read_stdin(&args),
            "non-empty args must not trigger stdin mode"
        );
    }

    #[test]
    fn test_should_read_stdin_args_gate_short_circuits() {
        // Verify that any non-empty args slice always prevents stdin mode,
        // regardless of the terminal state.  This tests the args.is_empty()
        // gate in isolation: the AND condition means a non-empty args slice
        // short-circuits to false before the is_terminal() check runs.
        //
        // We use several non-empty arg scenarios to confirm the gate.
        let cases: &[&[&str]] = &[&["12345"], &["--exit-status"], &["12345", "--exit-status"]];
        for args_strs in cases {
            let args: Vec<String> = args_strs.iter().map(|s| s.to_string()).collect();
            assert!(
                !crate::cmd::should_read_stdin(&args),
                "non-empty args {:?} must not trigger stdin mode",
                args
            );
        }
    }

    // ---- try_parse_job_line regression tests (Issue #2) ----

    #[test]
    fn test_x_ray_job_name_not_truncated() {
        // "X-ray test" starts with 'X' but "X-ray" is NOT a bare "X" token.
        // The old code used trim_start_matches('X') which would strip the 'X'
        // from the job name.  The new code splits on whitespace and matches
        // only the bare token "X", so "X-ray" is not mis-classified as Failed.
        let p = make_parser();
        // "  ✓ X-ray test Completed" — glyph is '✓', job name is "X-ray test".
        let result = p.try_parse_job_line("  ✓ X-ray test Completed");
        assert!(result.is_some(), "X-ray job should be parsed");
        let (key, status) = result.unwrap();
        assert_eq!(status, JobStatus::Completed, "status should be Completed");
        assert_eq!(
            key.name, "X-ray test",
            "name must not have 'X' stripped: got '{}'",
            key.name
        );
    }

    #[test]
    fn test_x_ray_job_failed_token_form() {
        // When "X" is a bare token (real gh CLI glyph for failed), the job
        // name following it should not be altered.  "X-ray test" after "X "
        // prefix → job name is "X-ray test Failed" stripped of "Failed" suffix.
        let p = make_parser();
        // Bare "X " prefix → Failed; rest is "X-ray test Failed".
        let result = p.try_parse_job_line("  X X-ray test Failed");
        assert!(result.is_some(), "bare-X glyph should parse");
        let (key, status) = result.unwrap();
        assert_eq!(status, JobStatus::Failed);
        assert_eq!(key.name, "X-ray test", "name: {}", key.name);
    }

    #[test]
    fn test_status_suffix_stripped_only_for_detected_status() {
        // "In progress" must NOT be stripped when the detected status is
        // Completed.  A job genuinely named "build In progress" that
        // transitions to Completed should preserve "In progress" in its name.
        let p = make_parser();
        let result = p.try_parse_job_line("  ✓ build In progress Completed");
        assert!(result.is_some(), "should parse");
        let (key, status) = result.unwrap();
        assert_eq!(status, JobStatus::Completed);
        // Only "Completed" is stripped; "In progress" stays in the name.
        assert_eq!(key.name, "build In progress", "name: {}", key.name);
    }

    #[test]
    fn test_checkmark_in_progress_completed_name_not_empty() {
        // Validation requirement: "✓ In progress Completed" must not produce
        // an empty name (i.e. the function must return Some, not None).
        let p = make_parser();
        let result = p.try_parse_job_line("  ✓ In progress Completed");
        assert!(result.is_some(), "name must not be empty for this input");
        let (key, status) = result.unwrap();
        assert_eq!(status, JobStatus::Completed);
        // "Completed" stripped, "In progress" preserved as the job name.
        assert!(
            !key.name.is_empty(),
            "name must not be empty: got '{}'",
            key.name
        );
    }

    // ---- Parser integration (pipe path simulation) ----

    #[test]
    fn test_parser_processes_pipe_scenario_lines() {
        // Mirrors the Tester's scenario:
        //   printf "workflow step 1\nworkflow step 2\ncompleted\n" | skim gh run watch
        //
        // The RunWatchParser receives these lines via on_line().  None match
        // job-status patterns, so all are suppressed; finalize() on empty
        // state returns None (no output, clean exit — not a crash).
        let mut p = make_parser();
        assert!(p.on_line("workflow step 1").is_none());
        assert!(p.on_line("workflow step 2").is_none());
        assert!(p.on_line("completed").is_none());
        let summary = Box::new(p).finalize();
        // Empty jobs map → no summary (None is valid; not an error).
        assert!(
            summary.is_none(),
            "empty job state should produce no summary"
        );
    }

    #[test]
    fn test_parser_pipe_with_job_lines_emits_output() {
        // Validates that the streaming parser correctly handles a mix of real
        // job-status lines delivered via stdin (pipe path).
        let mut p = make_parser();
        let out1 = p.on_line("  * build In progress");
        let out2 = p.on_line("  ✓ build Completed");
        assert!(out1.is_some(), "in-progress line should emit output");
        assert!(out2.is_some(), "completion line should emit output");
        let summary = Box::new(p).finalize().unwrap();
        assert!(
            summary.contains("1/1 succeeded"),
            "summary should report success: {summary}"
        );
    }
}
