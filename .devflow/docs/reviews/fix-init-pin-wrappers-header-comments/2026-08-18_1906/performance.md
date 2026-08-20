# Performance Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06

---

## Issues in Your Changes (BLOCKING)

### CRITICAL

**`is_module_header_comment()` backward sibling walk is quadratic on the default agent read path** — `crates/rskim-core/src/transform/minimal.rs:292-331` (loop at `308-330`)
**Confidence**: 90%

**Problem**

The new header-preservation predicate re-walks the entire preceding comment run *for every comment in that run*, turning a linear pass into a quadratic one.

```rust
// minimal.rs:308-330
let mut current = node;
loop {
    match current.prev_named_sibling() {
        None => return true,
        Some(prev) => {
            /* gap check */
            if is_comment_node(prev.kind(), language) { current = prev; } else { return false; }
        }
    }
}
```

Call-frequency analysis (answering the review question directly):

- It is **not** called per-node. `is_removable_comment` (`minimal.rs:148-157`) short-circuits on `is_comment_node(node.kind(), language)` first, so only comment nodes reach the preserve chain.
- Within the chain it is the **last** disjunct, so it runs only when `is_shebang` / `is_inside_function_body` / `is_doc_comment` all return false. For the four target languages `is_doc_comment` is a hardcoded `false` (`minimal.rs:176-178, 203-207, 216-223`), and `is_inside_function_body` returns true for anything inside a Python/Ruby `block`/`body_statement`. Net effect: **it runs for essentially every module-level comment** in a Python/Ruby/SQL/Bash file.
- The `is_root_child` guard at `minimal.rs:301` correctly bails O(1)-ish for non-root comments, so the blowup is confined to root-level comments — but that is exactly the set the header rule targets.

Cost of the walk itself:

- For a contiguous run of `L` root-level comments, comment at index `i` walks back `i` steps. Total steps = `L(L-1)/2` = **Θ(L²)**.
- Each step is a `prev_named_sibling()` FFI call. Verified against `tree-sitter-0.25.10/src/node.c`: `ts_node__prev_sibling()` calls `ts_node_parent()` (a descent from the tree root via `ts_node_child_with_descendant`) and then iterates the parent's children. Because `parser.c:1900-1920` balances long repeat chains via `ts_subtree_compress`, the hidden `_repeat1` structure is O(log L) deep — so each step is O(log L), not O(L). Realized cost: **Θ(L² log L)**.
- Baseline before this change was Θ(L log L) (the 1-2 `parent()` calls inside `is_inside_function_body`). **This change raises the exponent by one.**

Important: the walk runs to completion for *any* contiguous root-level comment run, not just the header. A commented-out block in the middle of a file costs the same Θ(L² log L) and then returns `false`.

**Failure scenario / impact**

Order-of-magnitude at ~50ns per `prev_named_sibling` step:

| Contiguous root-level comment lines | Backward steps | Approx. added time |
|---|---|---|
| 200 (large license + banner) | 20 K | ~1 ms — fine |
| 1 000 (commented-out module, `--` SQL preamble) | 500 K | **~25 ms** |
| 5 000 | 12.5 M | **~0.8 s** |
| 20 000 | 200 M | **~14 s** |
| 100 000 (at `MAX_AST_NODES`) | 5 × 10⁹ | effectively a hang |

Three things make this blocking rather than theoretical:

1. **It sits on the hot path an agent reads from.** `mode_for_files` (`crates/rskim/src/cmd/rewrite/handlers.rs:44-53`) selects `--mode=pseudo` for every regular code file, and `pseudo.rs:446` calls the same `is_removable_comment`. Every hook-rewritten `cat`/`head`/`tail` of a `.py` / `.rb` / `.sql` / `.sh` file pays this (`avoids PF-019` names this same hot path).
2. **It violates a stated MUST.** CLAUDE.md Design Constraints: "stay under 50ms for 1000-line files (benchmark regressions block)". A 1000-line all-comment `.py` or `.sh` file — a commented-out module, a generated `--` SQL preamble, a shell script with a long usage block — spends ~25 ms in this predicate alone, half the whole budget, before parse and transform.
3. **The existing complexity guard cannot catch it** (`applies ADR-002`). `MAX_AST_NODES = 100_000` (`minimal.rs:16`) is a *count of visited nodes*, incremented at `minimal.rs:86` **before** `is_removable_comment` runs at `minimal.rs:95`. It bounds how many nodes are visited, not the per-node work, so the process does ~5 × 10⁹ steps before the cap ever fires and `SkimError::ComplexityLimit` degrades to raw passthrough. ADR-002's rationale ("count caps never bounded parse cost… parse is bounded by `MAX_INPUT_SIZE`") held while per-node work was O(1); this change breaks that premise.

