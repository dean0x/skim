//! Pure computation functions for `skim heatmap` — 6 risk metrics.
//!
//! Zero I/O. All functions accept `&[CommitInfo]` and return computed values.
//! `now_epoch` is a parameter (not `SystemTime::now()`) for deterministic tests.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::noise::{BUS_FACTOR_TOP_SHARE_PCT, COUPLING_MAX_FILES, MIN_FILE_SUPPORT};
use super::types::{
    AuthorMetrics, ChurnMetrics, CommitInfo, CouplingEdge, CouplingEntry, FixRiskMetrics,
    ModuleHealth,
};

// ============================================================================
// Metric 1: Churn
// ============================================================================

/// Count commits per file and compute rate = file_commits / total_commits.
pub(crate) fn compute_churn(commits: &[CommitInfo]) -> HashMap<String, ChurnMetrics> {
    let total = commits.len();
    let mut counts: HashMap<String, usize> = HashMap::new();

    for commit in commits {
        for file in &commit.changed_files {
            *counts.entry(file.path_str().into_owned()).or_insert(0) += 1;
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

/// Output of [`compute_coupling`].
#[derive(Debug)]
pub(crate) struct CouplingResult {
    /// Per-file partners, each list sorted by [`entry_order`].
    pub(crate) blast_radius: HashMap<String, Vec<CouplingEntry>>,
    /// Undirected edges, sorted by [`edge_order`].
    pub(crate) graph: Vec<CouplingEdge>,
    /// Commits with more than [`COUPLING_MAX_FILES`] files. They are not
    /// paired and add nothing to the weighted totals.
    pub(crate) large_commits_skipped: usize,
}

/// Partner order: confidence desc, then support desc, then path asc.
fn entry_order(x: &CouplingEntry, y: &CouplingEntry) -> Ordering {
    y.confidence
        .total_cmp(&x.confidence)
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.path.cmp(&y.path))
}

/// Edge order: confidence desc, then support desc, then `a` asc, then `b` asc.
pub(crate) fn edge_order(x: &CouplingEdge, y: &CouplingEdge) -> Ordering {
    y.confidence
        .total_cmp(&x.confidence)
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.a.cmp(&y.a))
        .then_with(|| x.b.cmp(&y.b))
}

/// Compute file coupling from commit co-occurrences.
///
/// Weights each pair's contribution by `1.0 / sqrt(files_in_commit)` to
/// discount large commits (e.g. reformatting all files at once).
///
/// A commit with more than [`COUPLING_MAX_FILES`] files adds to neither the
/// pair counts nor the weighted totals, so the same commits make the
/// numerator and the denominator of each confidence; the result counts these
/// commits. A single-file commit still adds to the weighted total of its
/// file, because it is a real solo change.
pub(crate) fn compute_coupling(
    commits: &[CommitInfo],
    threshold: f64,
    min_support: usize,
) -> CouplingResult {
    // co_occur[(a, b)] = (weighted_sum, raw_count) for ordered pair (a, b).
    // &str keys borrow from PathBuf::to_str() on each FileChangeInfo.path — valid
    // for the entire function because `commits` (and all CommitInfos) outlive
    // these maps. Non-UTF-8 paths fall back to "" via unwrap_or_default().
    let mut co_occur: HashMap<(&str, &str), (f64, usize)> = HashMap::new();
    // weighted_total[a] = weighted sum of commits touching a
    let mut weighted_total: HashMap<&str, f64> = HashMap::new();
    let mut large_commits_skipped = 0usize;

    for commit in commits {
        let n = commit.changed_files.len();
        // A commit above the cap adds to neither map: the same commits must
        // make the numerator and the denominator of every confidence.
        if n > COUPLING_MAX_FILES {
            large_commits_skipped += 1;
            continue;
        }
        let files: Vec<&str> = commit
            .changed_files
            .iter()
            .map(|f| f.path.to_str().unwrap_or_default())
            .collect();
        let weight = 1.0 / (n as f64).sqrt();

        // Every commit within the cap adds to the weighted totals of its
        // files, also a single-file commit: it is a real solo change.
        for f in &files {
            *weighted_total.entry(f).or_insert(0.0) += weight;
        }

        if n < 2 {
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

    for entries in blast_radius.values_mut() {
        entries.sort_by(entry_order);
    }

    let mut graph: Vec<CouplingEdge> = graph_edges
        .into_iter()
        .map(|((a, b), (confidence, support))| CouplingEdge {
            a,
            b,
            confidence,
            support,
        })
        .collect();
    graph.sort_by(edge_order);

    CouplingResult {
        blast_radius,
        graph,
        large_commits_skipped,
    }
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
    commits: &[CommitInfo],
    max_churn: usize,
    now_epoch: u64,
) -> HashMap<String, u8> {
    // Per-file commit info
    let mut file_commits: HashMap<String, Vec<u64>> = HashMap::new(); // file -> timestamps
    let mut file_fix_count: HashMap<String, usize> = HashMap::new();

    for commit in commits {
        let is_fix = rskim_search::is_fix_commit(&commit.message);
        for file in &commit.changed_files {
            let path = file.path_str().into_owned();
            if is_fix {
                *file_fix_count.entry(path.clone()).or_insert(0) += 1;
            }
            file_commits
                .entry(path)
                .or_default()
                .push(commit.timestamp.max(0) as u64);
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
pub(crate) fn compute_authors(commits: &[CommitInfo]) -> HashMap<String, AuthorMetrics> {
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
            let single_owner_risk = top_author_pct > BUS_FACTOR_TOP_SHARE_PCT;

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
/// `commits` is newest first, the order `git log` gives, so a later commit
/// has a lower index.
///
/// - `keyword_pct`: percentage of commits touching the file that are fix commits.
/// - `proximity_pct`: percentage of the commits of the file that are non-fix
///   commits followed by a later fix commit, also touching the file, within
///   `window` commits.
/// - `combined_pct`: union (not sum) of the two signals.
/// - `fix_commits`: the count of fix commits touching the file.
/// - `insufficient_data`: true when the file has fewer than
///   [`MIN_FILE_SUPPORT`] commits. The raw rates are filled for every file.
pub(crate) fn compute_fix_after_touch(
    commits: &[CommitInfo],
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
            // A file is in the map only when a commit touched it.
            let total = indices.len();
            debug_assert!(total > 0, "{path}: no commit index");
            let pct = |count: usize| (count as f64 / total.max(1) as f64) * 100.0;

            // Keyword: fix commits touching this file
            let fix_commits = indices.iter().filter(|&&i| is_fix[i]).count();

            // Proximity: a non-fix commit at index idx, followed by a later fix
            // commit (a lower index) that also touches this file, within
            // `window` commits.
            let index_set: HashSet<usize> = indices.iter().copied().collect();
            let proximity_count = indices
                .iter()
                .copied()
                .filter(|&idx| !is_fix[idx])
                .filter(|&idx| {
                    (idx.saturating_sub(window)..idx).any(|j| is_fix[j] && index_set.contains(&j))
                })
                .count();

            // Union: fix commits (keyword) ∪ non-fix commits followed by a fix (proximity).
            // The two sets are disjoint by construction (proximity_count only counts non-fix
            // indices), so union_count = fix_commits + proximity_count.
            (
                path,
                FixRiskMetrics {
                    keyword_pct: pct(fix_commits),
                    proximity_pct: pct(proximity_count),
                    combined_pct: pct(fix_commits + proximity_count),
                    fix_commits,
                    insufficient_data: total < MIN_FILE_SUPPORT,
                },
            )
        })
        .collect()
}

// ============================================================================
// Metric 6: Encapsulation
// ============================================================================

/// File names whose folder is a package root.
const MANIFEST_NAMES: &[&str] = &["Cargo.toml", "package.json"];

/// Package roots for module labels: the folders of tracked manifests.
///
/// Only the manifest paths are used. No manifest content is read and no
/// workspace member glob is evaluated, so invalid manifest content cannot
/// stop a run. With no roots, every path uses the top-folder rule.
#[derive(Debug, Default)]
pub(crate) struct ModuleRoots {
    /// Root folders, repo-relative, with no trailing `/`. A C-quoted
    /// manifest path gives its folder without the outer quotes.
    dirs: HashSet<String>,
}

impl ModuleRoots {
    /// Collect the folder of each `Cargo.toml` or `package.json` in `paths`.
    ///
    /// A manifest in the repository root gives no root: that layout keeps
    /// the top-folder rule.
    pub(crate) fn from_tracked_files(paths: &[String]) -> Self {
        let dirs = paths
            .iter()
            .filter_map(|p| {
                let (dir, name) = without_quotes(p).0.rsplit_once('/')?;
                (!dir.is_empty() && MANIFEST_NAMES.contains(&name)).then(|| dir.to_string())
            })
            .collect();
        Self { dirs }
    }
}

/// Split one pair of outer `"` from a git C-quoted path.
///
/// git writes a path with unusual characters as `"..."` and escapes each such
/// character with `\`; every `/` of the path stays in place in the quoted text.
pub(crate) fn without_quotes(path: &str) -> (&str, bool) {
    match path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
        Some(inner) => (inner, true),
        None => (path, false),
    }
}

/// Return the module label of a repo-relative `path`.
///
/// The deepest package root that holds the path wins; folders match by
/// whole name. A path under no root uses its top folder. A file in the
/// repository root has no module.
pub(crate) fn module_of(path: &str, roots: &ModuleRoots) -> Option<String> {
    let (inner, quoted) = without_quotes(path);
    // Ancestor folders, deepest first: one per `/` of the path.
    let dir = inner
        .rmatch_indices('/')
        .map(|(end, _)| &inner[..end])
        .find(|dir| roots.dirs.contains(*dir))
        .or_else(|| inner.split_once('/').map(|(top, _)| top))
        .filter(|dir| !dir.is_empty())?;
    // A `\` in the folder is an escape of git's quoting, so the label keeps
    // the quotes. A plain folder gets the same label as its unquoted files.
    Some(if quoted && dir.contains('\\') {
        format!("\"{dir}\"")
    } else {
        dir.to_string()
    })
}

/// Compute module encapsulation health.
///
/// For each module (see [`module_of`]), counts commits that touch ONLY that
/// module vs. commits that also touch other modules (cross-boundary).
///
/// Filters modules with fewer than `min_commits` total commits.
/// Returns results sorted by encapsulation_pct ascending (worst first).
pub(crate) fn compute_encapsulation(
    commits: &[CommitInfo],
    min_commits: usize,
    roots: &ModuleRoots,
) -> Vec<ModuleHealth> {
    // module -> files seen
    let mut module_files: HashMap<String, HashSet<String>> = HashMap::new();
    // module -> (total_commits, cross_boundary_commits)
    let mut module_stats: HashMap<String, (usize, usize)> = HashMap::new();

    for commit in commits {
        // Map each file to its module once; both the module set of the
        // commit and the per-module file sets use the result.
        let file_dirs: Vec<Option<String>> = commit
            .changed_files
            .iter()
            .map(|f| module_of(&f.path_str(), roots))
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::FileChangeInfo;

    fn make_commit(hash: &str, author: &str, ts: u64, subject: &str, files: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: hash.to_string(),
            author: author.to_string(),
            timestamp: ts as i64,
            message: subject.to_string(),
            changed_files: files
                .iter()
                .map(|p| FileChangeInfo {
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
        let result = compute_coupling(&[], 0.5, 1);
        assert!(result.blast_radius.is_empty());
        assert!(result.graph.is_empty());
        assert_eq!(result.large_commits_skipped, 0);
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
        let result = compute_coupling(&commits, 0.5, 1);
        // confidence ~0.261 < 0.5, so no coupling entries for a.rs
        let a_entries = result
            .blast_radius
            .get("a.rs")
            .map(|v| v.len())
            .unwrap_or(0);
        assert_eq!(a_entries, 0);
    }

    #[test]
    fn test_coupling_below_min_support_excluded() {
        let commits = vec![make_commit("h1", "Alice", 1000, "msg", &["a.rs", "b.rs"])];
        let result = compute_coupling(&commits, 0.0, 5); // min_support=5, only 1 commit
        assert_eq!(
            result
                .blast_radius
                .get("a.rs")
                .map(|v| v.len())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn test_coupling_strong_pair() {
        // a.rs and b.rs always appear together
        let commits = vec![
            make_commit("h1", "A", 1, "m", &["a.rs", "b.rs"]),
            make_commit("h2", "A", 2, "m", &["a.rs", "b.rs"]),
            make_commit("h3", "A", 3, "m", &["a.rs", "b.rs"]),
        ];
        let result = compute_coupling(&commits, 0.5, 3);
        let a_entries = result.blast_radius.get("a.rs").unwrap();
        assert_eq!(a_entries.len(), 1);
        assert_eq!(a_entries[0].path, "b.rs");
        assert_eq!(a_entries[0].support, 3);
        assert_eq!(result.graph.len(), 1);
    }

    /// `n` file names `bulk_0.rs` .. `bulk_{n-1}.rs`, plus the `extra` names.
    fn file_names(n: usize, extra: &[&str]) -> Vec<String> {
        extra
            .iter()
            .map(|s| s.to_string())
            .chain((0..n).map(|i| format!("bulk_{i}.rs")))
            .collect()
    }

    #[test]
    fn test_coupling_large_commit_excluded_from_pairs() {
        // A commit with COUPLING_MAX_FILES + 1 files makes no pairs, adds to no
        // weighted total, and is counted once.
        let many_files = file_names(COUPLING_MAX_FILES + 1, &[]);
        let file_refs: Vec<&str> = many_files.iter().map(String::as_str).collect();
        let commits = vec![make_commit("h1", "A", 1, "m", &file_refs)];

        // With threshold=0.0 and min_support=1, any pair would be included if generated.
        let result = compute_coupling(&commits, 0.0, 1);
        assert!(
            result.blast_radius.is_empty(),
            "large commit must not generate coupling pairs"
        );
        assert!(
            result.graph.is_empty(),
            "large commit must not generate graph edges"
        );
        assert_eq!(result.large_commits_skipped, 1);
    }

    #[test]
    fn test_coupling_commit_at_cap_is_paired() {
        // Exactly COUPLING_MAX_FILES files is not above the cap: all pairs count.
        let files = file_names(COUPLING_MAX_FILES, &[]);
        let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let commits = vec![make_commit("h1", "A", 1, "m", &file_refs)];
        let result = compute_coupling(&commits, 0.0, 1);
        assert_eq!(result.large_commits_skipped, 0);
        assert_eq!(
            result.graph.len(),
            COUPLING_MAX_FILES * (COUPLING_MAX_FILES - 1) / 2
        );
    }

    /// Confidence of a.rs -> b.rs, or `None` when the pair is absent.
    fn confidence_a_to_b(result: &CouplingResult) -> Option<f64> {
        result
            .blast_radius
            .get("a.rs")?
            .iter()
            .find(|e| e.path == "b.rs")
            .map(|e| e.confidence)
    }

    #[test]
    fn test_coupling_large_commit_does_not_change_pair_confidence() {
        // Newest first: 3 pair commits and 1 solo commit of a.rs.
        let small = vec![
            make_commit("h4", "A", 4, "m", &["a.rs"]),
            make_commit("h3", "A", 3, "m", &["a.rs", "b.rs"]),
            make_commit("h2", "A", 2, "m", &["a.rs", "b.rs"]),
            make_commit("h1", "A", 1, "m", &["a.rs", "b.rs"]),
        ];
        // The same history plus one 60-file commit that touches both files.
        let big_files = file_names(58, &["a.rs", "b.rs"]);
        let big_refs: Vec<&str> = big_files.iter().map(String::as_str).collect();
        let mut with_large = vec![make_commit("h5", "A", 5, "m", &big_refs)];
        with_large.extend(small.iter().cloned());

        let without = compute_coupling(&small, 0.0, 1);
        let with = compute_coupling(&with_large, 0.0, 1);

        let expected = confidence_a_to_b(&without).expect("pair present without it");
        let got = confidence_a_to_b(&with).expect("pair present with it");
        assert!(
            (got - expected).abs() < 1e-12,
            "confidence with the 60-file commit ({got}) must equal the confidence without it ({expected})"
        );
        assert_eq!(without.large_commits_skipped, 0);
        assert_eq!(with.large_commits_skipped, 1);
    }

    #[test]
    fn test_coupling_ties_sort_by_path_and_by_edge_ends() {
        // a.rs pairs 3 times with each of e, c, d, b: equal confidence and support.
        let mut commits = Vec::new();
        for (i, partner) in ["e.rs", "c.rs", "d.rs", "b.rs"].iter().enumerate() {
            for k in 0..3u64 {
                let ts = (i as u64) * 10 + k;
                commits.push(make_commit("h", "A", ts, "m", &["a.rs", partner]));
            }
        }
        let result = compute_coupling(&commits, 0.0, 3);

        let partners: Vec<&str> = result.blast_radius["a.rs"]
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(partners, vec!["b.rs", "c.rs", "d.rs", "e.rs"]);

        let edges: Vec<(&str, &str)> = result
            .graph
            .iter()
            .map(|e| (e.a.as_str(), e.b.as_str()))
            .collect();
        assert_eq!(
            edges,
            vec![
                ("a.rs", "b.rs"),
                ("a.rs", "c.rs"),
                ("a.rs", "d.rs"),
                ("a.rs", "e.rs"),
            ]
        );
    }

    #[test]
    fn test_entry_order_is_confidence_then_support_then_path() {
        let entry = |path: &str, confidence: f64, support: usize| CouplingEntry {
            path: path.to_string(),
            confidence,
            support,
        };
        let mut entries = [
            entry("z.rs", 0.5, 3),
            entry("b.rs", 0.9, 3),
            entry("y.rs", 0.5, 7),
            entry("a.rs", 0.5, 3),
        ];
        entries.sort_by(entry_order);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["b.rs", "y.rs", "a.rs", "z.rs"]);
    }

    #[test]
    fn test_edge_order_is_confidence_then_support_then_a_then_b() {
        let edge = |a: &str, b: &str, confidence: f64, support: usize| CouplingEdge {
            a: a.to_string(),
            b: b.to_string(),
            confidence,
            support,
        };
        let mut edges = [
            edge("b.rs", "c.rs", 0.5, 3),
            edge("a.rs", "d.rs", 0.5, 3),
            edge("a.rs", "c.rs", 0.5, 3),
            edge("x.rs", "y.rs", 0.5, 9),
            edge("z.rs", "w.rs", 0.9, 3),
        ];
        edges.sort_by(edge_order);
        let pairs: Vec<(&str, &str)> = edges.iter().map(|e| (e.a.as_str(), e.b.as_str())).collect();
        assert_eq!(
            pairs,
            vec![
                ("z.rs", "w.rs"),
                ("x.rs", "y.rs"),
                ("a.rs", "c.rs"),
                ("a.rs", "d.rs"),
                ("b.rs", "c.rs"),
            ]
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
            (50..=70).contains(&score),
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

    // All commit lists below are newest first, the order `git log` gives.

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
    fn test_fix_risk_single_fix_commit_keeps_raw_keyword_rate() {
        // One fix commit: below support, but the raw rate and churn stay.
        let commits = vec![make_commit("h1", "Alice", 1, "fix: crash", &["a.rs"])];
        let m = &compute_fix_after_touch(&commits, 5)["a.rs"];
        assert!(m.insufficient_data);
        assert!(
            (m.keyword_pct - 100.0).abs() < 1e-9,
            "keyword_pct {}",
            m.keyword_pct
        );
        assert_eq!(m.fix_commits, 1);
        assert_eq!(compute_churn(&commits)["a.rs"].commits, 1);
    }

    #[test]
    fn test_fix_risk_support_minimum_is_five_commits() {
        let history = |n: u64| -> Vec<CommitInfo> {
            (0..n)
                .rev()
                .map(|i| make_commit("h", "Alice", i, "feat: work", &["a.rs"]))
                .collect()
        };
        assert!(compute_fix_after_touch(&history(4), 5)["a.rs"].insufficient_data);
        assert!(!compute_fix_after_touch(&history(5), 5)["a.rs"].insufficient_data);
    }

    #[test]
    fn test_fix_risk_keyword_detection() {
        let commits = vec![
            make_commit("h2", "Alice", 2, "fix: bug in a", &["a.rs"]),
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
        ];
        let result = compute_fix_after_touch(&commits, 5);
        let m = &result["a.rs"];
        // 1 fix commit out of 2 = 50%
        assert!((m.keyword_pct - 50.0).abs() < 1e-9);
        assert_eq!(m.fix_commits, 1);
    }

    #[test]
    fn test_fix_risk_proximity_counts_feat_followed_by_later_fix() {
        // feat at ts 1, then a later fix at ts 2 on the same file.
        let commits = vec![
            make_commit("h2", "Alice", 2, "fix: quick patch", &["a.rs"]),
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
        ];
        let m = &compute_fix_after_touch(&commits, 5)["a.rs"];
        // total=2, the feat is followed by a fix: proximity_pct = 1/2*100 = 50%
        assert!(
            (m.proximity_pct - 50.0).abs() < 1e-9,
            "proximity {}",
            m.proximity_pct
        );
    }

    #[test]
    fn test_fix_risk_proximity_ignores_feat_after_fix() {
        // fix at ts 1, then a later feat at ts 2: no fix follows the feat.
        let commits = vec![
            make_commit("h2", "Alice", 2, "feat: extend", &["a.rs"]),
            make_commit("h1", "Alice", 1, "fix: quick patch", &["a.rs"]),
        ];
        let m = &compute_fix_after_touch(&commits, 5)["a.rs"];
        assert!(
            m.proximity_pct.abs() < 1e-9,
            "proximity {}",
            m.proximity_pct
        );
    }

    #[test]
    fn test_fix_risk_proximity_respects_window() {
        // feat at ts 1, a commit on another file at ts 2, the fix at ts 3.
        let commits = vec![
            make_commit("h3", "Alice", 3, "fix: repair", &["a.rs"]),
            make_commit("h2", "Alice", 2, "feat: other", &["b.rs"]),
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
        ];
        // The fix is 2 commits later: outside a window of 1, inside a window of 2.
        let narrow = &compute_fix_after_touch(&commits, 1)["a.rs"];
        assert!(
            narrow.proximity_pct.abs() < 1e-9,
            "proximity {}",
            narrow.proximity_pct
        );
        let wide = &compute_fix_after_touch(&commits, 2)["a.rs"];
        assert!(
            (wide.proximity_pct - 50.0).abs() < 1e-9,
            "proximity {}",
            wide.proximity_pct
        );
    }

    #[test]
    fn test_fix_risk_combined_both_signals() {
        // ts 1 feat (idx 2) → a later fix follows within the window: proximity hit
        // ts 2 fix  (idx 1) → keyword hit
        // ts 3 feat (idx 0) → newest; no later fix
        //
        // Expected for a.rs (total=3):
        //   keyword_count   = 1 → keyword_pct   = 1/3*100 ≈ 33.33%
        //   proximity_count = 1 → proximity_pct = 1/3*100 ≈ 33.33%
        //   union_count     = 2 → combined_pct  = 2/3*100 ≈ 66.67%
        let commits = vec![
            make_commit("h3", "Alice", 3, "feat: more work", &["a.rs"]),
            make_commit("h2", "Alice", 2, "fix: bug in a", &["a.rs"]),
            make_commit("h1", "Alice", 1, "feat: add", &["a.rs"]),
        ];
        let result = compute_fix_after_touch(&commits, 5);
        let m = &result["a.rs"];

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
        let result = compute_encapsulation(&[], 1, &ModuleRoots::default());
        assert!(result.is_empty());
    }

    #[test]
    fn test_encapsulation_single_module() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["src/a.rs", "src/b.rs"]),
            make_commit("h2", "Alice", 2, "m", &["src/c.rs"]),
            make_commit("h3", "Alice", 3, "m", &["src/d.rs"]),
        ];
        let result = compute_encapsulation(&commits, 1, &ModuleRoots::default());
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
        let result = compute_encapsulation(&commits, 1, &ModuleRoots::default());
        let src = result.iter().find(|m| m.path == "src").unwrap();
        // 1 cross-boundary out of 3 → 66.7% internal → encapsulation = 66.7%
        assert!(src.encapsulation_pct < 100.0);
        assert_eq!(src.cross_boundary_commits, 1);
    }

    #[test]
    fn test_encapsulation_min_commits_filter() {
        let commits = vec![make_commit("h1", "Alice", 1, "m", &["src/a.rs"])];
        let result = compute_encapsulation(&commits, 3, &ModuleRoots::default()); // requires ≥3 commits
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
        let result = compute_encapsulation(&commits, 1, &ModuleRoots::default());
        // Sorted ascending by encapsulation_pct
        for window in result.windows(2) {
            assert!(window[0].encapsulation_pct <= window[1].encapsulation_pct);
        }
    }

    #[test]
    fn test_encapsulation_counts_commit_across_two_crates_as_cross_boundary() {
        let commits = vec![
            make_commit("h1", "Alice", 1, "m", &["crates/a/x.rs", "crates/b/y.rs"]),
            make_commit("h2", "Alice", 2, "m", &["crates/a/z.rs"]),
        ];
        let roots = roots_from(&["crates/a/Cargo.toml", "crates/b/Cargo.toml"]);

        let by_crate = compute_encapsulation(&commits, 1, &roots);
        let a = by_crate.iter().find(|m| m.path == "crates/a").unwrap();
        assert_eq!((a.total_commits, a.cross_boundary_commits), (2, 1));
        assert_eq!(by_crate.len(), 2, "modules: {by_crate:?}");

        // The top-folder rule sees one module and no boundary.
        let by_top = compute_encapsulation(&commits, 1, &ModuleRoots::default());
        assert_eq!(by_top.len(), 1, "modules: {by_top:?}");
        assert_eq!(by_top[0].path, "crates");
        assert_eq!(by_top[0].cross_boundary_commits, 0);
    }

    // -----------------------------------------------------------------------
    // module_of
    // -----------------------------------------------------------------------

    fn roots_from(paths: &[&str]) -> ModuleRoots {
        let owned: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        ModuleRoots::from_tracked_files(&owned)
    }

    #[test]
    fn test_module_of_root_file_has_no_module() {
        let roots = roots_from(&["Cargo.toml", "crates/a/Cargo.toml"]);
        assert_eq!(module_of("Makefile", &roots), None);
        assert_eq!(module_of("README.md", &ModuleRoots::default()), None);
    }

    #[test]
    fn test_module_of_without_roots_uses_top_folder() {
        let roots = ModuleRoots::default();
        assert_eq!(module_of("src/lib.rs", &roots).as_deref(), Some("src"));
        assert_eq!(
            module_of("src/cmd/heatmap/mod.rs", &roots).as_deref(),
            Some("src")
        );
        assert_eq!(module_of("tests/foo.rs", &roots).as_deref(), Some("tests"));
    }

    #[test]
    fn test_module_of_maps_files_to_their_manifest_folder() {
        let roots = roots_from(&[
            "crates/a/Cargo.toml",
            "crates/a/src/lib.rs",
            "crates/b/Cargo.toml",
            "packages/x/package.json",
        ]);
        assert_eq!(
            module_of("crates/a/src/lib.rs", &roots).as_deref(),
            Some("crates/a")
        );
        assert_eq!(
            module_of("crates/b/Cargo.toml", &roots).as_deref(),
            Some("crates/b")
        );
        assert_eq!(
            module_of("packages/x/index.js", &roots).as_deref(),
            Some("packages/x")
        );
        // A path under no package root keeps the top-folder rule.
        assert_eq!(
            module_of("crates/README.md", &roots).as_deref(),
            Some("crates")
        );
        assert_eq!(module_of("src/main.rs", &roots).as_deref(), Some("src"));
    }

    #[test]
    fn test_module_of_root_manifest_keeps_top_folder_rule() {
        let roots = roots_from(&["Cargo.toml", "package.json", "src/main.rs"]);
        assert_eq!(
            module_of("crates/a/src/lib.rs", &roots).as_deref(),
            Some("crates")
        );
        assert_eq!(module_of("src/main.rs", &roots).as_deref(), Some("src"));
    }

    #[test]
    fn test_module_of_deepest_manifest_folder_wins() {
        let roots = roots_from(&["crates/a/Cargo.toml", "crates/a/fuzz/Cargo.toml"]);
        assert_eq!(
            module_of("crates/a/fuzz/src/main.rs", &roots).as_deref(),
            Some("crates/a/fuzz")
        );
        assert_eq!(
            module_of("crates/a/src/lib.rs", &roots).as_deref(),
            Some("crates/a")
        );
        // Folders match by whole name: `crates/ab` is not inside `crates/a`.
        assert_eq!(
            module_of("crates/ab/x.rs", &roots).as_deref(),
            Some("crates")
        );
    }

    /// git C-quotes a path with non-ASCII characters in both `ls-tree` and
    /// `--numstat` output; a quoted crate folder still holds its own files.
    #[test]
    fn test_module_of_quoted_paths_match_their_quoted_manifest_folder() {
        let roots = roots_from(&[r#""crates/na\303\257ve/Cargo.toml""#]);
        assert_eq!(
            module_of(r#""crates/na\303\257ve/src/lib.rs""#, &roots).as_deref(),
            Some(r#""crates/na\303\257ve""#)
        );
        // A quoted file in a plain folder gets the plain folder name.
        assert_eq!(
            module_of(r#""src/na\303\257ve.rs""#, &roots).as_deref(),
            Some("src")
        );
    }
}
