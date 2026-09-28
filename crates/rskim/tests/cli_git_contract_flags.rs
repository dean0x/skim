//! ADR-022 — the machine-contract passthrough gate, end to end.
//!
//! `cmd/git/mod.rs::MACHINE_CONTRACT_FLAGS` names a closed set of flags whose
//! presence makes the invocation's output a machine contract: git's own bytes
//! are served raw, unconditionally, ahead of and independent of the ADR-001
//! net-savings verdict.  The gate's unit tests (in that module) pin the
//! *predicate*; this file pins the *contract* — that the bytes a caller
//! receives from `skim git <subcmd> <contract flag>` are byte-for-byte the
//! bytes real git would have written.
//!
//! # Why byte-identity, and not "contains the important parts"
//!
//! These formats exist for programs.  A test asserting that the render still
//! *mentions* a filename passes on a reflowed column layout, a substituted
//! separator, or a `\n` appended after a NUL terminator — every one of which
//! breaks the caller that splits on it, silently, at exit 0.  Measured at
//! `c2b4378`, all of those happened: `git log --stat -n 3` served 374 B against
//! 32 733 B of raw git with zero bytes on stderr; `--stat` / `--shortstat` /
//! `--numstat` / `--name-only` / `--name-status` all served the *same* 374 B,
//! i.e. the flag swallowed without a trace; `--graph` served `log no commits`
//! for a three-commit range; and `--porcelain -z` served 14 B where git writes
//! 13.
//!
//! # Measurement discipline (PF-026)
//!
//! The control is pinned as hard as the subject.  Both sides run through
//! [`HERMETIC_GIT_ENV`], because `skim git …` spawns git as a child and that
//! child inherits skim's environment: pinning one side only would compare two
//! git invocations made under different configurations, which is a worse
//! failure than leaving both unpinned.  `SKIM_PASSTHROUGH` and `SKIM_DEBUG` are
//! removed from the subject — the escape hatch would make every assertion here
//! vacuously true.
//!
//! # PF-027 / PF-031
//!
//! No constant or fixture size here may be adjusted to make an assertion pass.
//! [`assert_gate_is_observable`] carries the one sizing precondition this file
//! has, and fails loud with its own reason when it stops holding.  Nothing
//! reads this repository's history or pins a SHA: every assertion runs against
//! a temp repo whose commits the fixture constructs (PF-031 rule (d)).

mod common;

// ============================================================================
// Hermetic fixture
// ============================================================================

/// Git's config search path, pinned to nothing (PF-009).
///
/// Repo-local `git config` writes cannot make a fixture hermetic on their own:
/// `git init` and every later git invocation still read `~/.gitconfig` and
/// `/etc/gitconfig`, and this file asserts on exact bytes.  `format.pretty`
/// alone would rewrite every `git log` line; `status.short`, `diff.noprefix`,
/// `diff.context` and `color.ui = always` each move bytes too.
///
/// `/dev/null` is a readable, empty config file, which is what makes it a valid
/// value for the two path variables rather than merely an absent one.
///
/// `NO_COLOR` is listed because `common::skim()` sets it on the subject; the
/// control must see the same value or the two environments differ.
const HERMETIC_GIT_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_GLOBAL", "/dev/null"), // ~/.gitconfig
    ("GIT_CONFIG_SYSTEM", "/dev/null"), // /etc/gitconfig
    ("GIT_CONFIG_NOSYSTEM", "1"),       // belt-and-braces for older git
    ("NO_COLOR", "1"),
];

/// Env vars stripped because they do what a pinned config setting would do.
const HERMETIC_GIT_REMOVED: &[&str] = &["GIT_EXTERNAL_DIFF", "GIT_DIFF_OPTS", "GIT_PAGER"];

/// A `git` command that cannot see the developer's configuration.
///
/// Every git invocation in this file — fixture setup AND the raw control —
/// routes through here.  An unpinned control is worse than an unpinned
/// subject: it is the baseline the assertions are measured against (PF-026).
fn hermetic_git() -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    for (var, value) in HERMETIC_GIT_ENV {
        cmd.env(var, value);
    }
    for var in HERMETIC_GIT_REMOVED {
        cmd.env_remove(var);
    }
    cmd
}

