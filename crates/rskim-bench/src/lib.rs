//! BM25F parameter tuning benchmark harness for rskim-search.
//!
//! # Overview
//!
//! This crate provides tools to empirically measure and tune BM25F scoring
//! parameters against real source code corpora using IR metrics (MRR, P@K).
//!
//! # Modules
//!
//! - [`configs`]  — named BM25F configurations for comparison
//! - [`extract`]  — language-specific AST symbol extractors
//! - [`harness`]  — orchestrates index build → qrel eval per repo
//! - [`metrics`]  — pure IR metric functions (MRR, Precision@K)
//! - [`qrel`]     — relevance judgment generation from symbol extraction
//! - [`report`]   — JSON and Markdown report generation
//! - [`split`]    — deterministic train/test split
//! - [`tuning`]   — coordinate descent parameter search
//! - [`types`]    — shared data types

pub mod cochange;
pub mod configs;
pub mod extract;
pub mod harness;
pub mod metrics;
pub mod qrel;
pub mod report;
pub mod split;
pub mod tuning;
pub mod types;