Also note the `loop` at `minimal.rs:309` has **no iteration cap**, unlike every other walk in this module family: `is_inside_function_body` uses `const MAX_PARENT_WALK: usize = 500` (`transform/utils.rs:43`), and `collect_removable_comments` uses `MAX_AST_DEPTH` / `MAX_AST_NODES`. This is also a direct violation of the project reliability rule ("All loops and retries must have a fixed upper bound").

**Fix**

Hoist the header computation out of the per-node predicate. The header block is a *prefix property of the file*, so it needs to be computed once, not re-derived per comment. A single forward pass with a `TreeCursor` is O(L) (cursor `goto_next_sibling` is amortized O(1), unlike `prev_named_sibling`):

```rust
/// Byte offset one past the end of the leading contiguous root-level comment run.
/// Computed once per file; `is_module_header_comment` then becomes an O(1) compare.
pub(crate) fn module_header_end_byte(root: Node, source: &str, language: Language) -> usize {
    match language {
        Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
        _ => return 0,
    }
    let mut cursor = root.walk();
    let mut end = 0usize;
    let mut prev_end: Option<usize> = None;
    for child in root.named_children(&mut cursor) {
        if !is_comment_node(child.kind(), language) {
            break;
        }
        if let Some(pe) = prev_end
            && source
                .get(pe..child.start_byte())
                .is_some_and(|g| g.bytes().filter(|&b| b == b'\n').count() > 1)
        {
            break; // blank-line break ends the header block
        }
        end = child.end_byte();
        prev_end = Some(end);
    }
    end
}

// minimal.rs:292 — now O(1), same semantics
fn is_module_header_comment(node: Node, header_end: usize) -> bool {
    node.end_byte() <= header_end
}
```

Thread `header_end` through the two walk contexts: compute it in `transform_minimal` (`minimal.rs:37-43`, store on `CommentWalkContext`) and in the pseudo entry point, then pass it into `is_removable_comment` at `minimal.rs:95` and `pseudo.rs:446`. That is a 4th parameter on a `pub(crate)` fn with exactly two call sites. Total cost becomes one O(L) forward pass per file, and the per-comment check is a single integer compare. Semantics are byte-identical: the current walk returns `true` only when it reaches `prev_named_sibling() == None`, i.e. only for members of the leading run.

**Stopgap (if the hoist is deferred):** add `const MAX_HEADER_WALK: usize = 500;` mirroring `MAX_PARENT_WALK` and bail to `false` past it. This bounds the worst case but is *not* semantics-preserving — a header longer than the bound would be stripped, regressing #476's intent — so it should be paired with the hoist, not substituted for it.

**Also add a guard test.** The five new unit tests (`minimal.rs:479-596`) are all 2-4 line fixtures and cannot observe this. CLAUDE.md states "benchmark regressions block", but no `.py`/`.sh`/`.sql` fixture in `tests/fixtures/` is comment-dense, so `cargo bench` will not catch it either. A 2000-line all-comment fixture with a wall-clock assertion (or a `criterion` case) is the durable guard.

---

## Issues in Code You Touched (Should Fix)

None at ≥80% confidence.

---

## Pre-existing Issues (Not Blocking)

**`is_go_doc_comment()` has the identical super-linear shape** — `crates/rskim-core/src/transform/minimal.rs:235-260`
**Confidence**: 85%
**Severity**: MEDIUM

- Problem: the same defect class as the CRITICAL above, in the sibling disjunct of the same `should_preserve` chain that this PR modified (`minimal.rs:152-155`). `is_go_doc_comment` walks *forward* via `next_named_sibling()` from every Go comment, and for a contiguous run of `L` root-level comments each member re-walks the remainder of the run → Θ(L² log L). It also allocates nothing and uses `.chars()` (`minimal.rs:244`) where `.bytes()` would do.
- Why it is lower severity than the new code: Go doc comments idiomatically sit in short runs (1-5 lines) directly above declarations, so `L` is small in practice, and the walk terminates at the first non-comment sibling.
- Not blocking — pre-existing, untouched by this diff. But the hoisting fix for `is_module_header_comment` generalizes to it directly (compute the contiguous-run boundaries once per file, index into them per comment), so it is cheap to fix in the same pass.

---

## Suggestions (Lower Confidence)