/// Run a git command in `dir`, asserting success with a step-labelled panic.
///
/// A setup regression must report itself as a setup failure rather than
/// surfacing later as a confusing product-output mismatch (PF-009).
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let step = args.join(" ");
    let out = hermetic_git()
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("hermetic setup: `git {step}` spawn failed: {e}"));
    assert!(
        out.status.success(),
        "hermetic setup: `git {step}` failed;\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The three commit subjects.
///
/// Deliberately long: `parse_log`'s render is one line per commit, so a short
/// subject shrinks the compressed side and narrows the margin
/// [`assert_gate_is_observable`] depends on.
const SUBJECTS: [&str; 3] = [
    "feat(fixture): seed the contract-flag repository with eight tracked files",
    "refactor(fixture): rewrite every tracked file so each stat block is wide",
    "fix(fixture): rewrite every tracked file once more, for the third commit",
];

/// Files present in every commit, so each commit's stat block names eight paths
/// rather than one.
const TRACKED: [&str; 8] = [
    "src/alpha.rs",
    "src/beta.rs",
    "src/gamma.rs",
    "src/delta.rs",
    "src/epsilon.rs",
    "src/zeta.rs",
    "src/clean.rs",
    "src/dirty.rs",
];

/// Lines written per tracked file per commit.  Every line changes in every
/// commit, so each stat block carries both an insertion and a deletion count.
const LINES_PER_FILE: usize = 12;

/// A fixed epoch so the `Date:` line `git log` renders is identical on every
/// machine and in every timezone.  1 600 000 000 is 2020-09-13.
const BASE_EPOCH: u64 = 1_600_000_000;

/// A hermetic repo with three commits over eight tracked files, plus a dirty
/// working tree: `src/dirty.rs` modified and `untracked.txt` present.
///
/// - three commits, so `--graph` and `-n 3` have something to show;
/// - eight files rewritten per commit, so each `--stat` block is wide enough
///   for [`assert_gate_is_observable`]'s precondition to hold;
/// - `src/clean.rs` tracked and untouched, so `git diff --quiet -- src/clean.rs`
///   is the `#576` no-differences case (exit 0, no output);
/// - `src/dirty.rs` modified, so `git diff --quiet` is the with-differences
///   case (exit 1, no output) and `git status --porcelain` is non-empty.
fn contract_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");

    // PF-009: `-b main` pins the initial branch from creation rather than
    // inheriting the host's `init.defaultBranch`.
    git_in(dir.path(), &["init", "-b", "main", repo.to_str().unwrap()]);
    git_in(&repo, &["config", "user.email", "test@t.invalid"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["config", "commit.gpgsign", "false"]);
    git_in(&repo, &["config", "core.autocrlf", "false"]);
    git_in(&repo, &["config", "diff.algorithm", "myers"]);

    for (i, &subject) in SUBJECTS.iter().enumerate() {
        for path in TRACKED {
            let body: String = (0..LINES_PER_FILE)
                .map(|line| format!("// commit {i} line {line} of {path}\n"))
                .collect();
            std::fs::write(repo.join(path), body).expect("write tracked file");
        }
        git_in(&repo, &["add", "-A"]);

        let ts = (BASE_EPOCH + (i as u64) * 86_400).to_string();
        let out = hermetic_git()
            .args(["commit", "--no-verify", "-m", subject])
            .env("GIT_AUTHOR_DATE", &ts)
            .env("GIT_COMMITTER_DATE", &ts)
            .current_dir(&repo)
            .output()
            .expect("git commit must run");
        assert!(
            out.status.success(),
            "hermetic setup: commit '{subject}' failed;\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Dirty the working tree: one tracked modification, one untracked file.
    std::fs::write(repo.join("src/dirty.rs"), "// modified, not committed\n")
        .expect("modify dirty.rs");
    std::fs::write(repo.join("untracked.txt"), "untracked\n").expect("write untracked.txt");

    (dir, repo)
}

// ============================================================================
// Control and subject
// ============================================================================

/// What the reader would have got without skim at all.
fn raw_git(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    hermetic_git()
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|e| panic!("raw control `git {}` failed to spawn: {e}", args.join(" ")))
}

/// What `skim git …` actually serves for the same argv.
fn served(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut cmd = common::skim();
    // PF-026: the escape hatch would make every assertion below vacuous.
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    // `skim git …` spawns git, and the child inherits this environment.
    for (var, value) in HERMETIC_GIT_ENV {
        cmd.env(var, value);
    }
    for var in HERMETIC_GIT_REMOVED {
        cmd.env_remove(var);
    }
    let mut argv: Vec<&str> = vec!["git"];
    argv.extend_from_slice(args);
    cmd.current_dir(repo)
        .args(argv)
        .output()
        .unwrap_or_else(|e| panic!("subject `skim git {}` failed to spawn: {e}", args.join(" ")))
}

/// Escape a byte string for a failure message without losing non-UTF-8 bytes.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).escape_debug().to_string()
}

/// Assert `skim git <args>` delivers git's own stdout bytes and exit status.
///
/// Returns the raw stdout so the caller can make further, format-specific
/// assertions about what those bytes contain.
fn assert_byte_identical(repo: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let label = args.join(" ");
    let raw = raw_git(repo, args);
    let skim = served(repo, args);

    assert_eq!(
        skim.stdout,
        raw.stdout,
        "`git {label}` is a machine contract, so skim must deliver git's own \
         stdout bytes.\n  raw  ({} B): {}\n  skim ({} B): {}",
        raw.stdout.len(),
        show(&raw.stdout),
        skim.stdout.len(),
        show(&skim.stdout),
    );
    assert_eq!(
        skim.status.code(),
        raw.status.code(),
        "`git {label}`: exit status must match the raw tool's"
    );
    // Both descriptors, not just fd 1.  A gated invocation is a *lossless*
    // passthrough, so every skim notice on this path is ADR-011 class-2 and
    // therefore `SKIM_DEBUG`-gated — zero bytes by default.  Anything skim adds
    // here is context tax at best, and for `--quiet` it is the defect itself
    // (`#576`).  Comparing against raw rather than asserting emptiness keeps a
    // genuine git warning forwarded.
    assert_eq!(
        String::from_utf8_lossy(&skim.stderr),
        String::from_utf8_lossy(&raw.stderr),
        "`git {label}`: a lossless passthrough must not add or drop stderr bytes"
    );
    raw.stdout
}

/// Raw-output floor above which a `git log` byte-identity assertion is evidence
/// about the **gate** rather than about the ADR-001 guard.
///
/// A byte-identity assertion also passes on an *unfixed* binary whenever the
/// net-savings guard elects raw on its own — which it does exactly when skim's
/// parsed render is the *larger* of the two.  So the assertion discriminates
/// only while raw is bigger than that render.
///
/// Measured against the unfixed binary on this exact fixture: `parse_log`'s
/// render is **324 B** for every one of `--stat` / `--shortstat` / `--numstat`
/// / `--name-only` / `--name-status` (all five byte-identical to each other —
/// the flag swallowed), and **15 B** for `--graph`.  Raw runs from 622 B
/// (`--graph`) to 1 738 B (`--stat`).  400 B is 324 rounded up: every case sits
/// above it, and every unfixed render sits below.
///
/// PF-027: lowering this to silence a failure inverts its purpose.  If it
/// trips, the fixture stopped producing a wide enough payload — widen the
/// fixture, or re-derive the bound from a fresh measurement of the render.
const LOG_RENDER_FLOOR: usize = 400;

/// Assert `raw` clears [`LOG_RENDER_FLOOR`], failing loud with the reason.
fn assert_gate_is_observable(raw: &[u8], label: &str) {
    assert!(
        raw.len() >= LOG_RENDER_FLOOR,
        "`git {label}` produced only {} B of raw output.  Below \
         {LOG_RENDER_FLOOR} B the ADR-001 guard could elect raw on its own and \
         this assertion could not distinguish a working gate from a missing one \
         (PF-027: widen the fixture, do not lower this bound).",
        raw.len()
    );
}

// ============================================================================
// git status — the porcelain record formats (F9, F13)
// ============================================================================

/// `--porcelain` is served verbatim, in every version spelling.
///
/// Git documents porcelain as a stability contract for scripts; skim answered
/// it with a prose summary (`status 1 untracked`), so anything parsing it read
/// garbage.  `user_has_flag`'s `=` rule is what makes one entry cover all three
/// spellings.
///
/// Measured on this fixture against the unfixed binary: bare `--porcelain` and
/// `=v1` were **already** byte-identical (33 B raw against a 97 B render, so
/// the ADR-001 guard elected raw on its own — ADR-022's "lucky, not correct"),
/// while `=v2` was **not** (142 B raw against the same 97 B render, so the
/// guard elected `Keep` and served the summary).  The `=v2` case is what makes
/// this test discriminate; the two v1 spellings are the contract stated for
/// completeness.
#[test]
fn status_porcelain_is_byte_identical_to_git() {
    let (_dir, repo) = contract_repo();
    for args in [
        vec!["status", "--porcelain"],
        vec!["status", "--porcelain=v1"],
        vec!["status", "--porcelain=v2"],
    ] {
        let raw = assert_byte_identical(&repo, &args);
        assert!(
            !raw.is_empty(),
            "fixture must be dirty, or `git {}` has nothing to protect",
            args.join(" ")
        );
    }
}

/// F9: `--porcelain=v2 --branch` keeps every `#`-prefixed header line.
///
/// The originally reported defect — a dropped `# branch.ab` — does not exist:
/// the ahead/behind counts are parsed and rendered correctly.  What the parser
/// really loses is `# branch.oid`, which has no prefix match anywhere in
/// `status.rs` and therefore no representation in any skim render, plus the
/// wholesale substitution of the format itself.  Both are fixed by the same
/// thing: not reaching the parser.  Asserting `# branch.oid` specifically is
/// what makes this non-vacuous — no summary render can produce it.
///
/// Measured on this fixture against the unfixed binary: 215 B of raw git
/// against a 97 B summary, exit 0, **zero** bytes on stderr.  That is ADR-022's
/// central example — adding `--branch` is what lets compression win, so the
/// same format is protected or reshaped depending on repository state.
#[test]
fn status_porcelain_v2_branch_keeps_every_header_line() {
    let (_dir, repo) = contract_repo();
    let raw = assert_byte_identical(&repo, &["status", "--porcelain=v2", "--branch"]);
    let text = String::from_utf8(raw).expect("porcelain v2 is UTF-8 on this fixture");

    for header in ["# branch.oid ", "# branch.head "] {
        assert!(
            text.lines().any(|l| l.starts_with(header)),
            "`{header}` must reach the reader verbatim; got:\n{text}"
        );
    }
    assert!(
        !text.lines().any(|l| l.starts_with("status ")),
        "skim's own summary header must not appear in a porcelain stream:\n{text}"
    );
}

/// `--porcelain -z` is byte-exact, including the absence of a trailing newline.
///
/// `-z` exists for deterministic splitting, and a NUL-terminated body never
/// ends in `\n`.  Measured at `c2b4378` on the real repository raw 13 B against
/// skim's 14 B, and on this fixture 33 B against 34 B — in both cases a final
/// `\n` after the terminating NUL, so every consumer splitting on NUL saw one
/// extra empty record.  Note bare `--porcelain` does **not** show the defect
/// (its body already ends in `\n`, so the conditional guard does not fire); the
/// NUL variants are the only place it is observable.
///
/// The gate routes this through `run_passthrough`, whose `write_to_stdout` does
/// not append a newline, so the byte is gone here; the separate
/// `emit_raw_passthrough_exact` fix covers the ADR-001 `Passthrough`-verdict
/// path, which a contract flag no longer reaches.
#[test]
fn status_porcelain_nul_stream_gains_no_trailing_newline() {
    let (_dir, repo) = contract_repo();
    let raw = assert_byte_identical(&repo, &["status", "--porcelain", "-z"]);

    assert!(
        raw.ends_with(b"\0"),
        "fixture precondition: a `-z` porcelain stream must end in NUL; got: {}",
        show(&raw)
    );
    let skim = served(&repo, &["status", "--porcelain", "-z"]);
    assert!(
        !skim.stdout.ends_with(b"\n"),
        "a NUL-delimited stream must not gain a trailing newline; got: {}",
        show(&skim.stdout)
    );
}

/// The gate matches `-z` inside a **bundled short cluster**.
///
/// `git status -sz` is a real invocation, and `status.rs`'s own
/// conflicting-flag scan has always been cluster-aware — so an exact-token gate
/// would have been *weaker* than the shipped behaviour rather than a
/// pre-existing limitation.  Pinning `-sz` stops the cluster-awareness from
/// silently regressing into an exact-token match.
///
/// Measured on this fixture against the unfixed binary: 33 B raw against 34 B
/// — the same trailing-`\n`-after-NUL defect, reached through the bundled
/// spelling, so this case is non-vacuous as well as structural.
#[test]
fn status_short_nul_cluster_is_byte_identical_to_git() {
    let (_dir, repo) = contract_repo();
    let raw = assert_byte_identical(&repo, &["status", "-sz"]);
    assert!(
        raw.ends_with(b"\0") && !raw.ends_with(b"\n"),
        "fixture precondition: `git status -sz` must be NUL-terminated; got: {}",
        show(&raw)
    );
}

/// `-s` alone is NOT a contract and must keep reaching the status handler.
///
/// Short format is a human-facing rendering that `status.rs` translates
/// faithfully.  Adding `s` to `CONTRACT_SHORT_OPTS` would turn `skim git status
/// -sb` — which has its own pinned test in `cli_git.rs` — into raw passthrough,
/// so the exclusion is pinned here from the other side.
#[test]
fn status_short_alone_is_not_gated() {
    let (_dir, repo) = contract_repo();
    let plain = served(&repo, &["status", "-s"]);
    assert!(
        plain.status.success(),
        "`skim git status -s` must still run through the status handler; stderr: {}",
        show(&plain.stderr)
    );
    assert!(
        !plain.stdout.is_empty(),
        "`skim git status -s` on a dirty fixture must produce output"
    );
}

// ============================================================================
// git log — F6: six flags, all swallowed
// ============================================================================

/// F6: each stat-family flag on `git log` is served verbatim, and the five no
/// longer collapse onto one another.
///
/// The sharpest signature of the defect was that `--stat`, `--shortstat`,
/// `--numstat`, `--name-only` and `--name-status` all produced the *same*
/// 374 bytes: the flag reached git, git answered it, and `parse_log`'s
/// commit-line filter discarded the answer before rendering.  Pairwise
/// distinctness falsifies that mechanism directly — byte-identity alone would
/// not, because five wrong-but-equal answers and five right-and-different ones
/// are told apart only by comparing them to each other.
///
/// Re-measured on this fixture against the unfixed binary, and the signature
/// reproduces exactly: all five served **324 B**, byte-identical to one
/// another, against raw outputs of 730 / 903 / 951 / 1 039 / 1 738 B, at exit 0
/// with **zero** bytes on stderr.  Every one clears [`LOG_RENDER_FLOOR`], so
/// every one discriminates.
#[test]
fn log_stat_family_is_byte_identical_and_pairwise_distinct() {
    let (_dir, repo) = contract_repo();
    let flags = [
        "--stat",
        "--shortstat",
        "--numstat",
        "--name-only",
        "--name-status",
    ];

    let mut seen: Vec<(&str, Vec<u8>)> = Vec::new();
    for flag in flags {
        let raw = assert_byte_identical(&repo, &["log", flag, "-n", "3"]);
        assert_gate_is_observable(&raw, &format!("log {flag} -n 3"));
        for (other, bytes) in &seen {
            assert_ne!(
                bytes, &raw,
                "`git log {flag}` and `git log {other}` served identical bytes — \
                 that is the shape of the flag being swallowed, not honoured"
            );
        }
        seen.push((flag, raw));
    }

    let stat = &seen[0].1;
    let text = String::from_utf8_lossy(stat);
    assert!(
        text.contains(" | ") && text.contains("changed"),
        "`git log --stat` must carry its stat block; got:\n{text}"
    );
    // `commit <40 hex>` is raw git's shape and exists nowhere in skim's
    // vocabulary: `parse_log` renders an injected `%h`, a 7-character
    // abbreviation with no `commit ` prefix.  Three of them is positive proof
    // that the reader got git's bytes rather than a summary resembling them.
    let full_headers = text
        .lines()
        .filter(|l| {
            l.strip_prefix("commit ")
                .is_some_and(|h| h.len() >= 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .count();
    assert_eq!(
        full_headers, 3,
        "`git log --stat -n 3` must carry three full `commit <sha>` headers; got:\n{text}"
    );
}

/// F6, second mechanism: `--graph` no longer inverts the answer.
///
/// `--graph` prefixes every commit with `* `, so `is_commit_line`'s
/// `split_once(' ')` took `"*"` as the candidate hash, matched nothing, and
/// `parse_log` reported **`log no commits`** — 15 bytes, exit 0, empty stderr —
/// for a range containing three.  That is not a lossy summary but an inverted
/// answer, which a reader records as positive evidence of absence (the PF-021
/// shape).
///
/// This is also the file's strongest case.  Re-measured on this fixture against
/// the unfixed binary: 622 B of raw git against stdout whose whole content is
/// `log no commits\n`, 15 B, exit 0, zero stderr.  Because that render is 15 B,
/// the ADR-001 guard elects `Keep` for any raw output above roughly 20 B — so
/// nothing but the gate can be delivering these bytes.
#[test]
fn log_graph_serves_the_commits_and_not_no_commits() {
    let (_dir, repo) = contract_repo();
    let raw = assert_byte_identical(&repo, &["log", "--graph", "-n", "3"]);
    assert_gate_is_observable(&raw, "log --graph -n 3");

    let text = String::from_utf8_lossy(&raw);
    assert!(
        !text.contains("no commits"),
        "`git log --graph` must not report an empty range for a three-commit \
         fixture; got:\n{text}"
    );
    for subject in SUBJECTS {
        assert!(
            text.contains(subject),
            "`git log --graph` must carry the subject {subject:?}; got:\n{text}"
        );
    }
    let railed = text.lines().filter(|l| l.starts_with("* commit ")).count();
    assert_eq!(
        railed, 3,
        "`git log --graph -n 3` must carry three graph-railed commit headers; got:\n{text}"
    );
}

/// `--format` / `--pretty` still pass through after being hoisted out of
/// `log.rs` into the shared gate.
///
/// The hoist is behaviour-preserving for these two, and this is its regression
/// barrier: a mistake in the hoist shows up here rather than as a reshaped
/// custom format somebody notices in production.
#[test]
fn log_user_format_strings_survive_the_hoist() {
    let (_dir, repo) = contract_repo();
    for args in [
        vec!["log", "--format=%H", "-n", "3"],
        vec!["log", "--pretty=oneline", "-n", "3"],
    ] {
        let raw = assert_byte_identical(&repo, &args);
        assert!(
            !raw.is_empty(),
            "`git {}` must produce output",
            args.join(" ")
        );
    }
}

/// `--oneline` is NOT in the gate and must keep reaching the log handler.
///
/// `log.rs` answers it by stripping it and injecting an equivalent `--format`,
/// and three tests in `cli_git.rs` pin that behaviour.  This is the smoke check
/// that the hoist left the handler reachable.
///
/// # Why this asserts so little
///
/// There is no byte assertion available here in either direction.  Measured on
/// this fixture, the unfixed binary already serves raw for `--oneline` — 244 B
/// raw against a 257 B render, so the guard elects `Passthrough` — which means
/// "differs from raw" is false today and an `assert_ne!` here would fail on a
/// correct binary.  Equally, "equals raw" cannot distinguish the handler's
/// guard-elected passthrough from the gate having swallowed the flag.  The
/// exclusion itself is pinned where it is decidable: `non_contract_flags_are_not_gated`
/// in `cmd/git/mod.rs`'s unit tests asserts the predicate directly.
#[test]
fn log_oneline_still_reaches_the_log_handler() {
    let (_dir, repo) = contract_repo();
    let skim = served(&repo, &["log", "--oneline", "-n", "3"]);
    assert!(
        skim.status.success(),
        "`skim git log --oneline` must still run through the log handler; stderr: {}",
        show(&skim.stderr)
    );
    assert!(
        !skim.stdout.is_empty(),
        "`skim git log --oneline -n 3` on a three-commit fixture must produce output"
    );
}

// ============================================================================
// git diff — the hoisted spelling, and #576
// ============================================================================

/// `git diff`'s own stat-family gate still holds after the hoist.
///
/// A **move barrier, not a defect pin**: these flags were already gated in
/// `diff/mod.rs`, so the unfixed binary is byte-identical here too (measured).
/// Its job is to catch a mistake in the hoist — a flag dropped from the list
/// as it moved — rather than to demonstrate a fix.
#[test]
fn diff_stat_family_survives_the_hoist() {
    let (_dir, repo) = contract_repo();
    for flag in ["--stat", "--numstat", "--name-only", "--name-status"] {
        let raw = assert_byte_identical(&repo, &["diff", flag]);
        assert!(
            !raw.is_empty(),
            "fixture must have an unstaged modification, or `git diff {flag}` \
             has nothing to protect"
        );
    }
}

/// `--raw` is git's diff **record** format, and it is served verbatim on both
/// streams.
///
/// `:100644 100644 <pre> <post> M\tsrc/dirty.rs` — colon-prefixed,
/// tab-delimited, fixed field order, a format that exists to be split on.  It
/// is asserted here by actually splitting it, because a reflowed or
/// re-separated render still *mentions* the path.
///
/// # Where the non-vacuity comes from — and where it does not
///
/// Measured on this fixture against the unfixed binary, the two branches are
/// very different, and only one of them discriminates:
///
/// - **Differences present** (`git diff --raw`): 46 B raw against 46 B served,
///   exit 0, zero stderr — **already byte-identical**.  Not the ADR-001 guard:
///   under `SKIM_DEBUG=1` no guardrail banner fires and `--show-stats` reports
///   `0.0% reduction`.  It is the *empty-parse early return* — `--raw` is not a
///   unified diff, so `parse_unified_diff` yields no files and `run_diff`'s
///   `file_diffs.is_empty()` arm writes `raw_diff` straight to stdout before
///   `savings_decision` is ever consulted.  So this half is protected by the
///   **unified-diff parser failing**, which is a narrower accident than either
///   of ADR-022's two blunt mechanisms: it holds only while the parser cannot
///   read the format at all.
/// - **No differences** (`git diff --raw --cached`): raw writes 0 B on both
///   streams; the unfixed binary wrote **11 B** of `No changes\n` to stderr.
///   That is the discriminating half, and it is the same `#576` shape as
///   `--quiet` — skim-authored bytes on a stream the raw tool left empty.
///
/// Both are asserted, and the `--cached` case is what makes this test fail if
/// the `--raw` entry is removed from `MACHINE_CONTRACT_FLAGS`.
///
/// The stronger evidence for the entry is on `git log`, not `git diff`:
/// `git log --raw -n 3` served **324 B** against **1 695 B** of raw git at
/// exit 0 with zero stderr — byte-identical to what `--stat` served, i.e. the
/// flag swallowed without a trace, `parse_log`'s commit-line filter discarding
/// every `:100644` record.  That mechanism is the one
/// `log_stat_family_is_byte_identical_and_pairwise_distinct` already
/// characterises for its five siblings; the predicate-level guarantee for
/// `--raw` is pinned in `cmd/git/mod.rs`'s unit tests.
#[test]
fn diff_raw_record_format_is_byte_identical_on_both_streams() {
    let (_dir, repo) = contract_repo();

    // Differences present: one record line, exit 0.
    let raw = assert_byte_identical(&repo, &["diff", "--raw"]);
    let text = String::from_utf8(raw).expect("a `--raw` record is UTF-8 on this fixture");
    let records: Vec<&str> = text.lines().filter(|l| l.starts_with(':')).collect();
    assert_eq!(
        records.len(),
        1,
        "the fixture has exactly one unstaged modification, so `git diff --raw` \
         must carry exactly one `:`-prefixed record; got:\n{text}"
    );

    // Split it the way a caller would.  A render that reflowed the columns or
    // substituted the separator passes a "contains the path" check and fails
    // this one.
    let (fields, path) = records[0]
        .split_once('\t')
        .unwrap_or_else(|| panic!("a `--raw` record is tab-framed; got: {:?}", records[0]));
    assert_eq!(
        path, "src/dirty.rs",
        "the record's post-tab field is the path; got: {path:?}"
    );
    let cols: Vec<&str> = fields.split(' ').collect();
    assert_eq!(
        cols.len(),
        5,
        "a `--raw` record is `:<srcmode> <dstmode> <srcsha> <dstsha> <status>` — \
         five space-separated fields before the tab; got {cols:?}"
    );
    assert_eq!(
        cols[0], ":100644",
        "first field carries the colon prefix and the source mode; got: {:?}",
        cols[0]
    );
    assert_eq!(
        cols[4], "M",
        "the fixture modifies `src/dirty.rs`, so the status letter is `M`; got: {:?}",
        cols[4]
    );

    // No differences: the discriminating half.  Raw is silent on both streams;
    // the unfixed binary wrote `No changes\n` (11 B) to stderr.
    let staged = assert_byte_identical(&repo, &["diff", "--raw", "--cached"]);
    assert!(
        staged.is_empty(),
        "fixture precondition: nothing is staged, so `git diff --raw --cached` \
         must produce no records; got: {}",
        show(&staged)
    );
    let skim = served(&repo, &["diff", "--raw", "--cached"]);
    assert!(
        skim.stderr.is_empty(),
        "`--raw` with no differences must write nothing to stderr — not even \
         `No changes`; got: {}",
        show(&skim.stderr)
    );
    assert_eq!(
        skim.status.code(),
        Some(0),
        "`git diff --raw --cached` with nothing staged must exit 0"
    );
}

/// `#576`: `git diff --quiet` produces **no output at all**, on either stream.
///
/// `--quiet` is an exit-code contract: the answer is the status, and every byte
/// skim writes is output the caller explicitly asked not to receive.  On the
/// no-differences path `run_diff`'s empty-diff arm wrote `No changes` to stderr
/// — the reported defect, measured on this fixture against the unfixed binary
/// as exactly **11 bytes** (`No changes\n`) at exit 0.  The with-differences
/// path was already silent, because git exits 1 and the non-zero arm prints
/// nothing (measured: 0 B on both streams, exit 1); it is asserted here so a
/// later change cannot break it while fixing the other half.
///
/// The exit status is forwarded by `run_passthrough`'s `map_exit_code`, which
/// maps `Some(0)` to success and everything else to failure — exactly
/// `--quiet`'s documented 0/1 contract.  (Codes above 1, e.g. git's 128 for a
/// bad revision, collapse to 1; that is pre-existing `run_passthrough`
/// behaviour and outside `--quiet`'s contract.)
#[test]
fn diff_quiet_writes_nothing_and_forwards_its_exit_code() {
    let (_dir, repo) = contract_repo();

    // No differences: git exits 0 and writes nothing.
    let clean = served(&repo, &["diff", "--quiet", "--", "src/clean.rs"]);
    assert_eq!(
        clean.status.code(),
        Some(0),
        "`--quiet` on an unmodified path must exit 0"
    );
    assert!(
        clean.stdout.is_empty(),
        "`--quiet` must write nothing to stdout; got: {}",
        show(&clean.stdout)
    );
    assert!(
        clean.stderr.is_empty(),
        "#576: `--quiet` must write nothing to stderr — not even `No changes`; got: {}",
        show(&clean.stderr)
    );

    // Differences present: git exits 1 and writes nothing.
    let dirty = served(&repo, &["diff", "--quiet", "--", "src/dirty.rs"]);
    assert_eq!(
        dirty.status.code(),
        Some(1),
        "`--quiet` on a modified path must exit 1 — the flag's entire contract"
    );
    assert!(
        dirty.stdout.is_empty() && dirty.stderr.is_empty(),
        "`--quiet` must write nothing on either stream;\n  stdout: {}\n  stderr: {}",
        show(&dirty.stdout),
        show(&dirty.stderr)
    );

    // The control agrees on both, so the contract asserted above is git's and
    // not this file's invention (PF-026).
    for (path, code) in [("src/clean.rs", Some(0)), ("src/dirty.rs", Some(1))] {
        let raw = raw_git(&repo, &["diff", "--quiet", "--", path]);
        assert_eq!(
            raw.status.code(),
            code,
            "raw control: `git diff --quiet -- {path}`"
        );
        assert!(
            raw.stdout.is_empty() && raw.stderr.is_empty(),
            "raw control: `git diff --quiet -- {path}` must be silent"
        );
    }
}

/// `--exit-code` is `--quiet`'s contract with the diff still printed.
///
/// Measured byte-identical on the unfixed binary too — but for a reason worth
/// naming: git exits 1, and "non-zero exit ⇒ forward raw" is the *second* of
/// the two blunt mechanisms ADR-022 says were doing this work by accident.  It
/// protects only failing invocations, so `git diff --exit-code` on an
/// unmodified tree (exit 0) had no protection at all.  The gate makes the
/// behaviour intentional on both branches rather than incidental on one.
#[test]
fn diff_exit_code_is_byte_identical_and_forwards_status() {
    let (_dir, repo) = contract_repo();

    // Differences present: git prints the diff and exits 1.
    let raw = assert_byte_identical(&repo, &["diff", "--exit-code", "--", "src/dirty.rs"]);
    assert!(
        !raw.is_empty(),
        "`git diff --exit-code` on a modified path must print the diff"
    );
    let skim = served(&repo, &["diff", "--exit-code", "--", "src/dirty.rs"]);
    assert_eq!(
        skim.status.code(),
        Some(1),
        "`--exit-code` must report 1 when differences exist"
    );

    // No differences: exit 0 and silence.  This is the branch the non-zero-exit
    // mechanism never covered, so it is the discriminating half — the unfixed
    // binary wrote `No changes` to stderr here.
    let clean = served(&repo, &["diff", "--exit-code", "--", "src/clean.rs"]);
    assert_eq!(
        clean.status.code(),
        Some(0),
        "`--exit-code` must report 0 when no differences exist"
    );
    assert!(
        clean.stdout.is_empty() && clean.stderr.is_empty(),
        "`--exit-code` on an unmodified path must be silent on both streams;\n  \
         stdout: {}\n  stderr: {}",
        show(&clean.stdout),
        show(&clean.stderr)
    );
}

// ============================================================================
// The gate vs. `--json` — skim's OWN machine contract (ADR-022 regression)
// ============================================================================
//
// The gate is keyed on "flags and syntax the caller typed" (ADR-022,
// Consequences).  `--json` is such a flag, and what it asks for is *skim's*
// machine-readable envelope — so honouring it is not a false negative in the
// gate: the machine contract this caller requested is the JSON envelope, not
// git's porcelain.
//
// The gate as first landed fired ahead of every handler, while `--json` is
// extracted *inside* handlers (`extract_output_format`) and `run_passthrough`
// forwarded the user's argv to git unfiltered.  So a skim-only flag reached
// git and git rejected it.  Measured on this branch before the fix:
//
//   $ skim git status --porcelain --json
//   error: unknown option `json'
//   usage: git status [<options>] [--] [<pathspec>...]
//   exit=1                                    (0 B on stdout)
//
// against `c2b4378`, which served a 2 500 B JSON envelope at exit 0 with the
// class-1 marker on stderr.  `README.md`'s "All subcommands support `--json`
// for machine-readable output" was false for every
// (contract flag × compressed subcommand) pair.
//
// Asserting the exit code is the point: the defect's signature is exit 1 with
// an empty stdout, and a test that only inspected stdout for JSON-ish text
// would also pass on a future recurrence that returns nothing at all.

/// Subcommands that own both a compressing handler and a `--json` envelope,
/// each paired with a contract flag that is real git on that subcommand.
///
/// `status`/`log` are the two the gate *introduced* the defect on (`c2b4378`
/// served JSON for both); `diff` is the one it inherited, because
/// `diff/mod.rs`'s own pre-hoist gate had the identical bug.  All three are
/// listed because `README.md:98` states the contract for all of them.
/// The second element is a top-level key the subcommand's envelope must carry.
/// Asserting a *named* key rather than "parses as JSON" is what keeps this
/// non-vacuous: `status`/`log` are keyed `operation`, `git diff`'s modelled
/// envelope is keyed `files_changed`, and its empty-parse envelope — which is
/// what a `--stat` payload produces, since `--stat` emits no `diff --git`
/// header for `parse_diff` to find — is keyed `files` + `raw`.  A single
/// shared key would have to be weakened to something all three satisfy, and
/// "is a JSON object" is satisfied by `{}`.
const JSON_WITH_CONTRACT_FLAG: &[(&[&str], &str)] = &[
    (&["status", "--porcelain", "--json"], "operation"),
    (&["status", "--porcelain=v2", "--json"], "operation"),
    (&["status", "-z", "--json"], "operation"),
    (&["log", "--stat", "-n", "2", "--json"], "operation"),
    (&["log", "--name-only", "-n", "2", "--json"], "operation"),
    (&["diff", "--stat", "--json"], "raw"),
];

/// Assert `skim git <args>` produced a JSON envelope at exit 0.
///
/// Parses the payload rather than substring-matching it: a `--json` contract
/// that emits something merely JSON-*shaped* is the defect one layer down.
fn assert_json_envelope(repo: &std::path::Path, args: &[&str], required_key: &str) {
    let label = args.join(" ");
    let skim = served(repo, args);

    assert_eq!(
        skim.status.code(),
        Some(0),
        "`skim git {label}` must exit 0 — exit 1 with empty stdout is the \
         signature of `--json` being forwarded to git.\n  stdout: {}\n  stderr: {}",
        show(&skim.stdout),
        show(&skim.stderr)
    );
    assert!(
        !skim.stdout.is_empty(),
        "`skim git {label}` must write a JSON envelope to stdout; stderr: {}",
        show(&skim.stderr)
    );

    let text = String::from_utf8(skim.stdout.clone())
        .unwrap_or_else(|e| panic!("`skim git {label}`: stdout must be UTF-8 JSON: {e}"));
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("`skim git {label}`: stdout must parse as JSON ({e});\n  got: {text}")
    });
    assert!(
        parsed.get(required_key).is_some(),
        "`skim git {label}`: the envelope must carry a `{required_key}` key;\n  got: {text}"
    );
}

/// The regression: `--json` alongside a contract flag reaches the handler.
///
/// Covers the matrix rather than one case — the gate is one shared arm over
/// seven subcommands, so a fix that repaired `status` alone would leave the
/// same defect live on its siblings.
#[test]
fn json_with_a_contract_flag_still_serves_skims_envelope() {
    let (_dir, repo) = contract_repo();
    for (args, required_key) in JSON_WITH_CONTRACT_FLAG {
        assert_json_envelope(&repo, args, required_key);
    }
}

/// Control: `--json` with no contract flag must keep working.
///
/// This is the half of the matrix that never broke.  Pinning it stops a fix
/// aimed at the gate from being "corrected" into one that disarms `--json`
/// generally.
#[test]
fn json_without_a_contract_flag_still_serves_skims_envelope() {
    let (_dir, repo) = contract_repo();
    for (args, required_key) in [
        (vec!["status", "--json"], "operation"),
        (vec!["log", "-n", "2", "--json"], "operation"),
        (vec!["diff", "--json"], "files_changed"),
    ] {
        assert_json_envelope(&repo, &args, required_key);
    }
}

/// Do not regress the gate itself: a contract flag WITHOUT `--json` stays raw.
///
/// The `--json` fix narrows the gate's guard, and the failure mode of getting
/// that narrowing wrong is the gate declining on argv it must still catch.
/// Every pair below is the `--json` case from
/// [`JSON_WITH_CONTRACT_FLAG`] with the `--json` token removed, so the two
/// tests bracket the guard from both sides.
#[test]
fn contract_flag_without_json_is_still_served_raw() {
    let (_dir, repo) = contract_repo();
    for args in [
        vec!["status", "--porcelain"],
        vec!["status", "--porcelain=v2"],
        vec!["status", "-z"],
        vec!["log", "--stat", "-n", "2"],
        vec!["log", "--name-only", "-n", "2"],
        vec!["diff", "--stat"],
    ] {
        assert_byte_identical(&repo, &args);
    }
}

/// `--json` *after* a bare `--` is a pathspec, not skim's view flag.
///
/// `extract_json_flag` accepts only a bare `--json` before the separator, so
/// the gate's `--json` check must use the same rule or the two disagree: a gate
/// that disarmed here would route `-- --json` into a handler that then forwards
/// it to git as a pathspec, which is a different answer than git's own.
///
/// Measured identical on `c2b4378` and on this branch — git treats `--json` as
/// a pathspec that matches nothing, so the contract is "whatever git says",
/// which is what [`assert_byte_identical`] asserts.
#[test]
fn json_after_the_separator_does_not_disarm_the_gate() {
    let (_dir, repo) = contract_repo();
    assert_byte_identical(&repo, &["status", "--porcelain", "--", "--json"]);
    assert_byte_identical(&repo, &["diff", "--stat", "--", "--json"]);
}

// ============================================================================
// The gate vs. `--mode` — inert on a contract format, and must not reach git
// ============================================================================
//
// `--mode` gets the OPPOSITE answer to `--json`, and the asymmetry is the
// finding rather than an inconsistency.  `--json` names an output contract skim
// can actually produce for these subcommands, so the gate stands down and the
// handler serves it.  `--mode` selects a *view of source code*, and a
// `--stat` / `--numstat` / `--porcelain` payload is not source code — there is
// no view to select.  So the gate must keep firing, which means the token has
// to be dropped from the forwarded argv instead.
//
// Unlike `--json`, this was never a `c2b4378` regression: the per-command
// spellings in `diff/mod.rs` and `show.rs` forwarded `--mode` to git too, so
// both binaries error identically —
//
//   $ skim git diff --stat --mode=full        # c2b4378 AND this branch
//   error: invalid option: --mode=full
//   exit=1
//
// — which is why the ledger records the gate as having *widened* a defect the
// hoisted spellings already had, rather than as having introduced one.

/// `--mode` alongside a contract flag serves git's bytes instead of erroring.
///
/// Both spellings (`--mode=v` and `--mode v`) and both a mode that is
/// byte-faithful (`full`) and one that is lossy (`pseudo`), because the strip
/// must be value-agnostic: it drops the flag because the *payload* has no view
/// to select, not because a particular value happens to be a no-op.
#[test]
fn mode_flag_with_a_contract_flag_serves_raw_instead_of_erroring() {
    let (_dir, repo) = contract_repo();
    for (args, without) in [
        (
            vec!["diff", "--stat", "--mode=full"],
            vec!["diff", "--stat"],
        ),
        (
            vec!["diff", "--numstat", "--mode", "pseudo"],
            vec!["diff", "--numstat"],
        ),
        (
            vec!["log", "--stat", "-n", "2", "--mode=structure"],
            vec!["log", "--stat", "-n", "2"],
        ),
    ] {
        let label = args.join(" ");
        let skim = served(&repo, &args);
        let raw = raw_git(&repo, &without);

        assert_eq!(
            skim.status.code(),
            raw.status.code(),
            "`skim git {label}` must exit as git does for `git {}` — a skim-only \
             flag must never reach git.\n  stderr: {}",
            without.join(" "),
            show(&skim.stderr)
        );
        assert_eq!(
            skim.stdout,
            raw.stdout,
            "`skim git {label}` is still a machine contract, so the payload must \
             be git's own bytes for `git {}`.\n  raw  ({} B): {}\n  skim ({} B): {}",
            without.join(" "),
            raw.stdout.len(),
            show(&raw.stdout),
            skim.stdout.len(),
            show(&skim.stdout),
        );
    }
}

/// `git diff` + a contract flag + `--json` keeps git's bytes INSIDE the
/// envelope.
///
/// This is the evidence that settles the design question for the one
/// subcommand where it was genuinely open.  `git diff --stat` emits no
/// `diff --git` header, so `parse_diff` models nothing and the handler takes
/// its empty-parse branch — and that branch carries git's own `--stat` output
/// verbatim in a `raw` field.  So routing to the handler costs the caller
/// nothing: they receive the JSON they asked for *and* the contract bytes the
/// gate would have served, byte-for-byte, in one payload.
///
/// Had this field not existed, the honest answer for `diff` would have been
/// the other one — keep the gate armed and disclose the dropped `--json` —
/// because handing a stat payload to the AST pipeline and rendering `files: []`
/// with the bytes discarded is F6 rebuilt inside a JSON envelope.
#[test]
fn diff_contract_flag_with_json_carries_gits_own_bytes_in_the_envelope() {
    let (_dir, repo) = contract_repo();

    for flag in ["--stat", "--numstat", "--shortstat"] {
        let label = format!("diff {flag} --json");
        let skim = served(&repo, &["diff", flag, "--json"]);
        assert_eq!(
            skim.status.code(),
            Some(0),
            "`skim git {label}` must exit 0; stderr: {}",
            show(&skim.stderr)
        );

        let text = String::from_utf8(skim.stdout).expect("envelope must be UTF-8");
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("`skim git {label}`: stdout must be JSON ({e}): {text}"));

        let carried = parsed
            .get("raw")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("`skim git {label}`: envelope must carry `raw`: {text}"));

        let raw = raw_git(&repo, &["diff", flag]);
        let expected = String::from_utf8(raw.stdout).expect("git --stat output is UTF-8");
        assert_eq!(
            carried, expected,
            "`skim git {label}`: the `raw` field must carry git's own bytes for \
             `git diff {flag}` verbatim, or the contract was lost inside the envelope"
        );
    }
}

