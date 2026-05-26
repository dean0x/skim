<!-- TL;DR: 1 decisions. Key: ADR-001 -->
# Architectural Decisions

Append-only. Status changes allowed; deletions prohibited.

## ADR-001: Fix all noticed issues immediately regardless of scope — 'if you see something, do something'

- **Date**: 2026-04-30
- **Status**: Accepted
- **Context**: reviewer surfaced 7 findings including 4 classified as 'follow-up tickets' non-blocking for merge
- **Decision**: resolve all findings immediately — no deferral to future tickets or tech debt backlog regardless of blocking severity
- **Consequences**: 'I really hate tech debt' and 'I don't want to leave anything for later' — user treats noticed issues as immediate obligations regardless of blocking status. **Why:** the cost of leaving a known issue is higher than the cost of extended scope. **How to apply:** when reviewers flag any issues (blocking OR non-blocking), always surface them all to the user for decision rather than silently classifying as deferred. The user will almost always choose to fix now.
- **Source**: self-learning:obs_jk7n2w