- **`is_hook_script_current()` re-derives what `pin_is_current()` already computed** — `crates/rskim/src/cmd/init/install.rs:868-877` (Confidence: 75%) — it re-reads the script, re-runs `parse_binary_pin_from_script`, and re-runs `resolve_skim_binary()` + `canonicalize()`, duplicating `DetectedState::pin_is_current()` (`state.rs:59-75`) which already has the pin parsed into `DetectedState.hook_binary_pin`. The syscall cost is negligible (see the startup-budget assessment below); the real concern is that two independent implementations of the same path comparison must now stay in sync — the exact "three sites, three normalization policies" hazard that `avoids PF-018` warns about and that `resolve_skim_binary()` was introduced to eliminate. Consider having `is_hook_script_current` take the already-resolved binary path as a parameter.

- **Newline count in the gap check does not early-exit** — `crates/rskim-core/src/transform/minimal.rs:319` (Confidence: 65%) — `gap.bytes().filter(|&b| b == b'\n').count() > 1` scans the whole gap even though the answer is known after the second newline. Gaps between adjacent comment lines are 1-2 bytes so this is near-free today, but `gap.bytes().filter(|&b| b == b'\n').take(2).count() > 1` is strictly cheaper and bounds the pathological large-gap case.

- **No performance fixture covers comment-dense input** — `crates/rskim-core/tests/`, `tests/fixtures/` (Confidence: 75%) — CLAUDE.md makes benchmark regressions blocking, but there is no `.py`/`.sh`/`.sql` fixture that is predominantly root-level comments, so neither the new unit tests nor `cargo bench` can observe the regression above or any future recurrence.

---

## Assessed and Cleared (no finding)

These were explicitly analyzed against the stated budgets and are **not** problems:

**`resolve_skim_binary()` canonicalize syscalls vs the <10 ms startup budget** — `crates/rskim/src/cmd/init/helpers.rs:26-33`. Not a concern.

- Invocation count per `skim init` (single agent): ~7 `current_exe()` + `canonicalize()` pairs — `detect_state` (`state.rs:121`), `DetectedState::pin_is_current` (`state.rs:65`, plus one `canonicalize` of the pinned path at `state.rs:71`), `is_hook_script_current` (`install.rs:869`, plus one at `install.rs:871`), `create_hook_script` (`install.rs:938`), `maybe_install_wrappers` (`install.rs:747`).
- Per `skim doctor`: `print_hook_section` iterates `AgentKind::all_supported()` (~6 agents) calling `hook_facts()`, each doing `detect_state` + `pin_is_current` → ~13-19 `canonicalize` calls total, plus at most one more per drifted agent in the pin-mismatch branch at `doctor/mod.rs:478-483`.
- Each `canonicalize` is a `realpath(3)` — tens of microseconds. Worst case here is well under 1 ms.
- **Critically, none of this is on the <10 ms startup path.** `init` and `doctor` are their own subcommands; a plain `skim <file>` read never enters them. The hook rewrite path is also untouched — `check_hook_binary_mismatch` in `rewrite/hook.rs` keeps its own pre-existing single `canonicalize(current_exe())`, and `hook_facts()` / `pin_is_current()` are called only from `init` and `doctor`. The `<10ms startup` and `<50ms/1000 lines` budgets are unaffected by the pinning work.

**Source slicing and allocation in the new transform code.** Clean. `source.get(gap_start..gap_end)` (`minimal.rs:318`) returns a zero-copy `&str`; `gap.bytes().filter(...).count()` allocates nothing. This satisfies the CLAUDE.md MUST "prefer `&str` slices over allocation in the hot path". The hot-path cost identified above is entirely tree-sitter sibling traversal, **not** slicing or allocation.

**`normalize_line_map_blanks` leading-blank fix** — `crates/rskim-core/src/transform/mod.rs:338-344`. The added `if result.is_empty() { continue; }` is an O(1) `Vec::is_empty` inside an existing single pass. No complexity change; the function stays O(lines). Correct fix for `avoids PF-019` with zero performance cost.

**`wrappers_blocks_fast_path`** — `crates/rskim/src/cmd/init/install.rs:169-175`. Pure match on an `Option<bool>`, no I/O. The `None → false` choice is also the performance-correct one: `Some(true)` would force a full reinstall (script write + SHA-256 hash + manifest write + settings patch) on every non-TTY `skim init`.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 1 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Performance Score**: 5/10

The binary-pinning half of this PR is performance-clean — the added `canonicalize()` calls are confined to `init`/`doctor` and cost well under a millisecond, and the line-map fix is free. The transform half introduces one genuine super-linear regression on the mode (`pseudo`) that the PreToolUse hook selects for every code-file read, in a place that neither the existing `MAX_AST_NODES` guard (`applies ADR-002`) nor the new unit tests nor `cargo bench` can observe.

**Recommendation**: CHANGES_REQUESTED

Blocking item is a single, well-scoped fix: hoist the header-block boundary to one forward pass per file and reduce `is_module_header_comment` to an integer compare. Everything else in this PR is either clean or a low-confidence suggestion.