/// Where the `--json` route IS lossy, the loss is disclosed (ADR-011 class-1).
///
/// `git log --stat --json` is the case that makes the `--json` guard arguable:
/// `parse_log` renders one line per commit and drops the stat blocks, so the
/// envelope carries strictly less than git wrote.  That is not a silent
/// substitution — it is the disclosed path, and `ParsedCommandOptions` has no
/// `Default` precisely so no handler can emit an envelope without stating its
/// `Completeness`.  Asserting the marker is what distinguishes "the caller
/// opted into a summarised machine format" from the defect ADR-022 names,
/// which is loss with no marker at all.
///
/// Measured on `c2b4378`, which served this same envelope:
/// `271 lines omitted (2 of 273 shown)`.
#[test]
fn lossy_json_route_still_fires_its_class_one_marker() {
    let (_dir, repo) = contract_repo();
    let skim = served(&repo, &["log", "--stat", "-n", "2", "--json"]);

    assert_eq!(skim.status.code(), Some(0), "must exit 0");

    let err = String::from_utf8_lossy(&skim.stderr);
    assert!(
        err.contains("SKIM_PASSTHROUGH=1"),
        "a lossy `--json` view must carry its class-1 marker and remedy — the \
         marker is unconditional, and `served()` removes `SKIM_DEBUG`, so its \
         absence would mean the disclosure is gated;\n  stderr: {}",
        show(&skim.stderr)
    );
    assert!(
        err.contains("omitted") || err.contains("not the full tool output"),
        "the marker must state what was withheld;\n  stderr: {}",
        show(&skim.stderr)
    );
}
