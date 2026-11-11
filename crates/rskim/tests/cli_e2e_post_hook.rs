//! E2E tests for the post-run hook: `skim rewrite --hook --agent claude-code`
//! fed a Claude Code `PostToolUse` input on stdin.
//!
//! The command already ran, so the hook must start no process. Every run here
//! has a private `PATH` of sentinel stubs (a stub writes a file when it runs,
//! and each rewrite rule needs its tool to resolve), a private cache dir, and
//! `SKIM_HOOK_AUDIT=1`. [`hook`] fails the test when a stub ran, when the hook
//! wrote anything to the cache dir (marker, sidecar, audit log, hook.log), or
//! when it exited non-zero or printed to stderr.

#![cfg(unix)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::LazyLock;

use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

// ---- Harness ----

/// Every program a test names. Each is a sentinel stub, so its rewrite rule resolves.
const PROGRAMS: &str = "cargo pytest python3 eslint npx ruff mypy golangci-lint rubocop oxlint \
                        biome swiftlint cat vitest npm tsc next frobnicate";

/// JSON pointer of the captured stdout.
const STDOUT: &str = "/tool_response/stdout";

/// Keys that decide, continue, stop or message: the response holds none of them.
const FORBIDDEN_KEYS: &str = "decision continue systemMessage permissionDecision \
                              permissionDecisionReason updatedInput stopReason suppressOutput";

struct Stubs {
    _dir: TempDir,
    ran: PathBuf,
    path: String,
}

/// A stub that appends its own path to `ran` when it runs.
fn write_sentinel(dir: &Path, name: &str, ran: &Path) {
    let script = format!("#!/bin/sh\necho \"$0\" >> '{}'\n", ran.display());
    common::write_stub_script(dir, name, &script);
}

static STUBS: LazyLock<Stubs> = LazyLock::new(|| {
    let dir = tempfile::tempdir().expect("stub dir");
    let ran = dir.path().join("RAN");
    for program in PROGRAMS.split_whitespace() {
        write_sentinel(dir.path(), program, &ran);
    }
    Stubs {
        path: dir.path().display().to_string(),
        _dir: dir,
        ran,
    }
});

/// Start `skim rewrite --hook --agent <agent>` with piped stdio, a private
/// `PATH`, and `cache` as both home and cache dir.
fn spawn(agent: &str, cache: &Path) -> Child {
    spawn_with(agent, cache, &[])
}

/// [`spawn`] with extra environment variables.
fn spawn_with(agent: &str, cache: &Path, envs: &[(&str, &str)]) -> Child {
    let mut command = Command::new(common::skim_bin());
    command
        .args(["rewrite", "--hook", "--agent", agent])
        .env_clear()
        .envs(envs.iter().copied())
        .env("PATH", &STUBS.path)
        .env("HOME", cache)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_HOOK_AUDIT", "1")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.spawn().expect("spawn skim")
}

/// Run the hook with `stdin`. The hook may exit before it reads all of an
/// oversized input, so a broken pipe is ignored.
fn run(agent: &str, cache: &Path, stdin: impl AsRef<[u8]>) -> Output {
    let mut child = spawn(agent, cache);
    let _ = child.stdin.take().expect("stdin").write_all(stdin.as_ref());
    child.wait_with_output().expect("wait for skim")
}

/// Run the hook as Claude Code in a fresh cache dir; it must exit 0 and print
/// nothing to stderr.
fn run_quiet(stdin: impl AsRef<[u8]>) -> Output {
    let cache = tempfile::tempdir().expect("cache dir");
    let output = run("claude-code", cache.path(), stdin);
    assert_quiet(&output);
    output
}

fn assert_quiet(output: &Output) {
    assert!(output.status.success(), "status {:?}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

/// Send `input` to the hook as `agent` and return its response, or `None` for
/// no output. Holds every property all post runs share.
fn hook(agent: &str, input: &Value) -> Option<Value> {
    let cache = tempfile::tempdir().expect("cache dir");
    let output = run(agent, cache.path(), input.to_string());
    assert_quiet(&output);
    assert!(!STUBS.ran.exists(), "a stub ran");
    let written: Vec<_> = fs::read_dir(cache.path()).unwrap().flatten().collect();
    assert!(written.is_empty(), "the post path wrote {written:?}");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    if stdout.is_empty() {
        return None;
    }
    assert_eq!(stdout.matches('\n').count(), 1, "one JSON line: {stdout:?}");
    Some(serde_json::from_str(&stdout).expect("JSON response"))
}

fn post(input: &Value) -> Option<Value> {
    hook("claude-code", input)
}

// ---- Inputs ----

fn fixture(kind: &str) -> Value {
    let name = format!("tests/fixtures/hooks/post_tool_use_{kind}.json");
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name));
    serde_json::from_str(&text.expect("fixture")).expect("fixture JSON")
}

