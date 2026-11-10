//! ADR-022 — `git show <rev>:<path>` blob extraction, end to end.
//!
//! `<rev>:<path>` is git's blob-extraction syntax and its contract is the
//! file's exact historical bytes.  Until ADR-022 that path ran `Mode::Pseudo`
//! unconditionally (`AD-GIT-SHOW-PSEUDO`, "Fix D"): 18 of 50 lines differed on
//! the reported case (`ok: boolean;` served as `ok`), stderr was empty even
//! under `SKIM_DEBUG=1`, and the escape the reporter suggested did not exist —
//! `--mode=full` reached the child git and returned `fatal: unrecognized
//! argument: --mode=full`, exit 1, zero stdout.
//!
//! # Why this file exists at all
//!
//! `run_show_file_content` writes straight to the process's stdout and returns
//! only an `ExitCode`, so **no in-file test at any layer can capture the served
//! bytes**.  `show.rs`'s own `#[cfg(test)]` module pins what is reachable from
//! inside the crate — the byte identity of the two git invocations, and the
//! view-selection decision as a pure function of the argv — and says so
//! explicitly.  What it cannot do is observe fd 1.  Spawning the binary is the
//! only layer that can, which is what each test below does.
//!
//! # Scope: valid UTF-8 only
//!
//! Byte-faithfulness holds for **valid-UTF-8 blobs**.  `CommandRunner::run`
//! performs a lossy UTF-8 conversion (`runner.rs:472/498/506`), so a blob
//! containing invalid UTF-8 has those bytes replaced with U+FFFD before
//! `run_show_file_content` ever sees them.  That is pre-existing, predates
//! ADR-022, and is filed separately — do not add a test here asserting
//! invalid-UTF-8 fidelity and do not read the assertions below as covering it.
//! [`BLOB_FIXTURE`] does carry a multi-byte character, so multi-byte *valid*
//! UTF-8 is in scope and is exercised.
//!
//! # Measurement discipline (PF-026)
//!
//! The control is pinned as hard as the subject.  Both sides run through
//! [`HERMETIC_GIT_ENV`], because `skim git …` spawns git as a child and that
//! child inherits skim's environment: pinning one side only would compare two
//! git invocations made under different configurations.  `SKIM_PASSTHROUGH` and
//! `SKIM_DEBUG` are removed from the subject — the first would make every
//! assertion here vacuously true, and the second would let a class-2 banner
//! satisfy an assertion that exists to pin an *unconditional* class-1 marker.
//!
//! # PF-025 / PF-027 / PF-031
//!
//! Under an ADR-001 raw passthrough stdout **is** the source file, so a naive
//! "stdout equals the blob" assertion passes green against a completely unfixed
//! binary.  Every test here therefore calls
//! [`assert_transform_is_observable`] *before* asserting anything about the
//! verbatim contract.  No constant or fixture size may be adjusted to make an
//! assertion pass.  Nothing reads this repository's history or pins a SHA: the
//! commit is built by the fixture and addressed as `HEAD`.

mod common;

// ============================================================================
// Hermetic fixture
// ============================================================================

/// Git's config search path, pinned to nothing (PF-009).
///
/// Repo-local `git config` writes cannot make a fixture hermetic on their own:
/// `git init` and every later git invocation still read `~/.gitconfig` and
/// `/etc/gitconfig`, and this file asserts on exact bytes.  `core.autocrlf`
/// would rewrite line endings, `color.ui = always` would inject ANSI escapes,
/// and a missing `user.email` would fail the commit outright.
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

/// Run a git setup step in `dir`, failing loud as a *setup* error.
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

/// The blob-extraction spelling under test — `HEAD:<path>`.
///
/// `HEAD` rather than a SHA: PF-031.  The commit is built by [`blob_repo`], so
/// nothing here depends on this repository's history, on CI's depth-1 checkout,
/// or on a branch-only SHA surviving a squash-merge.
///
/// The `.rs` extension is required, not cosmetic: `Language::from_path` must
/// resolve it, or `run_show_file_content` takes the Tier-2 unsupported-language
/// passthrough and serves raw for a reason that has nothing to do with ADR-022
/// — which would make the verbatim assertions pass for the wrong reason.
const BLOB_REFPATH: &str = "HEAD:src/registry.rs";

