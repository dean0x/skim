//! Class → engine dispatch and language-hint routing table (#304 Phase 2, P0.1 #427).
//!
//! # P0.1 / ADR-007 — All code blocks pass through losslessly
//!
//! Under ADR-007 (lossless-only egress), the proxy MUST NOT apply lossy
//! transforms to code content. `Class::Code` now routes directly to
//! `EngineTarget::Passthrough` regardless of language hint. The code engine
//! (`engines/code.rs`) is deleted; no rskim-core AST transform is applied on
//! the egress path.
//!
//! # AD-006 — Data-format routing precedence (retained for Mixed/Json)
//!
//! `language_hint == "json"` → JSON engine, NEVER a code arm.
//! `language_hint` in {`yaml`, `toml`, `markdown`} → byte-identical passthrough.
//! Code language hints → `Passthrough` (P0.1 — code is never compressed on egress).
//!
//! # Fence-body routing (Gate 1 — maximise lossless compression)
//!
//! Within `Class::Mixed` blocks, fence bodies are routed via [`engine_for_fence`]
//! (not `engine_for_class`). This keeps the routing table in one place while
//! allowing json-hinted fences to route to the JSON engine:
//!
//! - hint `"json"` (case-insensitive, exact match; `jsonc` excluded — it allows
//!   comments, which are not RFC-8259 JSON) → [`EngineTarget::Json`].
//! - All other hints (code languages, unknown, none) → [`EngineTarget::Passthrough`].
//!
//! After minification the caller applies a `value_equivalent_raw` gate before
//! splicing; any fence the gate cannot confirm as value-equivalent is forwarded
//! byte-identical (fail-safe).
//!
//! # AD-012 — `unknown` is the extension point
//!
//! Adding a future class (e.g., HTML, diff) requires only a new `match` arm;
//! existing arms are unaffected.

/// The engine a block should be routed to.
///
/// Returned by [`engine_for_class`]. `BlockRouter::route`
/// dispatches on this value to select the compressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineTarget {
    /// Route to the JSON engine (`engines/json.rs`).
    Json,
    /// Route to the log engine (`engines/log.rs`).
    Log,
    /// Route to the mixed-content fence scanner (`engines/mixed.rs`).
    Mixed,
    /// Forward byte-identical; no compressor invoked.
    ///
    /// Used for: code blocks (P0.1 / ADR-007), data-format hints (yaml/toml/markdown),
    /// unknown hints, and all `Class::Text` / `Class::Unknown` blocks.
    Passthrough,
}

/// Map a `Class` + optional `language_hint` to an engine target.
///
/// This is the SINGLE committed routing table (AD-006 / AD-012 / P0.1). Every
/// routing decision flows through this function — no other site may
/// hard-code class→engine mappings.
///
/// # Class routing order
///
/// 1. `Class::Code` → [`EngineTarget::Passthrough`] always (P0.1 / ADR-007).
///    Code is NEVER compressed on the egress path regardless of language hint.
/// 2. `Class::Json` → [`EngineTarget::Json`].
/// 3. `Class::Log` → [`EngineTarget::Log`].
/// 4. `Class::Mixed` → [`EngineTarget::Mixed`] (fence scanner in `engines/mixed.rs`).
/// 5. `Class::Text` / `Class::Unknown` → [`EngineTarget::Passthrough`] (AC7).
pub(crate) fn engine_for_class(
    class: rskim_llm::Class,
    _language_hint: Option<&str>,
) -> EngineTarget {
    use rskim_llm::Class;

    match class {
        // P0.1 / ADR-007: code blocks always pass through losslessly on egress.
        // Language hint is irrelevant — no code engine exists on this path.
        Class::Code => EngineTarget::Passthrough,
        Class::Json => EngineTarget::Json,
        Class::Log => EngineTarget::Log,
        Class::Mixed => EngineTarget::Mixed,
        // AD-012: Text and Unknown are passthrough; the Unknown arm is the extension point.
        Class::Text | Class::Unknown => EngineTarget::Passthrough,
    }
}

