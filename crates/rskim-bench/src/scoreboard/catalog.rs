//! skim's `--ast` pattern catalog as the scoreboard sees it (#541).
//!
//! [`skim_catalog`] is the scoreboard's one read of
//! `rskim_search::all_patterns`. It projects each entry to its name, `exact`
//! flag and example snippet, never the n-gram tables that encode how skim
//! matches the pattern. The pipeline (`Inputs::load`) and `golden-gen --ast`
//! call it once and pass the result to everything that needs the catalog.
//!
//! The structural oracle (`rskim_oracle::structural`) never sees the catalog:
//! it knows its own registry by pattern name, and [`catalog_coverage`]
//! crosses the two, so a catalog pattern the oracle has no query for is listed
//! as uncovered instead of silently skipped.

use std::collections::BTreeMap;

use rskim_oracle::structural::{PatternCoverage, coverage_of};

/// One entry of skim's pattern catalog, as the scoreboard may see it. The
/// n-gram tables that encode how skim matches the pattern are left out, so
/// no oracle answer or score can depend on them (AC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogPattern {
    /// The `--ast` pattern name.
    pub name: &'static str,
    /// skim's `exact` flag, from which `golden-gen` proposes an entry's
    /// precision class. No oracle answer or score reads it.
    pub exact: bool,
    /// The catalog's example snippet, which the oracle's fixture tests match.
    pub example: &'static str,
}

/// skim's pattern catalog projected to [`CatalogPattern`], in catalog order:
/// the scoreboard's only read of `rskim_search::all_patterns`.
pub fn skim_catalog() -> Vec<CatalogPattern> {
    rskim_search::all_patterns()
        .iter()
        .map(|p| CatalogPattern {
            name: p.name,
            exact: p.exact,
            example: p.example,
        })
        .collect()
}

/// The structural oracle's coverage of every pattern in `catalog`, keyed by
/// pattern name. A pattern the oracle has neither a query nor a recorded
/// reason for reads as `UNCLASSIFIED_REASON`, never as absent.
pub fn catalog_coverage(catalog: &[CatalogPattern]) -> BTreeMap<&'static str, PatternCoverage> {
    catalog
        .iter()
        .map(|p| (p.name, coverage_of(p.name)))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly
#[path = "catalog_tests.rs"]
mod tests;