/// A passing `cargo test` run in auto mode: the input every gate test changes.
fn baseline() -> Value {
    fixture("plain")
}

/// `input` with `pointer` set to `value`, or removed when `value` is `None`.
fn changed(input: &Value, pointer: &str, value: Option<Value>) -> Value {
    let mut input = input.clone();
    let (parent, key) = pointer.rsplit_once('/').expect("pointer");
    let parent = input.pointer_mut(parent).expect("parent");
    match value {
        Some(value) => parent[key] = value,
        None => drop(parent.as_object_mut().expect("object").remove(key)),
    }
    input
}

/// The baseline with another command and stdout, and no stderr.
fn call(command: &str, stdout: &str) -> Value {
    let input = changed(&baseline(), "/tool_input/command", Some(json!(command)));
    let input = changed(&input, STDOUT, Some(json!(stdout)));
    changed(&input, "/tool_response/stderr", Some(json!("")))
}

fn stdout_of(input: &Value) -> &str {
    input["tool_response"]["stdout"].as_str().expect("stdout")
}

/// Assert that the baseline with `pointer` set to `value` (or removed) gives no output.
fn assert_declined(pointer: &str, value: Option<Value>) {
    let input = changed(&baseline(), pointer, value.clone());
    assert!(post(&input).is_none(), "{pointer} = {value:?}");
}

// ---- Realistic default output of each tool ----

/// An input, and the strings its view must keep.
type Case = (Value, Vec<String>);

fn cargo_case() -> Case {
    (baseline(), vec!["40".into()])
}

fn pytest_case() -> Case {
    let stdout = "============================= test session starts ==============================\n\
        platform linux -- Python 3.12.3, pytest-8.3.2, pluggy-1.5.0\n\
        rootdir: /work/project\nconfigfile: pyproject.toml\ncollected 26 items\n\n\
        tests/test_basic.py ..........................                           [100%]\n\n\
        ============================== 26 passed in 0.01s ==============================\n";
    (call("pytest", stdout), vec!["26".into()])
}

/// The source of a code file: twelve functions, each after a long non-doc
/// comment. `function` is one function with `{n}` for its number.
fn source(comment: &str, function: &str) -> String {
    (0..12)
        .map(|n| {
            let function = function.replace("{n}", &n.to_string());
            format!("{comment} Explain step {n} in some detail for the reader.\n{function}\n")
        })
        .collect()
}

fn rust_source() -> String {
    source("//", "fn step_{n}() -> u32 { {n} }")
}

fn cat_case(path: &str, comment: &str, function: &str) -> Case {
    let input = call(&format!("cat {path}"), &source(comment, function));
    (input, vec!["step_3".into()])
}

/// A lint run with nine diagnostics over three files (`files` has `{n}` for the
/// file number), written from `[header, per-file head, per-diagnostic text,
/// footer]`; `{f}`, `{l}` and `{p}` (the line before) stand for the file and
/// line. The view must keep the location of every diagnostic.
fn lint(cmd: &str, files: &str, [header, file_head, diagnostic, footer]: [&str; 4]) -> Case {
    let mut spots = Vec::new();
    for n in 0..9u32 {
        spots.push((files.replace("{n}", &(n % 3).to_string()), 10 + 7 * n));
    }
    spots.sort();
    let mut out = header.to_string();
    let mut current = "";
    for (file, line) in &spots {
        if file != current {
            out += &file_head.replace("{f}", file);
            current = file;
        }
        let text = diagnostic
            .replace("{f}", file)
            .replace("{l}", &line.to_string());
        out += &text.replace("{p}", &(line - 1).to_string());
    }
    let kept = spots.iter().map(|(f, l)| format!("{f}:{l}")).collect();
    (call(cmd, &(out + footer)), kept)
}

