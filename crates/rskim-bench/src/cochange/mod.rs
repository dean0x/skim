//! Co-change validation benchmark.
//!
//! Measures the precision/recall of blast-radius predictions against actual
//! PR file sets from OSS repositories, establishing baseline metrics for
//! Jaccard threshold tuning.
//!
//! # Modules
//!
//! - [`types`]          — shared result types
//! - [`deny_list`]      — lock-file and generated-file exclusions
//! - [`temporal_split`] — chronological train/test split
//! - [`validate`]       — core evaluation pipeline
//! - [`report`]         — JSON and Markdown output

pub mod deny_list;
pub mod report;
pub mod temporal_split;
pub mod types;
pub mod validate;
