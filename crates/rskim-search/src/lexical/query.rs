//! Query engine: `SearchLayer` + `SearchIndex` implementation for the lexical layer.
//!
//! Opens a persistent index, executes n-gram lookups, intersects posting lists,
//! scores documents via BM25F, and returns ranked results.