// ---- TP-1, TP-2: compression and the response shape ----

/// The response is exactly `hookSpecificOutput.{hookEventName, updatedToolOutput}`.
/// `updatedToolOutput` is the input `tool_response` with a smaller `stdout` that
/// keeps every string in `kept`, and one skim marker line added to `stderr`.
fn assert_compresses((input, kept): Case) {
    let command = input["tool_input"]["command"].as_str().unwrap();
    let response = post(&input).unwrap_or_else(|| panic!("{command:?} not compressed"));
    let after = &response["hookSpecificOutput"]["updatedToolOutput"];
    let shape =
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": after}});
    assert_eq!(response, shape, "{command:?}");
    let before = &input["tool_response"];
    let mut copied = before.clone();
    copied["stdout"] = after["stdout"].clone();
    copied["stderr"] = after["stderr"].clone();
    assert_eq!(*after, copied, "{command:?}: every other key is copied");
    let (raw, served) = (stdout_of(&input), after["stdout"].as_str().unwrap());
    assert!(served.len() < raw.len(), "{command:?}: {served:?}");
    for text in &kept {
        assert!(served.contains(text), "{command:?} lost {text:?}");
    }
    let old = before["stderr"].as_str().unwrap();
    let new = after["stderr"].as_str().unwrap();
    assert!(new.starts_with(old), "{command:?}: {new:?}");
    let marker = new[old.len()..].trim();
    assert!(marker.starts_with("[skim] "), "{command:?}: {new:?}");
    assert!(!marker.contains('\n'), "{command:?}: one marker line");
}

