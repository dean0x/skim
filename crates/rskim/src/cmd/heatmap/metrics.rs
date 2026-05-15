//! Pure computation functions for `skim heatmap` — 6 risk metrics.
//!
//! Zero I/O. All functions accept `&[CommitRecord]` and return computed values.
//! `now_epoch` is a parameter (not `SystemTime::now()`) for deterministic tests.

use std::collections::{HashMap, HashSet};

use super::types::{
    AuthorMetrics, ChurnMetrics, CommitRecord, CouplingEdge, CouplingEntry, FixRiskMetrics,
    ModuleHealth,
};

// ============================================================================
// Metric 1: Churn
// ============================================================================

/// Count commits per file and compute rate = file_commits / total_commits.
pub(crate) fn compute_churn(commits: &[CommitRecord]) -> HashMap<String, ChurnMetrics> {
    let total = commits.len();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for commit in commits {
        for file in &commit.changed_files {
            *counts
                .entry(file.path_str().into_owned())
                .or_insert(0) += 1;
        }
    }

    counts
        .into_iter()
        .map(|(path, file_commits)| {
            let rate = if total == 0 {
                0.0
            } else {
                file_commits as f64 / total as f64
            };
            (
                path,
                ChurnMetrics {
                    commits: file_commits,
                    rate,
                },
            )
        })
        .collect()
}

// ============================================================================
// Metric 2: Coupling
// ============================================================================

/// Maximum number of files in a commit that participates in coupling pair generation.
///
/// Commits touching more than this many files (e.g. large reformats) still count
/// toward `weighted_total` for each file, but are skipped for pair enumeration.
/// This caps worst-case pair allocations at 50*49 = 2450 per commit instead of
/// unbounded O(n^2).
const COUPLING_MAX_FILES: usize = 50;

/// Compute file coupling from commit co-occurrences.
///
/// Weights each pair's contribution by `1.0 / sqrt(files_in_commit)` to
/// discount large commits (e.g. reformatting all files at once).
///
/// Commits with more than [`COUPLING_MAX_FILES`] files are excluded from pair
/// enumeration (but still contribute to each file's `weighted_total`).
///
/// Returns:
/// - per-file blast radius map: `HashMap<String, Vec<CouplingEntry>>`
/// - global coupling graph: `Vec<CouplingEdge>`
pub(crate) fn compute_coupling(
    commits: &[CommitRecord],
    threshold: f64,
    min_support: usize,
) -> (HashMap<String, Vec<CouplingEntry>>, Vec<CouplingEdge>) {
    // co_occur[(a, b)] = (weighted_sum, raw_count) for ordered pair (a, b).
    // &str keys borrow from PathBuf::to_str() on each FileChangeInfo.path — valid
    // for the entire function because `commits` (and all CommitRecords) outlive
    // these maps. Non-UTF-8 paths fall back to "" via unwrap_or_default().
    let mut co_occur: HashMap<(&str, &str), (f64, usize)> = HashMap::new();
    // weighted_total[a] = weighted sum of commits touching a
    let mut weighted_total: HashMap<&str, f64> = HashMap::new();

    for commit in commits {
        let files: Vec<&str> = commit
            .changed_files
            .iter()
            .map(|f| f.path.to_str().unwrap_or_default())
            .collect();
        let n = files.len();
        let weight = 1.0 / (n as f64).sqrt();

        // Every commit contributes to weighted_total for its files
        for f in &files {
            *weighted_total.entry(f).or_insert(0.0) += weight;
        }

        // Skip large commits for pair enumeration to avoid O(n^2) blowup
        if !(2..=COUPLING_MAX_FILES).contains(&n) {
            continue;
        }

        // All ordered pairs (a, b) where a != b — zero allocations in the hot path
        for i in 0..n {
            for j in 0..n {
                if i == j {
                    continue;
                }
                let key = (files[i], files[j]);
                let entry = co_occur.entry(key).or_insert((0.0, 0));
                entry.0 += weight;
                entry.1 += 1;
            }
        }
    }

    // Build blast_radius per file and global graph — .to_string() only here,
    // when building the owned output structures.
    let mut blast_radius: HashMap<String, Vec<CouplingEntry>> = HashMap::new();
    let mut graph_edges: HashMap<(String, String), (f64, usize)> = HashMap::new();

    for (&(a, b), &(weighted_co, sup)) in &co_occur {
        let total_a = weighted_total.get(a).copied().unwrap_or(0.0);
        if total_a == 0.0 {
            continue;
        }
        let confidence = weighted_co / total_a;
        if confidence < threshold {
            continue;
        }
        if sup < min_support {
            continue;
        }

        blast_radius
            .entry(a.to_string())
            .or_default()
            .push(CouplingEntry {
                path: b.to_string(),
                confidence,
                support: sup,
            });

        // De-duplicate edges: only store canonical (smaller, larger) pair
        let edge_key = if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        };
        let entry = graph_edges.entry(edge_key).or_insert((0.0, sup));
        if confidence > entry.0 {
            entry.0 = confidence;
        }
    }

    // Sort each blast radius by confidence descending
    for entries in blast_radius.values_mut() {
        entries.sort_by(|x, y| y.confidence.total_cmp(&x.confidence));
    }

    let coupling_graph: Vec<CouplingEdge> = graph_edges
        .into_iter()
        .map(|((a, b), (confidence, support))| CouplingEdge {
            a,
            b,
            confidence,
            support,
        })
        .collect();

    (blast_radius, coupling_graph)
}

