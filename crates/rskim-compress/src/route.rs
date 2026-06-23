//! Class → engine dispatch and language-hint routing table (#304 Phase 2).
//!
//! # AD-006 — Data-format routing precedence
//!
//! `language_hint == "json"` → JSON engine, NEVER the rskim-core code arm.
//! Reason: `rskim_core::Language::Json`'s `transform_json` emits non-valid JSON
//! (unquoted keys, non-standard syntax) — this was verified at
//! `rskim-core/src/transform/json.rs:4-31`. The JSON engine in this crate
//! always emits valid, re-parseable JSON (AC5 / D5).
//!
//! `language_hint` in {`yaml`, `toml`, `markdown`} → byte-identical passthrough.
//! Reason: rskim-core's YAML/TOML/Markdown transforms are file-skimming renderings
//! with no re-emittable-validity guarantee for embedded content blocks.
//!
//! All other recognized language hints → code arm (rskim-core AST transform).
//!
//! # AD-012 — `unknown` is the extension point
//!
//! Adding a future class (e.g., HTML, diff) requires only a new `match` arm;
//! existing arms are unaffected. The mapping table (`hint_to_language`) is the
//! ONLY source of routing — no other module may hard-code language-hint logic.

use rskim_core::Language;

/// The engine a block should be routed to.
///
/// Returned by [`engine_for_class`] and [`hint_to_language`]. Phase 3's
/// `BlockRouter::route` dispatches on this value to select the compressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineTarget {
    /// Route to the code engine (`engines/code.rs`).
    Code(Language),
    /// Route to the JSON engine (`engines/json.rs`).
    Json,
    /// Route to the log engine (`engines/log.rs`).
    Log,
    /// Route to the mixed-content fence scanner (`engines/mixed.rs`).
    Mixed,
    /// Forward byte-identical; no compressor invoked.
    Passthrough,
}

/// Map a `Class` + optional `language_hint` to an engine target.
///
/// This is the SINGLE committed routing table (AD-006 / AD-012). Every
/// routing decision flows through this function — no other site may
/// hard-code class→engine mappings.
///
/// # Class routing order
///
/// 1. `Class::Code` → look up `language_hint` in [`hint_to_language`].
///    - `"json"` hint → [`EngineTarget::Json`] (never Code; AD-006).
///    - Data-format hints (`yaml`, `toml`, `markdown`) → [`EngineTarget::Passthrough`].
///    - Other known hints → [`EngineTarget::Code`] with the mapped `Language`.
///    - No hint / unknown hint → [`EngineTarget::Passthrough`] (AC2).
/// 2. `Class::Json` → [`EngineTarget::Json`].
/// 3. `Class::Log` → [`EngineTarget::Log`].
/// 4. `Class::Mixed` → [`EngineTarget::Mixed`] (fence scanner in `engines/mixed.rs`).
/// 5. `Class::Text` / `Class::Unknown` → [`EngineTarget::Passthrough`] (AC7).
pub(crate) fn engine_for_class(
    class: rskim_llm::Class,
    language_hint: Option<&str>,
) -> EngineTarget {
    use rskim_llm::Class;

    match class {
        Class::Code => match language_hint.and_then(hint_to_language) {
            Some(HintTarget::Language(lang)) => EngineTarget::Code(lang),
            Some(HintTarget::Json) => EngineTarget::Json,
            Some(HintTarget::Passthrough) | None => EngineTarget::Passthrough,
        },
        Class::Json => EngineTarget::Json,
        Class::Log => EngineTarget::Log,
        Class::Mixed => EngineTarget::Mixed,
        // AD-012: Text and Unknown are passthrough; the Unknown arm is the extension point.
        Class::Text | Class::Unknown => EngineTarget::Passthrough,
    }
}

/// Internal result of hint → engine mapping.
enum HintTarget {
    /// Route to the code engine with this language.
    Language(Language),
    /// Route to the JSON engine (special-cased per AD-006).
    Json,
    /// Forward byte-identical (data-format or unknown hint).
    Passthrough,
}

