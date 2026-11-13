# Skim Search — Vision

Tracking: #174 (master) — the active phase tracker and current scoreboard standing are linked there.

> **The best code search an AI agent can reach for.** It finds everything grep finds, puts the right code first, answers questions grep can't, and costs fewer tokens doing it. "Better than what exists today" is proven head-to-head, never asserted. Agents should choose it on merit, not because a hook forced them.

## Where search fits

Skim is one Rust binary that owns the three layers of an agent's context budget (#98):

| Layer | Job | In skim |
|---|---|---|
| **1. Retrieval** | Find the right code | `skim search` |
| 2. Representation | Shrink what you show | `skim <file>` modes, command compression |
| 3. Compression | Compress what the LLM reads | `rskim-proxy` |

Retrieval comes first: every token an agent spends reading the wrong file is wasted before the other two layers can help.

## Who it serves

AI coding agents first, humans second. Agents query with identifiers and short phrases, act on the top few results, and **cannot tell a silent miss from "not found."**

## What "better" means — in priority order

A lower rung never trades away a higher one.

1. **Never miss what grep finds.** Recall parity with `rg` on the same files. Table stakes an agent reaches for — case-insensitive, regex, path/type filters, file lists, true counts, a no-match exit code — work here too, so the agent never has to fall back. Every file that search cannot read (binary, not UTF-8, too large, unreadable, a link, outside the root) is counted with its reason. A silent false "not found" is the worst bug search can have.
2. **The right answer first.** grep returns matches in path order. Skim returns the definition first, with its line range, and then each call with the function that holds it. For plain words, the files that hold all the words come first, ranked by file kind.
3. **Answer questions grep can't.** Every matching line gets a role from the syntax: definition, call, import, string, comment or text. Each line is listed or counted, so one answer shows where a name is defined and where it is used. Other signals live in other tools: `skim heatmap` for git history (what is hot, what is risky, what changes together), and `ast-grep` for the shape of code.
4. **More signal per token.** Compact, ranked results. Every matching line is listed or counted. JSON on one line. Ultimately served in skim's compressed views (#208).
5. **Honest state.** There is no index to go stale: each query reads the files. When a bound fires or a file is left out, search says so with exact counts. It never guesses.

Performance targets (a p95 below 300 ms per query on the scoreboard corpora) matter, but they follow correctness: a fast wrong answer is worse than a slow right one.

## Principles

- **Local and zero-config.** One binary, no network, no Python, CLI flags only.
- **Text, syntax and history over embeddings.** Vector similarity scores ~2.6% F1 on structural code search vs ~70% for structural patterns (#173). Revisit only with evidence.
- **Measured, not asserted.** A green CI is not evidence of retrieval quality (SEARCH-ADR-007). The scoreboard is.

## The scoreboard

"Better" is a benchmark, not an adjective. A golden query set over real repositories (#203), run head-to-head against `rg` / `git grep` (text), with the answers for plain words frozen from a hand read, tracks:

| Metric | Bar |
|---|---|
| Recall vs `rg`, same file universe | 100% |
| Silent false negatives | 0 |
| Definition first, for a bare identifier | Always |
| Every site of a name, each with its role | Every frozen site found; every frozen call labelled `call` |
| A plain-word question | The frozen file is among the first 5 files |
| `rg`-style flags and exit codes | Equal to the regex oracle |
| Definition top-1, MRR, precision@k | Beat path-ordered grep output; never regress |
| Tokens to first correct answer | Fewer than `rg` |

### How it runs

The scoreboard lives in [`crates/rskim-bench/scoreboard/`](../crates/rskim-bench/scoreboard/README.md) (#203): an end-to-end harness that drives the release `skim` binary against four pinned, full-history corpora (skim, ripgrep, flask, zod) and checks every answer against independent in-process oracles: `rg -F` and regex semantics over the same file universe for text (no `rg` dependency), and answers frozen from `git grep -nw` and a hand read for names and their roles.

- **Required gate.** The `Search Scoreboard` CI job runs on every PR that touches search code; a search PR merges only with it green.
- **HARD checks** must pass per query: recall = precision = 1, zero silent misses, honest pagination, prefix consistency, score order, and the frozen answers (every site of a name, the definitions of a name that has two, the flags, the exit codes). **RATCHET** metrics (definition top-1, MRR, anchor = definition, P@k vs baselines, output bytes, work counters) cannot move in either direction without an explicit re-bless of `baseline.json`. Latency is informational.
- **Frozen answers (#540).** `[[refs]]`, `[[clash]]` and `[[stakes]]` entries hold answers that a person read from the pinned source before any skim run. The role oracle (`rskim-oracle`, tree-sitter queries written from the grammars) proposes entries for review. It is its own crate and depends on no `rskim-*` crate (a manifest test enforces it), so nothing from skim's search code can reach an answer.
- **Known failures are ledgered, never hidden.** Each entry in `known_failures.toml` names its ticket; fixing the bug turns the entry into an XPASS, which fails the gate until the entry is removed.
- **Manual adversarial dog-food (SEARCH-ADR-007)** remains only for what the scoreboard does not cover yet: the text layout of both views and of the notices, role precision outside the frozen names, the split of calls by definition, and any new query flag.

A change to retrieval or ranking merges only if the scoreboard does not regress.

## Non-goals

- Embedding / semantic vector search (see Principles).
- LSP features — type resolution, cross-reference by compiled symbol, refactoring. Roles come from syntax, not from types. Syntax evidence (imports, scope) can assign a call to one of several definitions in a later change. Type resolution stays out.
- Config files — modes and options stay CLI flags.
