# Empirical Verification: doctor/init Behavior

Branch: `fix/init-pin-wrappers-header-comments`
Binary: `target/debug/skim` (v2.11.0, commit f00e37a)
Tested: 2026-08-19
Sandbox: all HOME/CLAUDE_CONFIG_DIR/GEMINI_CONFIG_DIR/COPILOT_CONFIG_DIR/CODEX_HOME/CRUSH_CONFIG_DIR/SKIM_WRAPPERS_DIR/SKIM_CACHE_DIR redirected into scratchpad; SKIM_DISABLE_ANALYTICS=1

---

## CLAIM 1: `skim doctor` falsely reports DRIFT in a foreign repo

**Verdict: CONFIRMED**

### Test

Created a throwaway git repo (`git init`, one commit, content unrelated to skim). Ran `skim doctor` inside it with the full sandbox env. Then ran `skim doctor` inside the skim repo itself.

### Foreign repo output (decisive lines)

```
Running binary
  /Users/dean/Sandbox/skim-issues/target/debug/skim (v2.11.0, commit f00e37a)

Staleness  (binary vs. repo HEAD)
  ✗  SHA f00e37a not found in this repo — built from a different repository

Status: DRIFT DETECTED — exit 1
```

Exit code: **1**

### Skim repo output (staleness section only)

```
Staleness  (binary vs. repo HEAD)
  ✓  up to date  (commit f00e37a is HEAD)

Status: DRIFT DETECTED — exit 1
```

Exit code: **1** (but for a different reason — PATH drift from other clones on this machine, not the staleness check)

### Analysis

The staleness check in `doctor/mod.rs:704-711` runs `git cat-file -e <sha>^{commit}` in the **current working directory's** git repo. Any binary built from the skim source will have its commit SHA embedded by `build.rs`. When an end user runs `skim doctor` inside their own project (which is not the skim source repo), that SHA cannot exist there, so the check always returns "built from a different repository" → DRIFT DETECTED → exit 1.

An end user whose binary was built from cargo-install or Homebrew will always see this false positive in every project they work in. The exact message shown is:

```
✗  SHA f00e37a not found in this repo — built from a different repository
```

with no remedy text — just the "DRIFT DETECTED — exit 1" status line. The user has no way to distinguish this false positive from a genuine version skew.

---

## CLAIM 2: `skim init --force` is silently a no-op

**Verdict: CONFIRMED**

### Test

1. First install: `skim init --yes --agent claude` — succeeded, created hook script.
2. Plain repeat (no `--force`): captured output and hook file mtime/md5.
3. Force run: `skim init --yes --force --agent claude` — captured output and hook file mtime/md5.

Sleep of 2 seconds was inserted before each run to ensure mtime would differ if the file was rewritten.

### Plain repeat run output

```
  + Hook: installed (v2.11.0)
  Already up to date. Nothing to do.
```
Exit code: 0. Hook mtime: unchanged. Hook md5: unchanged.

### Force run output

```
  + Hook: installed (v2.11.0)
  Already up to date. Nothing to do.
```
Exit code: 0. Hook mtime: **unchanged**. Hook md5: **unchanged**.

### Evidence

| Measurement | Before --force | After --force | Changed? |
|---|---|---|---|
| mtime | 1787143546 | 1787143546 | NO |
| md5 | 4edaaab7b002791019b4ce5edd9c5ecb | 4edaaab7b002791019b4ce5edd9c5ecb | NO |
| sha256 pin | sha256:3a2e41... | sha256:3a2e41... | NO |

Output is byte-for-byte identical to the plain repeat run. `--force` has zero observable effect on install behavior. Source confirms this: `flags.force` is only read in `uninstall.rs:156`; `install.rs` never reads it.

---

## CLAIM 3: `current_exe()` + `canonicalize()` resolves symlinks on macOS

**Verdict: CONFIRMED — symlink is resolved; no false-positive pin mismatch**

### Test

1. Copied debug binary to `real_binary_dir/skim`.
2. Created `symlink_dir/skim` → absolute path to `real_binary_dir/skim`.
3. Ran `skim init --yes --agent claude` **via the symlink path**.
4. Inspected the generated hook script for `SKIM_HOOK_BINARY` value.
5. Ran `skim doctor` via both the symlink path and the real path.

### Init output (binary identification line)

```
+ skim binary: .../real_binary_dir/skim (v2.11.0, commit f00e37a)
```

The init "Checking current state" banner resolves to the **real binary path**, not the symlink. The generated hook script records:

```sh
export SKIM_HOOK_BINARY='.../real_binary_dir/skim'
_SKIM_BIN='.../real_binary_dir/skim'
```

`SKIM_HOOK_BINARY` is set to the **resolved target**, not the symlink path.

### Doctor via real binary (hooks section)

```
✓ claude-code  installed (v2.11.0, commit f00e37a)  pin: .../real_binary_dir/skim
```

### Doctor via symlink

```
Running binary
  .../real_binary_dir/skim (v2.11.0, commit f00e37a)

✓ claude-code  installed (v2.11.0, commit f00e37a)  pin: .../real_binary_dir/skim
```

Both show the same "Running binary" path (resolved target). Both show a matching pin. **Neither reports a pin mismatch.** Exit code is 1 in both cases, but only due to the Claim 1 false positive (staleness check in a foreign repo) — not due to any pin mismatch.

### Implication for PF-018

The named landmine (pin gate false-positive on Homebrew/cargo-install symlinked layouts) does **not** occur via path-comparison alone. `current_exe()` on macOS resolves through symlinks before canonicalize runs, so the running-binary path and the pin-recorded path are always the same real path regardless of which symlink invoked the binary. A pin mismatch can only occur if the pinned binary is physically replaced (e.g., Homebrew upgrade) — which is a genuine staleness event, not a false positive.

---

## Summary Table

| Claim | Verdict | Key Evidence |
|---|---|---|
| 1: doctor falsely exits 1 in foreign repo | CONFIRMED | `✗  SHA f00e37a not found in this repo — built from a different repository` / exit 1 in throwaway repo |
| 2: `--force` is a no-op | CONFIRMED | Output identical to plain repeat; hook mtime/md5 unchanged; `install.rs` never reads `flags.force` |
| 3: symlink resolves; no false-positive pin mismatch | CONFIRMED | SKIM_HOOK_BINARY = resolved target; doctor via both paths shows matching pin; exit 1 only from Claim 1 issue |