// ============================================================================
// Metric 3: Stability
// ============================================================================

/// Compute a stability score [0, 100] for each file.
///
/// Formula: `100 - (churn_component * 40 + recency_component * 35 + volatility_component * 25)`
///
/// - `churn_component` = file_commits / max_churn (0–1)
/// - `recency_component` = inverted exponential decay based on days since last change
/// - `volatility_component` = fix_commits / total_commits for the file
///
/// `now_epoch` is a parameter so tests are deterministic.
pub(crate) fn compute_stability(
    commits: &[CommitRecord],
    max_churn: usize,
    now_epoch: u64,
) -> HashMap<String, u8> {
    // Per-file commit info
    let mut file_commits: HashMap<String, Vec<u64>> = HashMap::new(); // file -> timestamps
    let mut file_fix_count: HashMap<String, usize> = HashMap::new();

    for commit in commits {
        let is_fix = rskim_search::is_fix_commit(&commit.message);
        for file in &commit.changed_files {
            let path_str = file.path_str().into_owned();
            file_commits
                .entry(path_str.clone())
                .or_default()
                .push(commit.timestamp.max(0) as u64);
            if is_fix {
                *file_fix_count.entry(path_str).or_insert(0) += 1;
            }
        }
    }

    let effective_max = max_churn.max(1);

    file_commits
        .into_iter()
        .map(|(path, timestamps)| {
            let commit_count = timestamps.len();
            let churn_component = (commit_count as f64 / effective_max as f64).min(1.0);

            // Recency: days since last commit. Decay half-life ~30 days.
            let last_ts = timestamps.iter().copied().max().unwrap_or(0);
            let days_since = if now_epoch >= last_ts {
                ((now_epoch - last_ts) as f64) / 86400.0
            } else {
                0.0
            };
            // Recent files are riskier: exp(-days/30) → 1.0 today, ~0 after months.
            let recency_component = (-days_since / 30.0_f64).exp().clamp(0.0, 1.0);

            let fix_count = file_fix_count.get(&path).copied().unwrap_or(0);
            let volatility_component = if commit_count == 0 {
                0.0
            } else {
                (fix_count as f64 / commit_count as f64).min(1.0)
            };

            let penalty =
                churn_component * 40.0 + recency_component * 35.0 + volatility_component * 25.0;
            let score = (100.0 - penalty).round().clamp(0.0, 100.0) as u8;

            (path, score)
        })
        .collect()
}

// ============================================================================
// Metric 4: Author diversity
// ============================================================================

