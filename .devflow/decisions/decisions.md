<!-- TL;DR: 2 decisions. Key: ADR-001, ADR-002 -->
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
