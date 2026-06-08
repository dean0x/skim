<!-- TL;DR: 6 pitfalls. Key: PF-002, PF-003, PF-004, PF-005, PF-006 -->
# Known Pitfalls

Area-specific gotchas, fragile areas, and past bugs.

## PF-001: Post-release verification checklist omitting Homebrew tap check

- **Area**: post-release verification checklist (release process documentation in CLAUDE.md)
- **Issue**: after completing a release the assistant declared 'All done' without proactively checking whether the Homebrew tap PR was merged and the formula updated — the user had to ask explicitly
- **Impact**: User had to prompt for the brew tap check after the assistant declared the release complete — a broken or lagging Homebrew tap would go unnoticed if the agent treats the checklist as done
- **Resolution**: Always include `brew update && brew info dean0x/tap/skim` as the fourth post-release verification step. Full checklist per CLAUDE.md: (1) `cargo install rskim` — shows new version, (2) `npx rskim --version` — shows new version, (3) GitHub Release page — 7 binary assets attached, (4) `brew update && brew info dean0x/tap/skim` — formula updated.
- **Status**: Active
- **Source**: self-learning:obs_qt2m8p

## PF-002: Classifying review findings as pre-existing or deferred to skip resolution

- **Area**: review resolution system (Resolver agent, resolution summary output)
- **Issue**: The assistant grouped unresolved review findings into 'pre-existing', 'deferred', 'tech debt', and 'suggestions' categories and declared resolution complete — implicitly treating these as skippable without explicit user sign-off. The user rejected this framing in two distinct sessions, each time asking to have the skipped items explained and fixed
- **Impact**: Silently closing pre-existing or tech-debt findings accumulates hidden debt and triggers a correction turn where the user must ask for re-explanation and re-planning
- **Resolution**: Never present pre-existing, deferred, or tech-debt findings as closed without explicit user sign-off. Surface all findings with explanations and let the user decide. User's explicit policy: 'I am not one to notice an issue and skip it, even if it is preexisting' and 'I really hate tech debt. my approach is if you see something do something.'
- **Status**: Active
- **Source**: self-learning:obs_nk8w2v

## PF-003: Attributing command failures to external tools without verifying skim's rewrite hook intercepted the command

- **Area**: rewrite hook rule matching for git commands
- **Issue**: assistant attributed two git command failures (zsh parenthesis glob expansion in commit message, git branch -d squash-merge ancestry check) to non-skim causes without ruling out whether skim's rewrite hook intercepted those commands
- **Impact**: genuine skim involvement is dismissed prematurely, causing the user to challenge with 'Are you one hundred percent sure?' — mirrors prior npx instance where zero output was attributed to external npx hanging rather than skim's vitest rewrite
- **Resolution**: before attributing any command failure to an external tool in a project where skim is installed, verify (1) whether skim's rewrite hook matched that command and (2) whether the rewrite itself could have caused the failure mode. Never declare 'not related to skim' without checking the engine.rs rule table for the failing command pattern.
- **Status**: Active
- **Source**: self-learning:obs_yw3m6d

## PF-004: u16 depth arithmetic overflow: widen to u32 before adding an offset in depth comparisons

- **Area**: extract.rs gap-fill depth arithmetic in extract_ast_ngrams_with_weights
- **Issue**: the condition `node.depth > p + 1` uses u16 arithmetic, so when prev_depth p == u16::MAX the addition wraps to 0, making the condition silently false and skipping gap-fill entirely, risking a panic on the subsequent slice index or a spurious parent-child edge
- **Impact**: a file with a CST node at depth 65535 (or a corrupt u16 in synthetic DI input) bypasses the gap-fill guard
- **Resolution**: always widen u16 depth values to u32 before arithmetic in comparisons -- use u32::from(p) + 1, not p + 1, when p is u16. Generalizes to any bounded integer: widen before adding an offset rather than risk wrap at the type maximum.
- **Status**: Active
- **Source**: self-learning:obs_kp2v7n

## PF-005: Acceptance criteria copied verbatim from an issue may be empirically baseless — verify against research before treating as a hard gate

- **Area**: acceptance criteria / quality gates for index size
- **Issue**: a numeric acceptance criterion (A16: index < 5% of source) was inherited verbatim from the GitHub issue without empirical grounding
- **Impact**: an impossible gate forces either a blocked PR, an #[ignore] cop-out, or test-gaming — all of which erode trust in the suite
- **Resolution**: before enforcing an inherited numeric criterion, trace it to a measured basis
- **Status**: Active
- **Source**: self-learning:obs_acqv8m

## PF-006: Feature-knowledge files (KNOWLEDGE.md, index.json) go stale after a rename refactor, leaving broken import-path references

- **Area**: .devflow feature-knowledge maintenance after refactors
- **Issue**: a module or symbol rename (test_support to test_utils, PR #279) propagated through 77 compiled .rs files but left build-parsers KNOWLEDGE.md and index.json referencing the old name and a now-broken crate::cmd::test_support import path, because doc and metadata files are not type-checked by cargo
- **Impact**: feature knowledge surfaced to future agents points at non-existent symbols, eroding trust in the pre-computed context and causing wasted exploration
- **Resolution**: when a rename refactor touches symbols named in feature knowledge, grep KNOWLEDGE.md and index.json for the old identifier with word boundaries and update referencedFiles and import paths in the same PR — treat feature-knowledge drift as part of the refactor blast radius
- **Status**: Active
- **Source**: self-learning:obs_knstal
