//! Compound multi-layer query composition (#198).
//!
//! Owns the `compound/` sub-tree.  #198 is the sole author of this directory;
//! #200 extends it additively (new files, not modifications).
//!
//! # Public surface
//!
//! - [`intersection`] — core intersection + weighted-RRF fusion module.
//! - Re-exports of the key types/functions consumed by the CLI layer.

pub mod intersection;

pub use intersection::{
    CompositeWeights, RRF_K, WEIGHT_AST, WEIGHT_LEXICAL, intersect_and_rank, recompose_with_lexical,
};
