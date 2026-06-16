<!-- TL;DR: 6 decisions. Key: ADR-002, ADR-003, ADR-004, ADR-005, ADR-006 -->
# Architectural Decisions

Append-only. Status changes allowed; deletions prohibited.

## ADR-001: Fix all noticed issues immediately regardless of scope — 'if you see something, do something'

- **Date**: 2026-04-30
- **Status**: Accepted
- **Context**: reviewer surfaced 7 findings including 4 classified as 'follow-up tickets' non-blocking for merge
- **Decision**: resolve all findings immediately — no deferral to future tickets or tech debt backlog regardless of blocking severity
- **Consequences**: 'I really hate tech debt' and 'I don't want to leave anything for later' — user treats noticed issues as immediate obligations regardless of blocking status. **Why:** the cost of leaving a known issue is higher than the cost of extended scope. **How to apply:** when reviewers flag any issues (blocking OR non-blocking), always surface them all to the user for decision rather than silently classifying as deferred. The user will almost always choose to fix now.
- **Source**: self-learning:obs_jk7n2w

## ADR-002: Progress trackers and prioritized issue queues belong as GitHub issues, not memory files

- **Date**: 2026-06-03
- **Status**: Accepted
- **Context**: After completing issue triage and agreeing on the next 5 priorities, a progress tracker was created in WORKING-MEMORY.md. The user immediately rejected this, noting memory files don't age well.
- **Decision**: Any prioritized work queue, roadmap tracker, or multi-issue progress tracker must be created as a GitHub issue (e.g., issue #268 "Roadmap: Next 5 issues"), not in WORKING-MEMORY.md or any devflow memory file.
- **Consequences**: GitHub issues survive context compaction, are version-controlled, and are visible to all collaborators. Memory files are session-scoped and lose fidelity over time. **How to apply:** when the user asks to "track" multiple upcoming issues or create a work queue, default to `gh issue create` — not a memory file edit. WORKING-MEMORY.md should only reflect current session state (branch, latest commit, blockers), not a durable to-do list.
- **Source**: sidecar:decisions.2fea848d-a8f7-46da-aa16-be990ed4d829

## ADR-003: Replace empirically-baseless acceptance criteria with grounded regression guards rather than chasing impossible targets

- **Date**: 2026-06-04
- **Status**: Accepted
- **Context**: issue #194 acceptance criterion A16 demanded the AST index be < 5% of source bytes, but structural AST n-grams are dense by design (O(vocab x files) posting entries) so 5% is unreachable and the figure had no empirical origin in any prior wave research. A numeric criterion copied verbatim from an issue may be empirically baseless and structurally unachievable.
- **Decision**: before enforcing an inherited numeric acceptance criterion, trace it to a measured basis; if none exists, replace the target with a defensible grounded regression guard (here < 3x source-bytes, measured 1.23x) as a real non-ignored test, amend the issue, and file the follow-up work (on-disk compression filed as #273)
- **Consequences**: an impossible gate forces a blocked PR, an #[ignore] cop-out, or test-gaming — all of which erode trust in the suite; a regression guard grounded in measurement and industry norms (uncompressed code-search trigram indexes run 3-5x) protects against real bloat, whereas an impossible target either blocks the PR or gets silently ignored
- **Source**: self-learning:obs_a16x3g (absorbs retired PF-005)

## ADR-004: File follow-up and integration tickets up front as Step 0 (before any code), never post-merge, and never leave #NEW placeholder numbers in source

- **Date**: 2026-06-07
- **Status**: Accepted
- **Context**: implementation plans repeatedly deferred follow-up and integration ticket filing until after merge, and used #NEW placeholders in source for not-yet-filed issue numbers, creating a fix-it-later trap where the placeholder survives the PR and the real number is never wired in
- **Decision**: front-load all ticketing as an explicit Step 0 that runs before any code is written — file the follow-up ticket first so its real issue number can be hardcoded directly into code (e.g. into an error string), and annotate consuming/integration issues up front with the contracts they depend on
- **Consequences**: a placeholder in code is silent debt that is easily forgotten
- **Source**: self-learning:obs_tk3f9w

## ADR-005: Never auto-merge a green PR — hand off for the user to explicitly request the squash-merge once CI passes

- **Date**: 2026-06-07
- **Status**: Accepted
- **Context**: a feature PR (#284) reached green CI through the full implement/validate/scrutinize/align/QA pipeline and the agent had the standing capability to merge it, yet the agent declined to merge automatically
- **Decision**: the agent must not auto-merge even when CI is fully green — merge is a user-gated action
- **Consequences**: the user keeps the merge decision as a deliberate human gate so the final integration step is never taken without explicit human intent, even when every automated check has passed. How to apply: after CI goes green, report the verdict and stop — do not run gh pr merge or any merge command until the user explicitly asks for the squash-merge.
- **Source**: self-learning:obs_mrg7vk

## ADR-006: In a dual-index build, an unrecoverable per-file desync must abort BEFORE persisting the manifest so the old index survives and the next query self-heals via full rebuild — never silently continue past it

- **Date**: 2026-06-09
- **Status**: Accepted
- **Context**: the search index build pipeline indexes each file into BOTH a lexical and an AST index under a shared FileId, then persists a manifest
- **Decision**: treat an AST-add failure that occurs after the same-FileId lexical entry was already accepted as unrecoverable — propagate an error up through run() so it aborts BEFORE new_manifest.save() is ever reached
- **Consequences**: persisting a desynced manifest commits silent corruption that survives across sessions, whereas aborting leaves the prior valid manifest in place so the next query self-heals via a full rebuild
- **Source**: self-learning:obs_fdz9k3