macro_rules! compresses {
    ($($name:ident => $case:expr,)*) => { $(#[test] fn $name() { assert_compresses($case); })* };
}

compresses! {
    compresses_cargo_test => cargo_case(),
    compresses_pytest => pytest_case(),
    compresses_eslint_warnings => lint("eslint src/", "/work/src/mod_{n}.ts", [
        "", "\n{f}\n",
        "  {l}:5  warning  'value' is assigned a value but never used  @typescript-eslint/no-unused-vars\n",
        "\n✖ 9 problems (0 errors, 9 warnings)\n"]),
    // A project that sets `output-format = "concise"` in its ruff config gets this default output.
    compresses_ruff_check => lint("ruff check src/", "src/mod_{n}.py", [
        "", "", "{f}:{l}:8: F401 [*] `os` imported but unused\n",
        "Found 9 errors.\n[*] 9 fixable with the `--fix` option.\n"]),
    compresses_mypy => lint("mypy src/", "src/mod_{n}.py", [
        "", "", "{f}:{l}: error: Incompatible return value type (got \"str\", expected \"int\")  [return-value]\n",
        "Found 9 errors in 3 files (checked 5 source files)\n"]),
    compresses_golangci_lint => lint("golangci-lint run ./...", "cmd/mod_{n}.go", [
        "", "", "{f}:{l}:2: Error return value of `os.Remove` is not checked (errcheck)\n\tos.Remove(name)\n\t         ^\n", ""]),
    compresses_rubocop => lint("rubocop app/", "app/mod_{n}.rb", [
        "Inspecting 5 files\n.WWWW\n\nOffenses:\n\n", "",
        "{f}:{l}:5: W: Lint/UselessAssignment: Useless assignment to variable - `total`.\n    total = compute\n    ^^^^^\n",
        "\n5 files inspected, 9 offenses detected\n"]),
    compresses_oxlint => lint("oxlint src/", "src/mod_{n}.ts", ["", "",
        "  ⚠ eslint(no-unused-vars): Variable 'value' is declared but never used.\n   ╭─[{f}:{l}:7]\n{p} │ \n\
         {l} │ const value = compute();\n   ·       ──┬──\n   ·         ╰── 'value' is declared here\n   ╰────\n  \
         help: Consider removing this declaration.\n\n",
        "Found 9 warnings and 0 errors.\nFinished in 3ms on 3 files with 93 rules using 8 threads.\n"]),
    compresses_biome_check => lint("biome check src/", "src/mod_{n}.ts", ["", "",
        "{f}:{l}:7 lint/correctness/noUnusedVariables  FIXABLE  ━━━━━━━━━━━━━━━━━━━━━━\n\n  ⚠ This variable value is unused.\n  \n  \
         > {l} │ const value = compute();\n      │       ^^^^^\n  \n  ℹ Unused variables are often the result of typos.\n  \n",
        "Checked 3 files in 4ms. No fixes applied.\nFound 9 warnings.\n"]),
    compresses_swiftlint => lint("swiftlint lint", "/work/Sources/mod_{n}.swift", ["", "",
        "{f}:{l}:1: warning: Line Length Violation: Line should be 120 characters or less; currently it is 135 characters (line_length)\n",
        "Done linting! Found 9 violations, 0 serious in 3 files.\n"]),
    compresses_cat_of_a_rust_file => cat_case("src/never_created/lib.rs", "//", "fn step_{n}() -> u32 { {n} }"),
    compresses_cat_of_a_typescript_file => cat_case("src/never_created/app.ts", "//", "export function step_{n}(): number { return {n}; }"),
    compresses_cat_of_a_python_file => cat_case("src/never_created/tool.py", "#", "def step_{n}():\n    return {n}"),
}

#[test]
fn response_holds_no_permission_decision_at_any_level() {
    let text = post(&baseline()).expect("compresses").to_string();
    for key in FORBIDDEN_KEYS.split_whitespace() {
        assert!(!text.contains(&format!("\"{key}\"")), "{key}: {text}");
    }
}

/// Measured: each of these gives no output, because the view does not save more
/// than its own marker costs, or the parser does not read the output.
#[test]
fn output_skim_cannot_shrink_gives_no_output() {
    let ruff_default = "F401 [*] `os` imported but unused\n --> src/a.py:1:8\n  |\n1 | import os\n  |        ^^\n  |\nhelp: Remove unused import: `os`\n\n".repeat(9);
    #[rustfmt::skip]
    let runs = [
        ("ruff check src/", "All checks passed!\n"),
        ("mypy src/", "Success: no issues found in 5 source files\n"),
        ("rubocop app/", "Inspecting 5 files\n.....\n\n5 files inspected, no offenses detected\n"),
        ("swiftlint lint", "Done linting! Found 0 violations, 0 serious in 3 files.\n"),
        ("biome check src/", "Checked 3 files in 4ms. No fixes applied.\n"),
        ("oxlint src/", "Found 0 warnings and 0 errors.\nFinished in 3ms on 3 files with 93 rules using 8 threads.\n"),
        ("golangci-lint run ./...", "\n"),
        ("eslint src/", ""),
        // A declaration file is all signal: the structure view is the same text.
        ("cat types/index.d.ts", "/** A function. */\nexport declare function step(value: number): number;\n"),
        // The default output format of ruff 0.15 puts the location on its own line.
        ("ruff check src/", ruff_default.as_str()),
    ];
    for (command, stdout) in runs {
        assert!(post(&call(command, stdout)).is_none(), "{command:?}");
    }
}

// ---- TP-3: which calls reach the hook ----

#[test]
fn only_a_claude_auto_mode_bash_post_event_gets_output() {
    assert!(post(&baseline()).is_some(), "the baseline compresses");
    for mode in ["default", "plan", "acceptEdits", "bypassPermissions"] {
        assert_declined("/permission_mode", Some(json!(mode)));
    }
    assert_declined("/permission_mode", Some(json!(7)));
    assert_declined("/permission_mode", None);
    assert_declined("/tool_name", Some(json!("Read")));
    for event in ["PostToolUseFailure", "Stop"] {
        assert_declined("/hook_event_name", Some(json!(event)));
    }
    for agent in ["cursor", "gemini", "copilot", "codex"] {
        assert!(hook(agent, &baseline()).is_none(), "{agent}");
    }
}

// ---- TP-4: every skip gate, one field at a time from the baseline ----

#[test]
fn each_skip_gate_gives_no_output() {
    let raw = stdout_of(&baseline()).to_string();
    let padded = |len: usize| json!(format!("{raw}{}", " ".repeat(len - raw.len())));
    let input = changed(&baseline(), STDOUT, Some(padded(28_999)));
    assert!(post(&input).is_some(), "28,999 characters compress");
    #[rustfmt::skip]
    let set = [
        (STDOUT, padded(29_000)), (STDOUT, json!(7)),
        (STDOUT, json!(" \n\t")), (STDOUT, json!(format!("\0{raw}"))),
        (STDOUT, json!(format!("\u{FFFD}{raw}"))),
        (STDOUT, json!(format!("[skim] compressed view of 'x'\n{raw}"))),
        (STDOUT, json!(format!("[skim] transformed view (cat{raw}"))),
        ("/tool_response/stderr", json!("[skim] compressed view of 'x'")), ("/tool_response/stderr", json!(null)),
        ("/tool_response/interrupted", json!(true)), ("/tool_response/isImage", json!(true)),
        ("/tool_response/persistedOutputPath", json!("/tmp/skim-fixture/out.txt")),
        ("/tool_response/backgroundTaskId", json!("b0000000a")),
        ("/tool_response/returnCodeInterpretation", json!("note")),
        ("/tool_response", json!("text")), ("/tool_input/command", json!(7)),
    ];
    for (pointer, value) in set {
        assert_declined(pointer, Some(value));
    }
    #[rustfmt::skip]
    let removed = [STDOUT, "/tool_response/interrupted", "/tool_response/isImage"];
    for pointer in removed {
        assert_declined(pointer, None);
    }
    #[rustfmt::skip]
    let commands = [
        "cargo test && cargo build", "cargo test || true", "cargo test; ls", "cargo test | tee out.txt",
        "cargo test &", "cargo test > out.txt", "cargo test 2>&1", "cargo test < in.txt", "cargo test << EOF",
        "cargo test $(date)", "cargo test `date`", "cargo test\nls", "FOO=1 cargo test",
        "SKIM_PASSTHROUGH=1 cargo test", "cargo test SKIM_PASSTHROUGH", "skim cargo test",
        "env", "printenv HOME", "curl https://example.com", "git status", "ls",
    ];
    for command in commands {
        assert_declined("/tool_input/command", Some(json!(command)));
    }
}

#[test]
fn recorded_inputs_compress_only_when_they_are_a_plain_foreground_run() {
    assert!(post(&fixture("plain")).is_some());
    assert!(post(&fixture("persisted")).is_none());
    assert!(post(&fixture("background")).is_none());
}

// ---- TP-5: no process starts ----

/// [`hook`] checks every run for a sentinel run, and the TP-1 cases cover each
/// allow-listed command. These commands are long-running or unknown: the hook
/// declines them and still starts nothing.
#[test]
fn long_running_and_unknown_commands_start_nothing() {
    let stdout = stdout_of(&baseline()).to_string();
    #[rustfmt::skip]
    let commands = ["vitest", "npx vitest", "npm run dev", "tsc --watch", "next dev", "frobnicate --now"];
    for command in commands {
        assert!(post(&call(command, &stdout)).is_none(), "{command:?}");
    }
}

#[test]
fn a_sentinel_stub_records_its_own_run() {
    let dir = tempfile::tempdir().expect("dir");
    let ran = dir.path().join("RAN");
    write_sentinel(dir.path(), "tool", &ran);
    let stub = dir.path().join("tool");
    Command::new(&stub).status().expect("run stub");
    assert!(ran.exists());
}

// ---- TP-6: bad input and hostile input exit quietly ----

#[test]
fn bad_empty_truncated_and_oversized_input_exit_quietly() {
    let good = baseline().to_string();
    let oversized = changed(&baseline(), STDOUT, Some(json!("x".repeat(1 << 20))));
    let inputs: [(&str, Vec<u8>); 5] = [
        ("not JSON", b"not json".to_vec()),
        ("empty", Vec::new()),
        ("truncated", good.as_bytes()[..good.len() / 2].to_vec()),
        ("not UTF-8", vec![0xff, 0xfe, b'{']),
        ("over 1 MiB", oversized.to_string().into_bytes()),
    ];
    for (name, input) in inputs {
        assert!(run_quiet(&input).stdout.is_empty(), "{name}");
    }
}

#[test]
fn a_closed_stdout_exits_quietly() {
    assert!(post(&baseline()).is_some(), "the input compresses");
    let cache = tempfile::tempdir().expect("cache dir");
    let mut child = spawn("claude-code", cache.path());
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().expect("stdin");
    let input = baseline().to_string();
    stdin.write_all(input.as_bytes()).expect("write");
    drop(stdin);
    assert_quiet(&child.wait_with_output().expect("wait"));
}

#[test]
fn deeply_nested_code_exits_quietly_with_no_output() {
    #[rustfmt::skip]
    let files = [("deep.rs", "fn f() ", "{", "}", ""), ("deep.ts", "function f() ", "{", "}", ""), ("deep.py", "x = ", "(", ")", "1")];
    for (path, head, open, close, core) in files {
        let (open, close) = (open.repeat(14_000), close.repeat(14_000));
        let text = format!("{head}{open}{core}{close}\n");
        assert!(text.len() < 29_000, "{path}: size gate");
        let input = call(&format!("cat {path}"), &text);
        assert!(post(&input).is_none(), "{path}");
    }
}

// ---- TP-7: cat transforms the given text and never opens the path ----

#[test]
fn cat_compresses_the_given_text_not_the_file_and_keeps_escape_bytes() {
    let dir = tempfile::tempdir().expect("dir");
    let locked = dir.path().join("locked.rs");
    fs::write(&locked, "fn on_disk() {}\n").expect("write");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let text = format!(
        "{}fn escape() -> &'static str {{ \"\u{1b}[31m\" }}\n",
        rust_source()
    );
    for path in ["no/such/dir/lib.rs", locked.to_str().expect("UTF-8 path")] {
        let response = post(&call(&format!("cat {path}"), &text)).expect("must compress");
        let after = &response["hookSpecificOutput"]["updatedToolOutput"];
        let served = after["stdout"].as_str().unwrap();
        assert!(served.contains("step_3"), "{path}");
        assert!(served.contains('\u{1b}'), "{path}: the escape byte is kept");
        assert!(!served.contains("on_disk"), "{path}: the file is not read");
        // The first comment is the module header, which pseudo mode keeps.
        assert!(
            !served.contains("Explain step 5"),
            "{path}: comments are gone"
        );
    }
    assert!(post(&call("cat a.rs b.rs", &rust_source())).is_none());
}

// ---- TP-9: the pre-run path is unchanged ----

#[test]
fn a_pre_run_input_still_rewrites_and_writes_to_the_cache_dir() {
    let cache = tempfile::tempdir().expect("cache dir");
    let pre = json!({ "tool_input": { "command": "cargo test" } });
    let output = run("claude-code", cache.path(), pre.to_string());
    assert_quiet(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("skim cargo test"));
    // `hook` asserts that a post run leaves the cache dir empty: this run shows it can fill.
    assert!(fs::read_dir(cache.path()).unwrap().next().is_some());
}

#[test]
fn a_pre_run_input_over_64_kib_still_passes_through() {
    let pre = |pad: usize| json!({ "tool_input": { "command": "cargo test", "description": "x".repeat(pad) } });
    assert!(!run_quiet(pre(0).to_string()).stdout.is_empty());
    assert!(run_quiet(pre(70_000).to_string()).stdout.is_empty());
}

// ---- A drift finding reaches hook.log on a pre-run or bad input, never on a post-run input ----

/// Run the hook as a drifted install (a hook version that is not the binary's)
/// and return the hook.log it wrote, or `None` when it wrote none.
fn drift_log(stdin: &str) -> Option<String> {
    let cache = tempfile::tempdir().expect("cache dir");
    let mut child = spawn_with(
        "claude-code",
        cache.path(),
        &[("SKIM_HOOK_VERSION", "0.0.0-fake")],
    );
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write");
    assert_quiet(&child.wait_with_output().expect("wait for skim"));
    fs::read_to_string(cache.path().join("hook.log")).ok()
}

#[test]
fn a_drift_finding_is_logged_for_pre_and_bad_inputs_but_not_for_a_post_input() {
    assert_eq!(drift_log(&baseline().to_string()), None, "post input");
    for (name, stdin) in [
        ("pre input", r#"{"tool_input":{"command":"cargo test"}}"#),
        ("bad JSON", "{"),
    ] {
        let log = drift_log(stdin).unwrap_or_else(|| panic!("{name}: no hook.log"));
        assert!(log.contains("0.0.0-fake"), "{name}: {log}");
    }
}
