//! Bash transformation tests — verify all modes work correctly
//!
//! AC-F4.1: structure mode shows functions AND top-level commands/variable assignments
//! AC-F4.2: shebang detection (sh/bash/dash/zsh/ksh, python, node, ruby; CRLF-tolerant)
//! AC-F4.6: all 6 modes have explicit Bash arms

#![allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests

use rskim_core::{Language, Mode, transform};

const FUNCTIONS_SH: &str = include_str!("../../../tests/fixtures/bash/functions.sh");
const DEPLOY_SH: &str = include_str!("../../../tests/fixtures/bash/deploy.sh");
const HEREDOC_SH: &str = include_str!("../../../tests/fixtures/bash/heredoc_case.sh");
const CRLF_SH: &str = include_str!("../../../tests/fixtures/bash/crlf_dialect.sh");

// ============================================================================
// Language detection
// ============================================================================

#[test]
fn test_bash_extension_detection() {
    use std::path::Path;
    assert_eq!(rskim_core::detect_language("sh"), Some(Language::Bash));
    assert_eq!(rskim_core::detect_language("bash"), Some(Language::Bash));
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("deploy.sh")),
        Some(Language::Bash)
    );
    assert_eq!(
        rskim_core::detect_language_from_path(Path::new("script.bash")),
        Some(Language::Bash)
    );
}

#[test]
fn test_bash_shebang_detection_direct_paths() {
    // #!/bin/bash
    assert_eq!(Language::from_shebang("#!/bin/bash"), Some(Language::Bash));
    // #!/bin/sh
    assert_eq!(Language::from_shebang("#!/bin/sh"), Some(Language::Bash));
    // #!/usr/bin/dash
    assert_eq!(
        Language::from_shebang("#!/usr/bin/dash"),
        Some(Language::Bash)
    );
    // #!/bin/zsh
    assert_eq!(Language::from_shebang("#!/bin/zsh"), Some(Language::Bash));
    // #!/usr/bin/ksh
    assert_eq!(
        Language::from_shebang("#!/usr/bin/ksh"),
        Some(Language::Bash)
    );
}

#[test]
fn test_bash_shebang_detection_env_form() {
    // #!/usr/bin/env bash
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env bash"),
        Some(Language::Bash)
    );
    // #!/usr/bin/env sh
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env sh"),
        Some(Language::Bash)
    );
    // #!/usr/bin/env -S bash -x  (env with flags)
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env -S bash -x"),
        Some(Language::Bash)
    );
}

#[test]
fn test_shebang_detection_other_languages() {
    // python
    assert_eq!(
        Language::from_shebang("#!/usr/bin/python"),
        Some(Language::Python)
    );
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env python3"),
        Some(Language::Python)
    );
    // node
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env node"),
        Some(Language::JavaScript)
    );
    // ruby
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env ruby"),
        Some(Language::Ruby)
    );
}

#[test]
fn test_shebang_detection_crlf_tolerant() {
    // CRLF line ending on shebang
    assert_eq!(
        Language::from_shebang("#!/usr/bin/env bash\r"),
        Some(Language::Bash)
    );
    assert_eq!(Language::from_shebang("#!/bin/sh\r"), Some(Language::Bash));
}

#[test]
fn test_shebang_detection_non_shebang() {
    assert_eq!(Language::from_shebang("# regular comment"), None);
    assert_eq!(Language::from_shebang(""), None);
    assert_eq!(Language::from_shebang("echo hello"), None);
}

// ============================================================================
// Structure mode (AC-F4.1)
// ============================================================================

#[test]
fn test_bash_structure_strips_function_bodies() {
    let result = transform(FUNCTIONS_SH, Language::Bash, Mode::Structure).unwrap();
    // Function bodies should be replaced with {...}
    assert!(
        result.contains("{...}"),
        "function bodies should be replaced with {{...}}, got:\n{result}"
    );
    // Function names should be preserved
    assert!(
        result.contains("log"),
        "log function should be visible, got:\n{result}"
    );
    assert!(
        result.contains("deploy"),
        "deploy function should be visible, got:\n{result}"
    );
    assert!(
        result.contains("retry"),
        "retry function should be visible, got:\n{result}"
    );
}