/// The `<path>` half of [`BLOB_REFPATH`], which is where [`blob_repo`] writes
/// the fixture.
///
/// Derived rather than declared as a second constant: the two spellings must
/// name the same file, and a pair of literals is a pair that can drift.
fn blob_path() -> &'static str {
    BLOB_REFPATH
        .split_once(':')
        .expect("BLOB_REFPATH must carry the `<rev>:<path>` separator")
        .1
}

/// The blob the tests commit and read back.
///
/// Four properties are load-bearing and none is incidental:
///
/// - **No trailing newline.**  A byte comparison would not notice an appended
///   `\n` on a file that already ends in one, and appending one is a live defect
///   class in this tree: `emit_raw_passthrough`'s guard does exactly that (F13),
///   which is why the verbatim path must keep using `exec::write_to_stdout`
///   with `ensure_trailing_newline: false`.
/// - **Genuinely lossy under pseudo.**  Measured against the read-only pinned
///   baseline `target/skim-baseline-c2b4378` on the uncharged git-show path:
///   **1046 B raw → 586 B** under `Mode::Pseudo`, i.e. 460 B of headroom over
///   the ADR-001 guard.  [`assert_transform_is_observable`] re-derives that at
///   run time rather than trusting this number.
/// - **The saving comes from MODULE-LEVEL non-doc comments.**  Measured on the
///   same baseline: pseudo removes non-doc comments *between items* and leaves
///   comments *inside a function body* alone.  A fixture whose only comment sits
///   in a body saves nothing, the guard serves it raw, and every assertion here
///   goes vacuous — which is exactly what happened to the committed fixture
///   `tests/fixtures/cmd/git/show_file.rs` (1916 B, one in-body comment, served
///   raw byte-identically at `c2b4378`).  `tests/fixtures/typescript/simple.ts`
///   (275 B) fails the precondition for the same reason.  Neither is usable here.
/// - **Rust, and free of the node kinds this batch touches.**  No
///   `function_signature_item`, no trailing-`;` `struct_item`, no
///   `associated_type` — the three kinds F1c adds bytes back to — and no
///   TypeScript at all, which is what F1/F1b touch.  So this headroom does not
///   move as the rest of the batch lands.  A TypeScript fixture's would:
///   `show.rs`'s in-crate `BLOB_FIXTURE` was measured at 442 B → 338 B (104 B
///   of headroom) *before* F1 restored four member annotations and F1b four
///   separator semicolons.
///
/// The first comment is the module header, which #476 preserves in every
/// language; the runs below it are removed.  Both are worded so a reader of the
/// fixture is not misled about which is which.  The header's em dash is
/// deliberate: it makes this a multi-byte valid-UTF-8 blob, so the byte
/// comparisons cover more than ASCII.
const BLOB_FIXTURE: &str = "\
// module header — preserved in every language (#476)
use std::collections::BTreeMap;

// A comment below the header run, which pseudo removes.  Four such lines, so
// the saving is a wide margin rather than a rounding error in the ADR-001
// guard, and so the fixture keeps clearing it if the marker is ever charged
// against the verdict the way the file-transform path already charges it.
pub struct Registry {
    entries: BTreeMap<String, u64>,
}

// A second run of header-level commentary, below the header, also removed.
// It is worded so a reader of the fixture cannot mistake it for the header.
impl Registry {
    /// A doc comment, which pseudo preserves — so its survival is not the
    /// lever any assertion in this file uses.
    pub fn insert(&mut self, key: String, hits: u64) {
        let slot = self.entries.entry(key).or_default();
        *slot = slot.saturating_add(hits);
    }

    /// Total hits recorded across every key.
    pub fn total_hits(&self) -> u64 {
        self.entries.values().copied().sum()
    }
}";