/// Compute author diversity metrics per file.
pub(crate) fn compute_authors(commits: &[CommitRecord]) -> HashMap<String, AuthorMetrics> {
    // file -> author -> commit_count
    let mut file_author_counts: HashMap<String, HashMap<String, usize>> = HashMap::new();

    for commit in commits {
        for file in &commit.changed_files {
            *file_author_counts
                .entry(file.path_str().into_owned())
                .or_default()
                .entry(commit.author.clone())
                .or_insert(0) += 1;
        }
    }

    file_author_counts
        .into_iter()
        .map(|(path, author_counts)| {
            let total: usize = author_counts.values().sum();
            if total == 0 {
                return (
                    path,
                    AuthorMetrics {
                        count: 0,
                        top_author_pct: 0.0,
                        single_owner_risk: false,
                    },
                );
            }

            let max_count = author_counts.values().copied().max().unwrap_or(0);
            let top_author_pct = (max_count as f64 / total as f64) * 100.0;
            let single_owner_risk = top_author_pct > 80.0;

            // Authors with >5% of commits
            let threshold = (total as f64 * 0.05).ceil() as usize;
            let count = author_counts.values().filter(|&&c| c >= threshold).count();

            (
                path,
                AuthorMetrics {
                    count,
                    top_author_pct,
                    single_owner_risk,
                },
            )
        })
        .collect()
}

// ============================================================================
// Metric 5: Fix-after-touch
// ============================================================================

/// Compute fix-after-touch risk per file.
///
/// - `keyword_pct`: percentage of commits touching the file that are fix commits.
/// - `proximity_pct`: percentage of non-fix commits after which a fix commit
///   also touching the file appears within `window` commits.
/// - `combined_pct`: union (not sum) of the two signals.
/// - `insufficient_data`: true when the file has <2 commits.
pub(crate) fn compute_fix_after_touch(
    commits: &[CommitRecord],
    window: usize,
) -> HashMap<String, FixRiskMetrics> {
    // Classify every commit
    let is_fix: Vec<bool> = commits
        .iter()
        .map(|c| rskim_search::is_fix_commit(&c.message))
        .collect();

    // Per-file: which commit indices touch it?
    let mut file_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, commit) in commits.iter().enumerate() {
        for file in &commit.changed_files {
            file_indices
                .entry(file.path_str().into_owned())
                .or_default()
                .push(i);
        }
    }

    file_indices
        .into_iter()
        .map(|(path, indices)| {
            let total = indices.len();
            if total < 2 {
                return (
                    path,
                    FixRiskMetrics {
                        keyword_pct: 0.0,
                        proximity_pct: 0.0,
                        combined_pct: 0.0,
                        insufficient_data: true,
                    },
                );
            }

            // Keyword: fix commits touching this file
            let keyword_count = indices.iter().filter(|&&i| is_fix[i]).count();
            let keyword_pct = (keyword_count as f64 / total as f64) * 100.0;

            // Proximity: non-fix commit at index i, followed by a fix commit
            // also touching this file within the next `window` commits
            let index_set: HashSet<usize> = indices.iter().copied().collect();
            let non_fix_indices: Vec<usize> =
                indices.iter().copied().filter(|&i| !is_fix[i]).collect();

            // Count non-fix indices followed by a fix within window (no need to
            // materialise a set — uniqueness is guaranteed because each non-fix
            // index appears at most once in non_fix_indices).
            let proximity_count: usize = non_fix_indices
                .iter()
                .copied()
                .filter(|&idx| {
                    let upper = (idx + 1 + window).min(commits.len());
                    ((idx + 1)..upper).any(|j| is_fix[j] && index_set.contains(&j))
                })
                .count();

            let proximity_pct = if non_fix_indices.is_empty() {
                0.0
            } else {
                (proximity_count as f64 / total as f64) * 100.0
            };

            // Union: fix commits (keyword) ∪ non-fix commits followed by a fix (proximity).
            // The two sets are disjoint by construction (proximity_count only counts non-fix
            // indices), so union_count = keyword_count + proximity_count.
            let union_count = keyword_count + proximity_count;
            let combined_pct = (union_count as f64 / total as f64) * 100.0;

            (
                path,
                FixRiskMetrics {
                    keyword_pct,
                    proximity_pct,
                    combined_pct,
                    insufficient_data: false,
                },
            )
        })
        .collect()
}

// ============================================================================
// Metric 6: Encapsulation
// ============================================================================

/// Extract the top-level directory component from a file path.
///
/// Returns `None` for root-level files (no directory component), e.g. `Makefile`.
/// Returns `Some("src")` for `src/lib.rs`, `Some("tests")` for `tests/foo.rs`.
fn extract_top_dir(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    // Require at least one parent directory (not root-level files)
    let parent = p.parent()?;
    let s = parent.to_string_lossy();
    if s.is_empty() || s == "." {
        return None;
    }
    // Return the first path component as the module name
    p.components()
        .next()
        .and_then(|c| c.as_os_str().to_str().map(String::from))
}

