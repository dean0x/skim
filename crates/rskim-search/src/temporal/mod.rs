//! Temporal analysis layer: git-aware signals for search.
//!
//! # Architecture
//!
//! The temporal layer mines git history for:
//! - **Co-change patterns** — files that historically change together ([blast radius](crate::TemporalQuery::blast_radius))
//! - **Hotspots** — files with recent commit activity ([hotspots](crate::TemporalQuery::hotspots))
//! - **Coldspots** — files not recently changed ([coldspots](crate::TemporalQuery::coldspots))
//! - **Risk** — files with high fix-commit density ([risky](crate::TemporalQuery::risky))
//!
//! # Decoupling from lexical
//!
//! Temporal data uses its own internal path table so that files in git
//! history but absent from the current lexical index are still tracked. The
//! CLI joins temporal and lexical results at query time by comparing paths.
//!
//! # Storage
//!
//! All temporal data is persisted to SQLite at
//! `~/.cache/skim/search/<repo-hash>/temporal.db`, alongside the lexical
//! index files.

pub mod cochange;
pub mod git_parser;
pub mod query;
pub mod scoring;
pub mod storage;
pub mod types;

pub use cochange::build_cochange_matrix;
pub use git_parser::parse_history;
pub use query::TemporalIndex;
pub use scoring::{hotspot_scores, risk_scores};
pub use storage::{ScoreKind, TemporalDb, DEFAULT_LOOKBACK_DAYS};
pub use types::{CochangeEntry, CommitInfo, HotspotScore, RiskScore};
