//! The `-g` and `-t` filters of `skim search`.
//!
//! A filter only decides about a repo-relative path that the universe already
//! lists. It never opens a path, never joins a glob to the root and never sets
//! a walk root: an absolute glob or a glob with `..` just fits nothing.
//!
//! The glob and type rules are those of `rg`, from the `ignore` crate
//! (`ignore::overrides` and the default type table).

use std::path::Path;

use ignore::overrides::{Override, OverrideBuilder};
use ignore::types::{Types, TypesBuilder};

/// Why a filter cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FilterError {
    /// A `-g` value that is not a valid glob.
    Glob { glob: String, detail: String },
    /// A `-t` value that names no known type.
    UnknownType { name: String, known: Vec<String> },
    /// The type table cannot be built for another reason.
    Types(String),
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterError::Glob { glob, detail } => write!(f, "invalid glob {glob:?}: {detail}"),
            FilterError::UnknownType { name, known } => {
                write!(
                    f,
                    "unknown file type {name:?}; known types: {}",
                    known.join(", ")
                )
            }
            FilterError::Types(detail) => write!(f, "cannot build the file types: {detail}"),
        }
    }
}

impl std::error::Error for FilterError {}

/// The `-g` and `-t` filters of one query.
pub(super) struct PathFilter {
    globs: Option<Override>,
    types: Option<Types>,
}

impl PathFilter {
    /// Build the filter from the `-g` and `-t` values.
    ///
    /// # Errors
    ///
    /// [`FilterError`] for an invalid glob or an unknown type.
    pub(super) fn new(
        root: &Path,
        globs: &[String],
        types: &[String],
    ) -> Result<Self, FilterError> {
        let globs = if globs.is_empty() {
            None
        } else {
            let mut builder = OverrideBuilder::new(root);
            for glob in globs {
                builder.add(glob).map_err(|e| FilterError::Glob {
                    glob: glob.clone(),
                    detail: e.to_string(),
                })?;
            }
            Some(builder.build().map_err(|e| FilterError::Glob {
                glob: globs.join(" "),
                detail: e.to_string(),
            })?)
        };
        let types = if types.is_empty() {
            None
        } else {
            let mut builder = TypesBuilder::new();
            builder.add_defaults();
            for name in types {
                builder.select(name);
            }
            let built = builder.build().map_err(|e| match e {
                ignore::Error::UnrecognizedFileType(name) => {
                    let mut known: Vec<String> = builder
                        .definitions()
                        .iter()
                        .map(|def| def.name().to_owned())
                        .collect();
                    known.sort();
                    FilterError::UnknownType { name, known }
                }
                other => FilterError::Types(other.to_string()),
            })?;
            Some(built)
        };
        Ok(PathFilter { globs, types })
    }

    /// Whether the file `rel` (relative to the root) passes both filters.
    ///
    /// A glob that leaves a folder out leaves out every file below it. A glob
    /// that only whitelists is decided on the file itself.
    pub(super) fn keeps(&self, rel: &Path) -> bool {
        if let Some(globs) = &self.globs {
            let mut folders: Vec<&Path> = rel
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .collect();
            folders.reverse();
            if folders
                .iter()
                .any(|folder| globs.matched(folder, true).is_ignore())
            {
                return false;
            }
            if globs.matched(rel, false).is_ignore() {
                return false;
            }
        }
        if let Some(types) = &self.types
            && types.matched(rel, false).is_ignore()
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
