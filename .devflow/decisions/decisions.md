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

## ADR-002: Replace empirically-baseless acceptance criteria with grounded regression guards rather than chasing impossible targets

- **Date**: 2026-06-04
- **Status**: Accepted
- **Context**: issue #194 acceptance criterion A16 demanded the AST index be < 5% of source bytes, but structural AST n-grams are dense by design (O(vocab x files) posting entries) so 5% is unreachable and the figure had no empirical origin in any prior wave research
- **Decision**: replace the 5% target with a defensible < 3x source-bytes regression guard (measured 1.23x) as a real non-ignored test, and file on-disk compression as a tracked follow-up (#273)
- **Consequences**: a regression guard grounded in measurement and industry norms (uncompressed code-search trigram indexes run 3-5x) protects against real bloat, whereas an impossible target either blocks the PR or gets silently ignored
- **Source**: self-learning:obs_a16x3g