#[test]
fn test_bash_structure_zero_function_deploy_renders_meaningful_output() {
    // AC-F4.1: zero-function deploy.sh renders meaningful structure
    // (not near-empty — all top-level commands/assignments visible)
    let result = transform(DEPLOY_SH, Language::Bash, Mode::Structure).unwrap();
    // Variable assignments at top level are preserved
    assert!(
        result.contains("APP_NAME"),
        "top-level variable assignments should be visible, got:\n{result}"
    );
    assert!(
        result.contains("docker"),
        "top-level commands should be visible, got:\n{result}"
    );
    assert!(
        result.contains("kubectl"),
        "top-level commands should be visible, got:\n{result}"
    );
    // Output must not be empty
    assert!(
        result.lines().count() > 3,
        "zero-function deploy.sh should have substantial structure output, got:\n{result}"
    );
}

#[test]
fn test_bash_structure_output_never_exceeds_raw() {
    // AC-F4.1: output never exceeds raw (≤ raw token count)
    let result = transform(FUNCTIONS_SH, Language::Bash, Mode::Structure).unwrap();
    assert!(
        result.len() <= FUNCTIONS_SH.len(),
        "structure output should not exceed raw input size"
    );
}

#[test]
fn test_bash_structure_fixtures_parse() {
    // AC-F4.1: ≥4 fixtures parse without panic
    for (name, src) in [
        ("functions.sh", FUNCTIONS_SH),
        ("deploy.sh", DEPLOY_SH),
        ("heredoc_case.sh", HEREDOC_SH),
        ("crlf_dialect.sh", CRLF_SH),
    ] {
        let result = transform(src, Language::Bash, Mode::Structure);
        assert!(result.is_ok(), "Failed to parse {name}: {:?}", result.err());
    }
}

// ============================================================================
// Full mode (AC-F4.6: full == source bytes)
// ============================================================================

#[test]
fn test_bash_full_mode_equals_source() {
    for (name, src) in [("functions.sh", FUNCTIONS_SH), ("deploy.sh", DEPLOY_SH)] {
        let result = transform(src, Language::Bash, Mode::Full).unwrap();
        assert_eq!(
            result, src,
            "Full mode should return source bytes unchanged for {name}"
        );
    }
}

// ============================================================================
// Pseudo and Minimal modes (AC-F4.6: # markers)
// ============================================================================

#[test]
fn test_bash_pseudo_mode_preserves_functions() {
    let result = transform(FUNCTIONS_SH, Language::Bash, Mode::Pseudo).unwrap();
    // Bash pseudo strips nothing (no type annotations / decorators in bash)
    // but should still parse and return something non-empty
    assert!(
        !result.is_empty(),
        "pseudo mode should return non-empty output"
    );
    // Function definitions should be visible
    assert!(
        result.contains("log") || result.contains("deploy"),
        "pseudo mode should preserve function structure, got:\n{result}"
    );
}

#[test]
fn test_bash_minimal_mode_runs_without_error() {
    for (name, src) in [("functions.sh", FUNCTIONS_SH), ("deploy.sh", DEPLOY_SH)] {
        let result = transform(src, Language::Bash, Mode::Minimal);
        assert!(
            result.is_ok(),
            "Minimal mode should not error for {name}: {:?}",
            result.err()
        );
    }
}

#[test]
fn test_bash_signatures_mode() {
    let result = transform(FUNCTIONS_SH, Language::Bash, Mode::Signatures).unwrap();
    // Signatures mode should include function names
    assert!(
        result.contains("log") || result.contains("deploy") || result.contains("retry"),
        "Signatures mode should extract function definitions, got:\n{result}"
    );
}

#[test]
fn test_bash_types_mode_empty_for_no_types() {
    // Bash has no type system — types mode returns empty
    let result = transform(FUNCTIONS_SH, Language::Bash, Mode::Types).unwrap();
    // Types mode for bash should be empty or minimal (no type system)
    assert!(
        result.trim().is_empty() || result.len() < FUNCTIONS_SH.len(),
        "Types mode should produce minimal output for bash (no type system), got:\n{result}"
    );
}

// ============================================================================
// Shebang integration (AC-F4.2)
// ============================================================================

#[test]
fn test_bash_as_str() {
    assert_eq!(Language::Bash.as_str(), "bash");
}

#[test]
fn test_bash_name() {
    assert_eq!(Language::Bash.name(), "Bash");
}
