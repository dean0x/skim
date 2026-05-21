//! Deterministic train/test split via SHA-256 hash.
//!
//! Uses the query string as the key so split assignment is reproducible across
//! runs, machines, and orderings — it depends only on the query text, not on
//! the slice index.
//!
//! Assignment: `sha256(query)[0] % 5`
//! - 0, 1, 2 → train (60%)
//! - 3, 4    → test  (40%)

use sha2::{Digest, Sha256};

/// Whether a query belongs to the train or test split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    Train,
    Test,
}

/// Deterministically assign a query to train or test.
///
/// The assignment is a pure function of `query` — calling this function with
/// the same query always returns the same `Split`.
#[must_use]
pub fn assign_split(query: &str) -> Split {
    let mut hasher = Sha256::new();
    hasher.update(query.as_bytes());
    let hash = hasher.finalize();
    let bucket = hash[0] % 5;
    if bucket < 3 {
        Split::Train
    } else {
        Split::Test
    }
}

/// Partition a slice of items into (train, test) using `assign_split`.
///
/// Returns two owned `Vec`s. Items are cloned from the input slice.
pub fn partition<T: Clone, F>(items: &[T], key_fn: F) -> (Vec<T>, Vec<T>)
where
    F: Fn(&T) -> &str,
{
    let mut train = Vec::new();
    let mut test = Vec::new();
    for item in items {
        match assign_split(key_fn(item)) {
            Split::Train => train.push(item.clone()),
            Split::Test => test.push(item.clone()),
        }
    }
    (train, test)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code — unwrap acceptable for test assertions
mod tests {
    use super::*;

    #[test]
    fn split_is_deterministic_single_query() {
        let query = "calculate_sum";
        let first = assign_split(query);
        // Call 100 times — all must be the same
        for _ in 0..100 {
            assert_eq!(assign_split(query), first, "split should be deterministic");
        }
    }

    #[test]
    fn split_deterministic_across_100_distinct_queries() {
        // Build 100 distinct queries and verify determinism
        let queries: Vec<String> = (0..100).map(|i| format!("query_symbol_{i}")).collect();
        let first_results: Vec<Split> = queries.iter().map(|q| assign_split(q)).collect();

        for _ in 0..5 {
            for (q, &expected) in queries.iter().zip(first_results.iter()) {
                assert_eq!(assign_split(q), expected);
            }
        }
    }

    #[test]
    fn split_ratio_is_approximately_60_40() {
        // Generate enough queries that the law of large numbers gives ~60/40
        let queries: Vec<String> = (0..1000).map(|i| format!("sym_func_{i}")).collect();
        let train_count = queries
            .iter()
            .filter(|q| assign_split(q) == Split::Train)
            .count();
        let test_count = queries.len() - train_count;

        let train_pct = train_count as f64 / queries.len() as f64;
        let test_pct = test_count as f64 / queries.len() as f64;

        // Allow 10% tolerance around the target ratio
        assert!(
            (0.50..=0.70).contains(&train_pct),
            "train ratio should be ~60%, got {:.1}%",
            train_pct * 100.0
        );
        assert!(
            (0.30..=0.50).contains(&test_pct),
            "test ratio should be ~40%, got {:.1}%",
            test_pct * 100.0
        );
    }

    #[test]
    fn partition_preserves_all_items() {
        let items: Vec<String> = (0..20).map(|i| format!("sym_{i}")).collect();
        let (train, test) = partition(&items, |s| s.as_str());
        assert_eq!(train.len() + test.len(), items.len());
    }

    #[test]
    fn partition_empty_input() {
        let items: Vec<String> = vec![];
        let (train, test) = partition(&items, |s| s.as_str());
        assert!(train.is_empty());
        assert!(test.is_empty());
    }
}
