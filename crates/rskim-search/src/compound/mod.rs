//! Compound multi-layer query composition (#198, extended by #200).
//!
//! Owns the `compound/` sub-tree.  #198 is the sole author of `intersection`;
//! #200 extends it additively via new modules (no modifications to intersection).
//!
//! # Public surface
//!
//! ## #198 modules (intersection + RRF for 2-signal lexical+AST blend)
//! - [`intersection`] — core intersection + weighted-RRF fusion (2-signal).
//!
//! ## #200 modules (N-signal UNION fusion + structural signals)
//! - [`weights`] — `CompositeWeights6` (all 6 signals) + `validate()`.
//! - [`merge`] — N-signal weighted-RRF over the UNION of all FileId sets.
//! - [`proximity`] — directory-proximity pairwise signal.
//! - [`import_graph`] — import/use/require edge extraction signal.
//! - [`coupling`] — structural-coupling scaffold (deferred to #336).

pub mod coupling;
pub mod import_graph;
pub mod intersection;
pub mod merge;
pub mod proximity;
pub mod weights;

pub use coupling::structural_coupling_score;
pub use import_graph::{ImportGraph, ImportLanguage};
pub use intersection::{
    CompositeWeights, RRF_K, WEIGHT_AST, WEIGHT_LEXICAL, intersect_and_rank, recompose_with_lexical,
};
pub use merge::{merge_composite, merge_layer_scores};
pub use proximity::dir_proximity_score;
pub use weights::{
    CompositeWeights6, WEIGHT6_AST, WEIGHT6_DIR_PROXIMITY, WEIGHT6_IMPORT_GRAPH, WEIGHT6_LEXICAL,
    WEIGHT6_STRUCTURAL_COUPLING, WEIGHT6_TEMPORAL,
};