/// Map a fence language hint string to an engine target.
///
/// This is the ONLY source of hint → engine routing (AD-006). All hints are
/// lowercased for matching; case-variants from the classifier are normalized.
///
/// ## Mapping table (single committed source, AD-006)
///
/// | Hint | Target | Reason |
/// |------|--------|--------|
/// | `json` | Json engine | rskim-core Json emits non-valid JSON (verified) |
/// | `yaml` | Passthrough | No re-emittable-validity guarantee |
/// | `toml` | Passthrough | No re-emittable-validity guarantee |
/// | `markdown` / `md` | Passthrough | No re-emittable-validity guarantee |
/// | `rust` | Code(Rust) | tree-sitter supported |
/// | `typescript` / `ts` | Code(TypeScript) | tree-sitter supported |
/// | `javascript` / `js` | Code(JavaScript) | tree-sitter supported |
/// | `python` / `py` | Code(Python) | tree-sitter supported |
/// | `go` / `golang` | Code(Go) | tree-sitter supported |
/// | `java` | Code(Java) | tree-sitter supported |
/// | `c` | Code(C) | tree-sitter supported |
/// | `cpp` / `c++` | Code(Cpp) | tree-sitter supported |
/// | `csharp` / `c#` | Code(CSharp) | tree-sitter supported |
/// | `ruby` / `rb` | Code(Ruby) | tree-sitter supported |
/// | `sql` | Code(Sql) | tree-sitter supported |
/// | `kotlin` | Code(Kotlin) | tree-sitter supported |
/// | `swift` | Code(Swift) | tree-sitter supported |
/// | anything else | Passthrough | Unknown/unsupported hint |
///
/// Returns `None` for any hint not in the table (treated as passthrough by
/// the caller, AC2 — unsupported hint → Passthrough → no compressor call).
fn hint_to_language(hint: &str) -> Option<HintTarget> {
    match hint.to_ascii_lowercase().as_str() {
        // AD-006: JSON hint → Json engine, NEVER the code arm.
        "json" => Some(HintTarget::Json),

        // Data-format passthrough (AD-006 / AD-012).
        "yaml" | "yml" => Some(HintTarget::Passthrough),
        "toml" => Some(HintTarget::Passthrough),
        "markdown" | "md" => Some(HintTarget::Passthrough),

        // Code languages → rskim-core AST transform.
        "rust" | "rs" => Some(HintTarget::Language(Language::Rust)),
        "typescript" | "ts" | "tsx" => Some(HintTarget::Language(Language::TypeScript)),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => {
            Some(HintTarget::Language(Language::JavaScript))
        }
        "python" | "py" => Some(HintTarget::Language(Language::Python)),
        "go" | "golang" => Some(HintTarget::Language(Language::Go)),
        "java" => Some(HintTarget::Language(Language::Java)),
        "c" => Some(HintTarget::Language(Language::C)),
        "cpp" | "c++" | "cxx" | "cc" => Some(HintTarget::Language(Language::Cpp)),
        "csharp" | "c#" | "cs" => Some(HintTarget::Language(Language::CSharp)),
        "ruby" | "rb" => Some(HintTarget::Language(Language::Ruby)),
        "sql" => Some(HintTarget::Language(Language::Sql)),
        "kotlin" | "kt" => Some(HintTarget::Language(Language::Kotlin)),
        "swift" => Some(HintTarget::Language(Language::Swift)),

        // Unknown / unsupported hint → passthrough (fail-safe, AC2).
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rskim_llm::Class;

    // =========================================================================
    // engine_for_class tests — verify the routing table (AC8 / AD-006)
    // =========================================================================

    #[test]
    fn code_with_rust_hint_routes_to_code_engine() {
        let target = engine_for_class(Class::Code, Some("rust"));
        assert_eq!(target, EngineTarget::Code(Language::Rust));
    }

    #[test]
    fn code_with_json_hint_routes_to_json_engine_not_code() {
        // AD-006: json hint on a Code block → JSON engine, never code arm.
        let target = engine_for_class(Class::Code, Some("json"));
        assert_eq!(
            target,
            EngineTarget::Json,
            "AD-006: json hint must route to Json engine"
        );
    }

    #[test]
    fn code_with_yaml_hint_routes_to_passthrough() {
        // AD-006: data-format hints → passthrough.
        let target = engine_for_class(Class::Code, Some("yaml"));
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_toml_hint_routes_to_passthrough() {
        let target = engine_for_class(Class::Code, Some("toml"));
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_markdown_hint_routes_to_passthrough() {
        let target = engine_for_class(Class::Code, Some("markdown"));
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_md_hint_routes_to_passthrough() {
        let target = engine_for_class(Class::Code, Some("md"));
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_no_hint_routes_to_passthrough() {
        // AC2: no hint → passthrough, no code engine invocation.
        let target = engine_for_class(Class::Code, None);
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_unknown_hint_routes_to_passthrough() {
        // AC2: unknown hint → passthrough.
        let target = engine_for_class(Class::Code, Some("cobol"));
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn json_class_routes_to_json_engine() {
        let target = engine_for_class(Class::Json, None);
        assert_eq!(target, EngineTarget::Json);
    }

    #[test]
    fn log_class_routes_to_log_engine() {
        let target = engine_for_class(Class::Log, None);
        assert_eq!(target, EngineTarget::Log);
    }

    #[test]
    fn mixed_class_routes_to_mixed_engine() {
        let target = engine_for_class(Class::Mixed, None);
        assert_eq!(target, EngineTarget::Mixed);
    }

    #[test]
    fn text_class_routes_to_passthrough() {
        // AC7: text → passthrough, no compressor.
        let target = engine_for_class(Class::Text, None);
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn unknown_class_routes_to_passthrough() {
        // AC7: unknown → passthrough, no compressor.
        let target = engine_for_class(Class::Unknown, None);
        assert_eq!(target, EngineTarget::Passthrough);
    }

    // =========================================================================
    // hint_to_language mapping table coverage (AC8 — each documented hint tested)
    // =========================================================================

    #[test]
    fn mapping_table_all_code_languages() {
        // Verify every documented code-language hint maps to the expected Language.
        let cases: &[(&str, Language)] = &[
            ("rust", Language::Rust),
            ("rs", Language::Rust),
            ("typescript", Language::TypeScript),
            ("ts", Language::TypeScript),
            ("tsx", Language::TypeScript),
            ("javascript", Language::JavaScript),
            ("js", Language::JavaScript),
            ("jsx", Language::JavaScript),
            ("python", Language::Python),
            ("py", Language::Python),
            ("go", Language::Go),
            ("golang", Language::Go),
            ("java", Language::Java),
            ("c", Language::C),
            ("cpp", Language::Cpp),
            ("c++", Language::Cpp),
            ("cxx", Language::Cpp),
            ("csharp", Language::CSharp),
            ("c#", Language::CSharp),
            ("cs", Language::CSharp),
            ("ruby", Language::Ruby),
            ("rb", Language::Ruby),
            ("sql", Language::Sql),
            ("kotlin", Language::Kotlin),
            ("kt", Language::Kotlin),
            ("swift", Language::Swift),
        ];

        for (hint, expected_lang) in cases {
            let target = engine_for_class(Class::Code, Some(hint));
            assert_eq!(
                target,
                EngineTarget::Code(*expected_lang),
                "hint {:?} expected Code({:?})",
                hint,
                expected_lang
            );
        }
    }

    #[test]
    fn mapping_table_data_format_passthrough() {
        // AD-006: data-format hints (yaml, toml, markdown) → passthrough.
        let passthrough_hints = ["yaml", "yml", "toml", "markdown", "md"];
        for hint in &passthrough_hints {
            let target = engine_for_class(Class::Code, Some(hint));
            assert_eq!(
                target,
                EngineTarget::Passthrough,
                "hint {:?} expected Passthrough (AD-006)",
                hint
            );
        }
    }

    #[test]
    fn mapping_table_json_hint_routes_json() {
        // AD-006: json hint → Json engine (single documented entry for this case).
        let target = engine_for_class(Class::Code, Some("json"));
        assert_eq!(
            target,
            EngineTarget::Json,
            "json hint must route to Json engine, never code arm"
        );
    }

    #[test]
    fn hint_matching_is_case_insensitive() {
        // Hints are lowercased before matching.
        let target = engine_for_class(Class::Code, Some("Rust"));
        assert_eq!(target, EngineTarget::Code(Language::Rust));

        let target2 = engine_for_class(Class::Code, Some("JSON"));
        assert_eq!(target2, EngineTarget::Json);
    }
}