/// A hermetic repo holding [`BLOB_FIXTURE`] at [`blob_path`] in one commit.
///
/// PF-031: no SHA is pinned and no repository history is read.  The commit is
/// created here and addressed as `HEAD`, so the fixture is immune both to CI's
/// depth-1 checkout and to this repository's squash-merge policy.
fn blob_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().join("repo");
    let blob = repo.join(blob_path());
    std::fs::create_dir_all(blob.parent().expect("blob path must have a parent"))
        .expect("create blob parent dir");
    std::fs::write(&blob, BLOB_FIXTURE).expect("write blob fixture");

    // PF-009: `-b main` pins the initial branch from creation rather than
    // inheriting the host's `init.defaultBranch`.
    git_in(dir.path(), &["init", "-b", "main", repo.to_str().unwrap()]);
    git_in(&repo, &["config", "user.email", "test@t.invalid"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["config", "commit.gpgsign", "false"]);
    // The blob must reach the object database byte-for-byte as written.
    git_in(&repo, &["config", "core.autocrlf", "false"]);
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "--no-verify", "-m", "fixture"]);

    (dir, repo)
}

// ============================================================================
// Control and subject
// ============================================================================

/// The control: the narrowest way to ask git for a blob's contents.
///
/// `git cat-file blob <rev>:<path>` applies no textconv, no pager and no
/// decoration, so it is the blob's stored bytes and nothing else.  That makes it
/// the right baseline for a contract phrased as "the file's exact historical
/// bytes" — and a stronger one than `git show`, which is the invocation the
/// production path itself makes.
fn raw_blob(repo: &std::path::Path) -> Vec<u8> {
    let out = hermetic_git()
        .args(["cat-file", "blob", BLOB_REFPATH])
        .current_dir(repo)
        .output()
        .expect("raw control `git cat-file blob` must spawn");
    assert!(
        out.status.success(),
        "raw control `git cat-file blob {BLOB_REFPATH}` failed;\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// What `skim git …` actually serves for the same argv.
fn served(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut cmd = common::skim();
    // PF-026: the escape hatch would make every assertion below vacuous, and a
    // debug banner would satisfy a marker assertion that exists to pin an
    // unconditional one.
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
///
/// Used in place of slicing: `&s[..n]` panics on a multi-byte boundary, and
/// [`BLOB_FIXTURE`] is deliberately multi-byte.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).escape_debug().to_string()
}

/// The ADR-011 class-1 marker's stable prefix for a pseudo view.
///
/// Only the prefix and the remedy are asserted, never the whole line: the class
/// clause comes from `output::mode_class_label`, which names per-language
/// constructs and is expected to be reworded.  The prefix and the remedy are the
/// two halves ADR-011 class 1 actually obliges.
const PSEUDO_MARKER_PREFIX: &str = "[skim] pseudo view:";

/// The remedy a class-1 marker must print, per ADR-011.
const MARKER_REMEDY: &str = "SKIM_PASSTHROUGH=1";

// ============================================================================
// Precondition (PF-025 — mandatory, because this fix can fake its own success)
// ============================================================================

/// Prove that a lossy view of this blob is *reachable and served*, before any
/// test asserts that the default is verbatim.
///
/// # Why a precondition is mandatory here
///
/// The observable and the fallback coincide.  Under an ADR-001 raw passthrough,
/// an unsupported extension, or a transform error, stdout **is** the source
/// file — so "stdout equals the blob" is satisfied by a completely unfixed
/// binary, by a binary that regressed to always serving raw, and by a fixture
/// the transform happens to leave alone.  This function excludes all three, in
/// the only way stdout can: by showing that on *this* blob `--mode=pseudo`
/// serves something else.
///
/// # What each assertion excludes
///
/// 1. **Exit 0.**  At `c2b4378` `--mode=pseudo` leaked to the child git and died
///    with `fatal: unrecognized argument`, exit 1, zero stdout.  A test that
///    skipped this would read the empty stdout as "differs from raw" and pass.
/// 2. **stdout differs from the control.**  Jointly: the blob is a known-lossy
///    input, and the ADR-001 guard elected the transformed view rather than
///    falling back to raw.  Either failing would put the source file on stdout.
///
/// A failure of either means the fixture stopped exhibiting the defect and the
/// tests have gone vacuous.  Read it as "re-derive [`BLOB_FIXTURE`]", never as
/// "the guard now protects us" (PF-027: do not weaken the bound).
///
/// Returns the probe's `Output` so the disclosure test can assert on the same
/// invocation rather than running a second one.
fn assert_transform_is_observable(repo: &std::path::Path) -> std::process::Output {
    let raw = raw_blob(repo);
    let probe = served(repo, &["show", "--mode=pseudo", BLOB_REFPATH]);

    assert_eq!(
        probe.status.code(),
        Some(0),
        "PRECONDITION FAILED: `skim git show --mode=pseudo {BLOB_REFPATH}` did \
         not exit 0.  At c2b4378 the flag reached the child git and this was \
         exit 1 with `fatal: unrecognized argument: --mode=pseudo`.\n  \
         stdout: {}\n  stderr: {}",
        show(&probe.stdout),
        show(&probe.stderr),
    );
    assert_ne!(
        probe.stdout,
        raw,
        "PRECONDITION FAILED: `--mode=pseudo` served the blob's own bytes \
         ({} B), so either this fixture is no longer lossy under pseudo or the \
         ADR-001 guard elected raw.  Every verbatim assertion in this file \
         would then pass against an unfixed binary (PF-025).  Re-derive \
         BLOB_FIXTURE so pseudo saves more than the guard's margin; do not \
         weaken the assertions.\n  stderr: {}",
        raw.len(),
        show(&probe.stderr),
    );

    probe
}

// ============================================================================
// Facet 1 — the default is byte-faithful
// ============================================================================

/// `skim git show <rev>:<path>` serves the blob's exact bytes, and says nothing.
///
/// This is the decision reversal itself.  Before ADR-022 this invocation applied
/// `Mode::Pseudo` unconditionally, and the loss was silent on *both*
/// descriptors: there was no `lossy_view_marker` call anywhere under
/// `cmd/git/`, so a caller asking git for a blob's historical bytes received a
/// transformed view with no cue that a substitution had occurred — a loss
/// indistinguishable from the file having genuinely been different.
///
/// stderr is asserted empty, not merely marker-free.  A gated verbatim serve is
/// a *lossless* path, so every notice on it is ADR-011 class 2 and therefore
/// `SKIM_DEBUG`-gated; with `SKIM_DEBUG` removed by [`served`] the correct byte
/// count is zero.  Anything else is context tax on a path whose entire contract
/// is "give me the bytes".
#[test]
fn test_blob_default_is_byte_identical_to_git_cat_file_blob() {
    let (_dir, repo) = blob_repo();
    let raw = raw_blob(&repo);
    assert_transform_is_observable(&repo);

    let out = served(&repo, &["show", BLOB_REFPATH]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "`skim git show {BLOB_REFPATH}` must exit 0.\n  stderr: {}",
        show(&out.stderr),
    );
    assert_eq!(
        out.stdout,
        raw,
        "`<rev>:<path>` is git's blob-extraction syntax, so skim must deliver \
         the blob's own bytes (ADR-022).\n  raw  ({} B): {}\n  skim ({} B): {}",
        raw.len(),
        show(&raw),
        out.stdout.len(),
        show(&out.stdout),
    );
    assert!(
        out.stderr.is_empty(),
        "the verbatim path is lossless, so every notice on it is ADR-011 \
         class 2 and SKIM_DEBUG-gated; stderr must carry zero bytes.\n  \
         got ({} B): {}",
        out.stderr.len(),
        show(&out.stderr),
    );
}

// ============================================================================
// Facet 2 — `--mode=full` is parsed by skim, and serves the bytes
// ============================================================================

/// `--mode=full` exits 0 **and** serves the blob's bytes.
///
/// Both halves, deliberately.  At `c2b4378` this was
/// `fatal: unrecognized argument: --mode=full`, exit 1, zero stdout — the flag
/// reached the child git because nothing stripped it.  An "after" that checked
/// only "output equals raw" would also pass against a binary that merely learned
/// to *swallow* the flag: zero stdout equals zero stdout only if the control is
/// also empty, but a binary that stripped `--mode=full` and then served the
/// blob would satisfy the byte assertion while `--mode=pseudo` silently did
/// nothing either.  Pinning the exit code separately from the bytes is what
/// distinguishes "parsed and honoured" from "parsed away".
///
/// `full` reaching the *same* verbatim branch as the flagless default — rather
/// than being routed through `rskim_core::transform` with `Mode::Full` — is the
/// documented intent: `Mode::Full` means "no transformation", so serving the
/// blob honours it exactly, without making byte-faithfulness contingent on
/// tree-sitter parsing the blob or on the guardrail's tie rule.  It is also the
/// escape the original report asked for and did not have.
#[test]
fn test_blob_mode_full_exits_zero_and_serves_the_bytes() {
    let (_dir, repo) = blob_repo();
    let raw = raw_blob(&repo);
    assert_transform_is_observable(&repo);

    let out = served(&repo, &["show", "--mode=full", BLOB_REFPATH]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "`--mode=full` must be skim-owned and exit 0; at c2b4378 it reached the \
         child git and returned `fatal: unrecognized argument: --mode=full`.\n  \
         stdout: {}\n  stderr: {}",
        show(&out.stdout),
        show(&out.stderr),
    );
    assert_eq!(
        out.stdout,
        raw,
        "`--mode=full` means no transformation, so it must serve the blob's own \
         bytes.\n  raw  ({} B): {}\n  skim ({} B): {}",
        raw.len(),
        show(&raw),
        out.stdout.len(),
        show(&out.stdout),
    );
}

// ============================================================================
// Facet 3 — a lossy `--mode` discloses itself, unconditionally
// ============================================================================

/// A lossy `--mode` emits the ADR-011 class-1 marker with `SKIM_DEBUG` unset,
/// and the bytes it discloses really did move.
///
/// Two claims, and the second is what keeps the first honest.  A marker
/// asserted on its own passes against a binary that prints it on every
/// invocation — including the verbatim default, where it would be disclosing a
/// transform that never happened.  So stdout is required to differ from the
/// control on the lossy call (via [`assert_transform_is_observable`]) and the
/// marker is required to be *absent* from the verbatim default.
///
/// Unconditional is the load-bearing word.  [`served`] removes `SKIM_DEBUG`, so
/// a marker that had been filed as an ADR-011 class-2 banner — `debug_log!` into
/// `io::sink()` by default, which is how every no-loss notice under `cmd/git/`
/// is wired — would produce zero bytes here and fail.  At `c2b4378` the loss on
/// this path was silent even *with* `SKIM_DEBUG=1`, because no
/// `lossy_view_marker` call existed anywhere under `cmd/git/` at all.
#[test]
fn test_blob_lossy_mode_discloses_itself_unconditionally() {
    let (_dir, repo) = blob_repo();
    let raw = raw_blob(&repo);

    // Establishes exit 0 and that stdout genuinely differs from the blob, so
    // the marker asserted below is disclosing a transform that did happen.
    let lossy = assert_transform_is_observable(&repo);
    let stderr = String::from_utf8_lossy(&lossy.stderr);

    assert!(
        stderr.contains(PSEUDO_MARKER_PREFIX),
        "a lossy `--mode` must emit the ADR-011 class-1 marker on stderr with \
         SKIM_DEBUG unset; expected a line starting {PSEUDO_MARKER_PREFIX:?}.\n  \
         stderr ({} B): {}",
        lossy.stderr.len(),
        show(&lossy.stderr),
    );
    assert!(
        stderr.contains(MARKER_REMEDY),
        "an ADR-011 class-1 marker must name its remedy ({MARKER_REMEDY:?}), \
         which is what makes the disclosure actionable rather than merely \
         informative.\n  stderr: {}",
        show(&lossy.stderr),
    );

    // The complement: the marker must not fire where nothing was lost, or it
    // would be disclosing a transform that did not happen.
    let verbatim = served(&repo, &["show", BLOB_REFPATH]);
    assert_eq!(
        verbatim.stdout, raw,
        "fixture sanity: the default must still be the verbatim blob here"
    );
    assert!(
        !String::from_utf8_lossy(&verbatim.stderr).contains("view:"),
        "the verbatim default loses nothing, so it owes no class-1 marker; a \
         marker there would disclose a transform that never ran.\n  \
         stderr: {}",
        show(&verbatim.stderr),
    );
}