/// Compute module encapsulation health.
///
/// For each directory, counts commits that touch ONLY that directory vs.
/// commits that also touch other directories (cross-boundary).
///
/// Filters modules with fewer than `min_commits` total commits.
/// Returns results sorted by encapsulation_pct ascending (worst first).
pub(crate) fn compute_encapsulation(
    commits: &[CommitRecord],
    min_commits: usize,
) -> Vec<ModuleHealth> {
    // module -> files seen
    let mut module_files: HashMap<String, HashSet<String>> = HashMap::new();
    // module -> (total_commits, cross_boundary_commits)
    let mut module_stats: HashMap<String, (usize, usize)> = HashMap::new();

    for commit in commits {
        // Precompute top-level directory for each file once to avoid calling
        // extract_top_dir twice per file (once for dirs, once for module_files).
        let file_dirs: Vec<Option<String>> = commit
            .changed_files
            .iter()
            .map(|f| extract_top_dir(&f.path_str()))
            .collect();

        // Collect unique top-level directories for this commit
        let dirs: HashSet<&str> = file_dirs.iter().filter_map(|d| d.as_deref()).collect();

        // Track files per module
        for (file, dir) in commit.changed_files.iter().zip(file_dirs.iter()) {
            if let Some(d) = dir {
                module_files
                    .entry(d.clone())
                    .or_default()
                    .insert(file.path_str().into_owned());
            }
        }

        // Cross-boundary = commit touches >1 module
        let is_cross = dirs.len() > 1;
        for dir in &dirs {
            let entry = module_stats.entry(dir.to_string()).or_insert((0, 0));
            entry.0 += 1;
            if is_cross {
                entry.1 += 1;
            }
        }
    }

    let mut results: Vec<ModuleHealth> = module_stats
        .into_iter()
        .filter_map(|(path, (total_commits, cross_boundary_commits))| {
            if total_commits < min_commits {
                return None;
            }
            let encapsulation_pct = if total_commits == 0 {
                100.0
            } else {
                let internal = total_commits.saturating_sub(cross_boundary_commits);
                (internal as f64 / total_commits as f64) * 100.0
            };
            let files_count = module_files.get(&path).map(|s| s.len()).unwrap_or(0);
            Some(ModuleHealth {
                path,
                encapsulation_pct,
                files_count,
                total_commits,
                cross_boundary_commits,
            })
        })
        .collect();

    // Sort by encapsulation_pct ascending (worst first)
    results.sort_by(|a, b| a.encapsulation_pct.total_cmp(&b.encapsulation_pct));

    results
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::FileChange;

    fn make_commit(
        hash: &str,
        author: &str,
        ts: u64,
        subject: &str,
        files: &[&str],
    ) -> CommitRecord {
        CommitRecord {
            hash: hash.to_string(),
            author: author.to_string(),
            timestamp: ts as i64,
            message: subject.to_string(),
            changed_files: files
                .iter()
                .map(|p| FileChange {
                    path: std::path::PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        }
    }

    // -----------------------------------------------------------------------
    // compute_churn
    // -----------------------------------------------------------------------

    #[test]
    fn test_churn_empty() {
        let result = compute_churn(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_churn_single_commit() {
        let commits = vec![make_commit("h1", "Alice", 1000, "msg", &["a.rs"])];
        let result = compute_churn(&commits);
        let m = result.get("a.rs").unwrap();
        assert_eq!(m.commits, 1);
        assert!((m.rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_churn_multiple_commits() {
        let commits = vec![
            make_commit("h1", "Alice", 1000, "msg", &["a.rs", "b.rs"]),
            make_commit("h2", "Bob", 2000, "msg", &["a.rs"]),
        ];
        let result = compute_churn(&commits);
        assert_eq!(result["a.rs"].commits, 2);
        assert_eq!(result["b.rs"].commits, 1);
        assert!((result["a.rs"].rate - 1.0).abs() < 1e-9); // 2/2
        assert!((result["b.rs"].rate - 0.5).abs() < 1e-9); // 1/2
    }

    // -----------------------------------------------------------------------
    // compute_coupling
    // -----------------------------------------------------------------------

    #[test]
    fn test_coupling_empty() {
        let (blast, graph) = compute_coupling(&[], 0.5, 1);
        assert!(blast.is_empty());
        assert!(graph.is_empty());
    }

    #[test]
    fn test_coupling_below_threshold_excluded() {
        // 3 commits: a.rs and b.rs appear together in 1, a.rs alone in 2 others.
        // weighted_total[a] = 1/sqrt(2) + 1/sqrt(1) + 1/sqrt(1) ≈ 0.707 + 2 = 2.707
        // co_occur[(a,b)] = 1/sqrt(2) ≈ 0.707
        // confidence(a→b) = 0.707 / 2.707 ≈ 0.261
        // threshold 0.5 should exclude this pair
        let commits = vec![
            make_commit("h1", "Alice", 1, "msg", &["a.rs", "b.rs"]),
            make_commit("h2", "Alice", 2, "msg", &["a.rs"]),
            make_commit("h3", "Alice", 3, "msg", &["a.rs"]),
        ];
        let (blast, _graph) = compute_coupling(&commits, 0.5, 1);
        // confidence ~0.261 < 0.5, so no coupling entries for a.rs
        let a_entries = blast.get("a.rs").map(|v| v.len()).unwrap_or(0);
        assert_eq!(a_entries, 0);
    }

    #[test]
    fn test_coupling_below_min_support_excluded() {
        let commits = vec![make_commit("h1", "Alice", 1000, "msg", &["a.rs", "b.rs"])];
        let (blast, _graph) = compute_coupling(&commits, 0.0, 5); // min_support=5, only 1 commit
        assert_eq!(blast.get("a.rs").map(|v| v.len()).unwrap_or(0), 0);
    }

    #[test]
    fn test_coupling_strong_pair() {
        // a.rs and b.rs always appear together
        let commits = vec![
            make_commit("h1", "A", 1, "m", &["a.rs", "b.rs"]),
            make_commit("h2", "A", 2, "m", &["a.rs", "b.rs"]),
            make_commit("h3", "A", 3, "m", &["a.rs", "b.rs"]),
        ];
        let (blast, graph) = compute_coupling(&commits, 0.5, 3);
        let a_entries = blast.get("a.rs").unwrap();
        assert!(!a_entries.is_empty());
        assert_eq!(a_entries[0].path, "b.rs");
        assert!(!graph.is_empty());
    }

    #[test]
    fn test_coupling_large_commit_excluded_from_pairs() {
        // Build a commit with COUPLING_MAX_FILES + 1 files — must not generate pairs,
        // but the files should still appear in weighted_total (so they are not lost
        // from the churn perspective).
        let many_files: Vec<String> = (0..=COUPLING_MAX_FILES)
            .map(|i| format!("file_{i}.rs"))
            .collect();
        let file_refs: Vec<&str> = many_files.iter().map(String::as_str).collect();
        let commits = vec![make_commit("h1", "A", 1, "m", &file_refs)];

        // With threshold=0.0 and min_support=1, any pair would be included if generated.
        // The large-commit cap means zero pairs should appear.
        let (blast, graph) = compute_coupling(&commits, 0.0, 1);
        assert!(
            blast.is_empty(),
            "large commit must not generate coupling pairs"
        );
        assert!(
            graph.is_empty(),
            "large commit must not generate graph edges"
        );
    }

    // -----------------------------------------------------------------------
    // compute_stability
    // -----------------------------------------------------------------------

    #[test]
    fn test_stability_empty() {
        let result = compute_stability(&[], 0, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_stability_moderate_for_max_churn_old_file() {
        // One commit, old (365 days ago), no fix keywords, but max churn (1/1)
        let old_ts = 0u64;
        let now = 365 * 86400u64;
        let commits = vec![make_commit(
            "h1",
            "Alice",
            old_ts,
            "feat: initial",
            &["stable.rs"],
        )];
        let result = compute_stability(&commits, 1, now);
        let score = result["stable.rs"];
        // Churn = 1/1 = 1.0 → penalty 40, recency ≈ 0 (old), volatility = 0
        // penalty ≈ 40, score ≈ 60
        assert!(
            score >= 50 && score <= 70,
            "expected moderate score for max-churn but old file, got {score}"
        );
    }

    #[test]
    fn test_stability_recent_fix_lowers_score() {
        let now = 1_000_000u64;
        let commits = vec![make_commit(
            "h1",
            "Alice",
            now - 100,
            "fix: critical bug",
            &["risky.rs"],
        )];
        let result = compute_stability(&commits, 1, now);
        let score = result["risky.rs"];
        // Recent fix commit → low stability
        assert!(score < 50, "expected low score for recent fix, got {score}");
    }

    // -----------------------------------------------------------------------
    // compute_authors
    // -----------------------------------------------------------------------

    #[test]
    fn test_authors_empty() {
        let result = compute_authors(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_authors_single_owner() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["a.rs"]),
            make_commit("h2", "Alice", 2, "m", &["a.rs"]),
            make_commit("h3", "Alice", 3, "m", &["a.rs"]),
        ];
        let result = compute_authors(&commits);
        let m = &result["a.rs"];
        assert!(m.single_owner_risk);
        assert!((m.top_author_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_authors_multiple_owners() {
        let mut commits = Vec::new();
        for i in 0..5u64 {
            commits.push(make_commit(&format!("h{i}"), "Alice", i, "m", &["a.rs"]));
        }
        commits.push(make_commit("h5", "Bob", 5, "m", &["a.rs"]));
        let result = compute_authors(&commits);
        let m = &result["a.rs"];
        // Alice: 5/6 = 83.3% → single_owner_risk = true
        assert!(m.single_owner_risk);
    }

    #[test]
    fn test_authors_no_single_owner_risk() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["a.rs"]),
            make_commit("h2", "Bob", 2, "m", &["a.rs"]),
            make_commit("h3", "Charlie", 3, "m", &["a.rs"]),
            make_commit("h4", "Dave", 4, "m", &["a.rs"]),
            make_commit("h5", "Eve", 5, "m", &["a.rs"]),
        ];
        let result = compute_authors(&commits);
        // Each author has 20% → no single owner
        assert!(!result["a.rs"].single_owner_risk);
    }

    // -----------------------------------------------------------------------
    // compute_fix_after_touch
    // -----------------------------------------------------------------------

    #[test]
    fn test_fix_risk_empty() {
        let result = compute_fix_after_touch(&[], 5);
        assert!(result.is_empty());
    }

    #[test]
    fn test_fix_risk_single_commit_insufficient_data() {
        let commits = vec![make_commit("h1", "Alice", 1, "feat: add", &["a.rs"])];
        let result = compute_fix_after_touch(&commits, 5);
        assert!(result["a.rs"].insufficient_data);
    }

    #[test]
    fn test_fix_risk_keyword_detection() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
            make_commit("h2", "Alice", 2, "fix: bug in a", &["a.rs"]),
        ];
        let result = compute_fix_after_touch(&commits, 5);
        let m = &result["a.rs"];
        assert!(!m.insufficient_data);
        // 1 fix commit out of 2 = 50%
        assert!((m.keyword_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_fix_risk_proximity_detection() {
        // commit 0: feat touching a.rs
        // commit 1: fix touching a.rs (within window of commit 0)
        let commits = vec![
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
            make_commit("h2", "Alice", 2, "fix: quick patch", &["a.rs"]),
        ];
        let result = compute_fix_after_touch(&commits, 5);
        let m = &result["a.rs"];
        assert!(!m.insufficient_data);
        // total=2, proximity_set={0}, proximity_pct = 1/2*100 = 50%
        assert!((m.proximity_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_fix_risk_combined_both_signals() {
        // Exercises the disjointness optimization: keyword_count + proximity_set.len().
        //
        // commit 0 (idx 0): feat touching a.rs → non-fix
        // commit 1 (idx 1): fix touching a.rs  → keyword hit; also within window of idx 0
        //                                         so idx 0 enters proximity_set
        // commit 2 (idx 2): feat touching a.rs → non-fix (outside window trailing edge)
        //
        // Expected for a.rs (total=3):
        //   keyword_count = 1  → keyword_pct  = 1/3*100 ≈ 33.33%
        //   proximity_set = {0} (size 1) → proximity_pct = 1/3*100 ≈ 33.33%
        //   union_count   = 2  → combined_pct = 2/3*100 ≈ 66.67%
        let commits = vec![
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
            make_commit("h2", "Alice", 2, "fix: bug in a", &["a.rs"]),
            make_commit("h3", "Alice", 3, "feat: more work", &["a.rs"]),
        ];
        let result = compute_fix_after_touch(&commits, 5);
        let m = &result["a.rs"];

        assert!(!m.insufficient_data);

        let expected_keyword_pct = 100.0 / 3.0; // 33.33…
        let expected_proximity_pct = 100.0 / 3.0; // 33.33…
        let expected_combined_pct = 200.0 / 3.0; // 66.67…

        assert!(
            (m.keyword_pct - expected_keyword_pct).abs() < 1e-9,
            "keyword_pct: expected {expected_keyword_pct}, got {}",
            m.keyword_pct
        );
        assert!(
            (m.proximity_pct - expected_proximity_pct).abs() < 1e-9,
            "proximity_pct: expected {expected_proximity_pct}, got {}",
            m.proximity_pct
        );
        assert!(
            (m.combined_pct - expected_combined_pct).abs() < 1e-9,
            "combined_pct: expected {expected_combined_pct}, got {}",
            m.combined_pct
        );
    }

    // -----------------------------------------------------------------------
    // compute_encapsulation
    // -----------------------------------------------------------------------

    #[test]
    fn test_encapsulation_empty() {
        let result = compute_encapsulation(&[], 1);
        assert!(result.is_empty());
    }

    #[test]
    fn test_encapsulation_single_module() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["src/a.rs", "src/b.rs"]),
            make_commit("h2", "Alice", 2, "m", &["src/c.rs"]),
            make_commit("h3", "Alice", 3, "m", &["src/d.rs"]),
        ];
        let result = compute_encapsulation(&commits, 1);
        // All commits touch only "src" module → 100% encapsulation
        let src = result.iter().find(|m| m.path == "src").unwrap();
        assert!((src.encapsulation_pct - 100.0).abs() < 1e-9);
    }

    #[test]
    fn test_encapsulation_cross_boundary() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["src/a.rs", "tests/b.rs"]),
            make_commit("h2", "Alice", 2, "m", &["src/c.rs"]),
            make_commit("h3", "Alice", 3, "m", &["src/d.rs"]),
        ];
        let result = compute_encapsulation(&commits, 1);
        let src = result.iter().find(|m| m.path == "src").unwrap();
        // 1 cross-boundary out of 3 → 66.7% internal → encapsulation = 66.7%
        assert!(src.encapsulation_pct < 100.0);
        assert_eq!(src.cross_boundary_commits, 1);
    }

    #[test]
    fn test_encapsulation_min_commits_filter() {
        let commits = vec![make_commit("h1", "Alice", 1, "m", &["src/a.rs"])];
        let result = compute_encapsulation(&commits, 3); // requires ≥3 commits
        assert!(result.is_empty());
    }

    #[test]
    fn test_encapsulation_sorted_ascending() {
        let commits = vec![
            // src: 1 cross out of 2 = 50% encapsulation
            make_commit("h1", "Alice", 1, "m", &["src/a.rs", "lib/b.rs"]),
            make_commit("h2", "Alice", 2, "m", &["src/c.rs"]),
            // lib: 1 cross out of 2 = 50% encapsulation — both should be returned
            make_commit("h3", "Alice", 3, "m", &["lib/d.rs"]),
            make_commit("h4", "Alice", 4, "m", &["lib/e.rs", "src/f.rs"]),
        ];
        let result = compute_encapsulation(&commits, 1);
        // Sorted ascending by encapsulation_pct
        for window in result.windows(2) {
            assert!(window[0].encapsulation_pct <= window[1].encapsulation_pct);
        }
    }

    #[test]
    fn test_extract_top_dir_root_level_file() {
        assert_eq!(extract_top_dir("Makefile"), None);
        assert_eq!(extract_top_dir("README.md"), None);
    }

    #[test]
    fn test_extract_top_dir_nested_file() {
        assert_eq!(extract_top_dir("src/lib.rs"), Some("src".to_string()));
        assert_eq!(
            extract_top_dir("src/cmd/heatmap/mod.rs"),
            Some("src".to_string())
        );
        assert_eq!(extract_top_dir("tests/foo.rs"), Some("tests".to_string()));
    }
}