/// Map a fence language hint to an engine target for use within `Class::Mixed` blocks.
///
/// This is the SINGLE fence routing table (AD-006 / Gate-1). Fence bodies inside
/// mixed content MUST use this function — never `engine_for_class` — so that
/// json-hinted fences get the JSON minifier while all code fences stay lossless.
///
/// # Routing
///
/// | Hint | Engine |
/// |------|--------|
/// | `"json"` (case-insensitive, exact) | [`EngineTarget::Json`] |
/// | All other hints / `None` | [`EngineTarget::Passthrough`] |
///
/// `jsonc` is deliberately excluded: it allows `//` and `/* */` comments, making it
/// non-RFC-8259 JSON. The JSON minifier would reject those as parse errors and return
/// `CompressResult::Passthrough` anyway, but routing them explicitly here avoids the
/// unnecessary parse attempt.
///
/// After the caller receives `EngineTarget::Json` it MUST apply a
/// `value_equivalent_raw` gate before splicing the minified body; any fence the gate
/// cannot confirm as value-equivalent is forwarded byte-identical (fail-safe).
pub(crate) fn engine_for_fence(hint: Option<&str>) -> EngineTarget {
    match hint {
        Some(h) if h.eq_ignore_ascii_case("json") => EngineTarget::Json,
        _ => EngineTarget::Passthrough,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rskim_llm::Class;

    // =========================================================================
    // engine_for_class tests — P0.1 / ADR-007 passthrough for all code (AC8)
    // =========================================================================

    /// P0.1 / ADR-007: rust hint on a Code block → Passthrough (not Code engine).
    ///
    /// Discriminating: any restoration of Code-engine routing for a rust-hinted block
    /// would break this test, because the result would differ from Passthrough.
    #[test]
    fn code_with_rust_hint_routes_to_passthrough() {
        let target = engine_for_class(Class::Code, Some("rust"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "P0.1/ADR-007: rust hint on Code block must route to Passthrough, not Code engine"
        );
    }

    #[test]
    fn code_with_json_hint_routes_to_passthrough() {
        // P0.1: ALL code blocks pass through — even json-hinted Code blocks.
        // (In practice json-fenced code would classify as Mixed or Json, not Code.)
        let target = engine_for_class(Class::Code, Some("json"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "P0.1: json hint on Code block must route to Passthrough"
        );
    }

    #[test]
    fn code_with_yaml_hint_routes_to_passthrough() {
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
        // P0.1: no hint → passthrough.
        let target = engine_for_class(Class::Code, None);
        assert_eq!(target, EngineTarget::Passthrough);
    }

    #[test]
    fn code_with_unknown_hint_routes_to_passthrough() {
        // P0.1: unknown hint → passthrough.
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
    // P0.1 — All 26 code-language hints now route to Passthrough (not Code engine)
    // =========================================================================

    /// P0.1 discriminating: every documented code-language hint now routes to
    /// Passthrough on the egress path (ADR-007).
    ///
    /// Discriminating: any restoration of Code-engine routing for any hint would break
    /// this test, since the result would differ from Passthrough.
    #[test]
    fn mapping_table_all_code_languages_now_passthrough() {
        let code_language_hints: &[&str] = &[
            "rust",
            "rs",
            "typescript",
            "ts",
            "tsx",
            "javascript",
            "js",
            "jsx",
            "python",
            "py",
            "go",
            "golang",
            "java",
            "c",
            "cpp",
            "c++",
            "cxx",
            "csharp",
            "c#",
            "cs",
            "ruby",
            "rb",
            "sql",
            "kotlin",
            "kt",
            "swift",
        ];

        for hint in code_language_hints {
            let target = engine_for_class(Class::Code, Some(hint));
            assert_eq!(
                target,
                EngineTarget::Passthrough,
                "P0.1/ADR-007: hint {:?} on Code block must route to Passthrough, not Code engine",
                hint
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
    fn hint_matching_is_case_insensitive() {
        // Hints are lowercased before matching — Passthrough for code langs (P0.1).
        let target = engine_for_class(Class::Code, Some("Rust"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "P0.1: 'Rust' (mixed case) must route to Passthrough"
        );

        // json hint on Code block is also Passthrough (P0.1 — ALL Code → Passthrough).
        let target2 = engine_for_class(Class::Code, Some("JSON"));
        assert_eq!(
            target2,
            EngineTarget::Passthrough,
            "P0.1: 'JSON' hint on Code block must route to Passthrough"
        );
    }

    // =========================================================================
    // P0.1 — New AC test: >32 KiB code block routes to Passthrough (byte-identical)
    // =========================================================================

    /// P0.1 AC: a large (>32 KiB) code block routes to Passthrough regardless of hint.
    ///
    /// Code blocks route to Passthrough unconditionally (ADR-007) — no prefilter check,
    /// no engine invocation. The discriminating observable: byte-identical output
    /// regardless of block size.
    #[test]
    fn large_code_block_routes_to_passthrough() {
        // A hint that previously routed to Code engine — now must be Passthrough.
        let target = engine_for_class(Class::Code, Some("rust"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "P0.1: large code block must route to Passthrough (not Code engine)"
        );
    }

    // =========================================================================
    // engine_for_fence tests — fence-body routing (AD-006 / Gate-1 / P2-1)
    // =========================================================================

    /// Discriminating: json hint on a fence body → Json engine (Gate-1 restore).
    ///
    /// This test fails on any revision where json fences incorrectly route through
    /// `engine_for_class(Class::Code, hint)` (which always returns Passthrough).
    #[test]
    fn fence_json_hint_routes_to_json_engine() {
        let target = engine_for_fence(Some("json"));
        assert_eq!(
            target,
            EngineTarget::Json,
            "AD-006/Gate-1: 'json' fence hint must route to Json engine"
        );
    }

    #[test]
    fn fence_json_hint_case_insensitive() {
        // engine_for_fence normalises case; "JSON", "Json", "JSON" all → Json.
        for hint in ["JSON", "Json", "jSoN"] {
            let target = engine_for_fence(Some(hint));
            assert_eq!(
                target,
                EngineTarget::Json,
                "engine_for_fence must be case-insensitive for json; got {:?} → {:?}",
                hint,
                target
            );
        }
    }

    #[test]
    fn fence_rust_hint_routes_to_passthrough() {
        // Code-language hints on fences → Passthrough (P0.1 / ADR-007).
        let target = engine_for_fence(Some("rust"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "fence 'rust' hint must route to Passthrough (ADR-007)"
        );
    }

    #[test]
    fn fence_no_hint_routes_to_passthrough() {
        let target = engine_for_fence(None);
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "fence with no hint must route to Passthrough"
        );
    }

    #[test]
    fn fence_jsonc_routes_to_passthrough() {
        // jsonc allows comments → not RFC-8259 JSON → excluded from Json engine.
        let target = engine_for_fence(Some("jsonc"));
        assert_eq!(
            target,
            EngineTarget::Passthrough,
            "jsonc fence must route to Passthrough (comments not RFC-8259)"
        );
    }

    #[test]
    fn fence_unknown_hint_routes_to_passthrough() {
        let target = engine_for_fence(Some("cobol"));
        assert_eq!(target, EngineTarget::Passthrough);
    }
}
