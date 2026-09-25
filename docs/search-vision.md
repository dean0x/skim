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

1. **Never miss what grep finds.** Recall parity with `rg` on the same files. Table stakes an agent reaches for — case-insensitive, regex, path/type filters, file lists, true counts, a no-match exit code — work here too, so the agent never has to fall back. Anything search can't see (unindexed files, degraded state) is disclosed. A silent false "not found" is the worst bug search can have.
2. **The right answer first.** grep returns matches in path order; skim returns them in usefulness order — the definition, the most relevant implementation, at the right line, at the top.
3. **Answer questions grep can't.** Three signals, fused into one ranked answer — no single existing tool combines them (#173):
   - **Lexical** — trigram index + field-weighted BM25F (definitions outweigh mentions), exact phrase and proximity.
   - **Structural** — an AST n-gram index: find code by its shape, not just its text.
   - **Temporal** — git history as a search dimension: what's hot, what's risky, what changes together.
4. **More signal per token.** Compact, ranked, JSON-first results; ultimately served in skim's compressed views (#208).
5. **Honest state.** A self-healing local index. When a signal is unavailable, say so and degrade — never guess.

Performance targets (sub-50 ms queries, fast incremental updates) matter, but they follow correctness: a fast wrong answer is worse than a slow right one.

## Principles

- **Local and zero-config.** One binary, no network, no Python, CLI flags only.
- **Structure and history over embeddings.** Vector similarity scores ~2.6% F1 on structural code search vs ~70% for structural patterns (#173). Revisit only with evidence.
- **Measured, not asserted.** A green CI is not evidence of retrieval quality (ADR-007). The scoreboard is.

## The scoreboard

"Better" is a benchmark, not an adjective. A golden query set over real repositories (#203), run head-to-head against `rg` / `git grep` (text), `ast-grep` (structure), and `git log` (history), tracks:

| Metric | Bar |
|---|---|
| Recall vs `rg`, same file universe | 100% |
| Silent false negatives | 0 |
| Definition top-1, MRR, precision@k | Beat path-ordered grep output; never regress |
| Structural precision / recall | At least `ast-grep` on shared patterns |
| Temporal counts | Exact parity with `git log` |
| Co-change (blast radius) | Precision/recall on real PRs, trending up |
| Tokens to first correct answer | Fewer than `rg` |

A change to retrieval or ranking merges only if the scoreboard does not regress.

## Non-goals

- Embedding / semantic vector search (see Principles).
- LSP features — type resolution, cross-reference by compiled symbol, refactoring.
- Config files — modes and options stay CLI flags.
