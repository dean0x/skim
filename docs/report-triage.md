# Checking a skim issue report

Use this guide when a person or an agent reports a problem with skim.
It gives one verdict for each report, with evidence.
Do the checks in this order. The first checks are the cheapest.
Stop at the first check that gives a verdict.

## Verdicts

| Verdict | Meaning | Next action |
|---|---|---|
| Old install | The binary that ran is older than `main`, and `main` behaves differently. | Tell the reporter to update. Close the report. |
| Misread | skim did what its docs say. The reporter expected a different tool or view. | Answer with the doc that states the behaviour. If many readers make the same mistake, change the doc. |
| Intended | A doc, a decision record or a test states the behaviour, and the behaviour does not defeat a goal of skim. | Answer with the source. A request for a change is a feature request, not a bug. |
| Already handled | An open issue tracks it, or a commit on `main` fixes it. | Link the issue or the commit. Close the report as a duplicate. |
| Real fault | skim breaks its docs or one of its goals, and the fault occurs on the current `main` build. | Open or keep a bug. Give the minimal reproduction and the exact numbers. |

## 1. Old install

1. Get the version and commit that ran: `skim --version` (the form is `skim X.Y.Z (<commit>)`).
2. Run `skim doctor`. It lists every `skim` on `PATH`, the one that wins, and the binary that each agent hook uses. A hook can use a different binary than the shell.
3. Compare that commit with `main`: `git log --oneline <commit>..origin/main -- crates/`. If no code changed, the old binary is equal to `main` for this report.
4. Else build `main` and run the command again with that build, by its path (`./target/release/skim`). A bare `skim` can find a different binary.

Verdict: if the `main` build does not show the problem, it is **Old install**.

## 2. Misread

skim changes some commands before they run, and it changes some output after a run. A reporter can see a view where they expected the raw text.

1. Find the surface that acted: the rewrite hook (for example `cat file.rs` becomes `skim file.rs --mode=pseudo`), a PATH wrapper, or the after-run hook. The `[skim]` line on stderr names a view that differs from the raw output.
2. Read the mode docs (`docs/modes.md`) and the command help (`skim <command> --help`).
3. Run the raw tool with `SKIM_PASSTHROUGH=1` and compare it with the skim output.

Verdict: if the output agrees with the docs and with its stderr notice, it is **Misread**.

## 3. Intended

1. Search the docs (`README.md`, `docs/`), the design decisions and the rustdoc of the code path for the behaviour.
2. Find the test that pins the behaviour.
3. Ask one question: does the behaviour defeat a goal of skim? The goals are fewer tokens with no loss of meaning, no silent loss (an elision always has a marker with exact counts), the exit code of the wrapped tool, and a clear error message.

Verdict: if a source states the behaviour and no goal is defeated, it is **Intended**. If a goal is defeated, continue to step 5 even when a doc states the behaviour.

## 4. Already handled

1. Search open and closed issues for the command, the flag and the symptom.
2. Search `main` for a fix: `git log origin/main --oneline -S '<key text>'` and the `Unreleased` part of `CHANGELOG.md`.

Verdict: if an issue or a commit covers the same fault, it is **Already handled**. A fix on `main` that is not released yet is also Already handled.

## 5. Real fault

1. Reproduce the fault on the current `main` build, with the smallest input.
2. Get the ground truth from the raw tool, the raw file bytes, or the limit of the agent. Do not use a second skim command as the ground truth.
3. Write down the exact command, the exact skim output, the expected output and the numbers (bytes, lines, exit codes).
4. Measure the reach: is it the default setup or an edge case, and how many inputs show it?

Verdict: if the fault occurs on `main` against the ground truth, it is **Real fault**.

## Report form

Give the verdict, then one line of evidence for each check that you did:

```
Verdict: <one of the five>
1. Old install: <commit that ran> vs main <commit>: <same code | differs>
2. Misread: <surface and mode>, <doc that applies>
3. Intended: <source that states it, or none>; goal defeated: <yes/no, which>
4. Already handled: <issues or commits found, or none>
5. Real fault: <command> -> <numbers>; reach: <default setup | edge case>, <count>
```
