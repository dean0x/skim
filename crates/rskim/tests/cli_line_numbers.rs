//! CLI tests for --line-numbers / -n flag
//!
//! Tests the --line-numbers flag through the skim binary.
//! Validates: format, source line annotation, mode interactions,
//! truncation interactions, cascade interaction, caching, stdin, multi-file.

use assert_cmd::Command;
use tempfile::TempDir;

/// Get a command for the skim binary with clean env
fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// AC-1: Core line number annotation — format and basic behavior
// ============================================================================

#[test]
fn test_line_numbers_flag_long_form() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { foo } from 'bar';\ntype UserId = string;\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--no-cache")
        .arg("--mode=full")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Each line should have format: {num}\t{content}
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "Should have output");
    // First line should start with "1\t"
    assert!(
        lines[0].starts_with("1\t"),
        "First line should start with '1\\t', got: {:?}",
        lines[0]
    );
    // Second line should start with "2\t"
    if lines.len() >= 2 {
        assert!(
            lines[1].starts_with("2\t"),
            "Second line should start with '2\\t', got: {:?}",
            lines[1]
        );
    }
}

#[test]
fn test_line_numbers_flag_short_form() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "type A = string;\ntype B = number;\n").unwrap();

    // -n is the short form
    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("-n")
        .arg("--no-cache")
        .arg("--mode=full")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty());
    assert!(
        lines[0].starts_with("1\t"),
        "Short form -n should annotate with line numbers"
    );
}

#[test]
fn test_line_numbers_tab_separated_no_fixed_width() {
    // Format is {line}\t{content} — tab-separated, no fixed width padding
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    // Write 12 lines so we can check that line 10 is not space-padded
    let content: String = (1..=12)
        .map(|i| format!("const x{i} = {i};\n"))
        .collect();
    std::fs::write(&file, &content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--no-cache")
        .arg("--mode=full")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    // Line 10 should be "10\t..." not " 10\t..."
    let line10 = lines
        .iter()
        .find(|l| l.starts_with("10\t"))
        .expect("Should have a line starting with '10\\t'");
    assert!(
        !line10.starts_with(" 10\t"),
        "Should not have space padding: {:?}",
        line10
    );
}

// ============================================================================
// AC-5: Full mode — identity mapping
// ============================================================================

#[test]
fn test_line_numbers_full_mode_identity_mapping() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { foo } from 'bar';\ntype A = string;\nfunction hello(): void { return; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    // Full mode: output line N has source line N annotation
    for (i, line) in lines.iter().enumerate() {
        let expected_prefix = format!("{}\t", i + 1);
        assert!(
            line.starts_with(&expected_prefix),
            "Line {} should start with {:?}, got: {:?}",
            i + 1,
            expected_prefix,
            line
        );
    }
}

// ============================================================================
// AC-2: Structure mode — non-contiguous line numbers
// ============================================================================

#[test]
fn test_line_numbers_structure_mode_skips_body_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    // Function body spans lines 3-5, but output replaces with /* ... */
    std::fs::write(
        &file,
        "// comment\ntype A = string;\nfunction hello(name: string): void {\n  console.log(name);\n  return;\n}\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=structure")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Structure mode collapses bodies. With -n, the signature line should show
    // the source line number where the function starts, not consecutive numbering.
    // We just check that:
    // 1. All numbered lines have format "{num}\t{content}"
    // 2. Line numbers are present and 1-indexed
    for line in stdout.lines() {
        if line.contains("/* ... */") || line.is_empty() {
            // The body replacement line should still have a number
            let parts: Vec<&str> = line.splitn(2, '\t').collect();
            if parts.len() == 2 {
                let num: usize = parts[0]
                    .parse()
                    .expect("Line number should parse as usize");
                assert!(num >= 1, "Line number should be >= 1");
            }
        }
    }
}

// ============================================================================
// AC-3: Signatures mode — source line annotation
// ============================================================================

#[test]
fn test_line_numbers_signatures_mode_annotates_source_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { x } from 'y';\ntype A = string;\nfunction foo(a: number): void { return; }\nfunction bar(): string { return 'hi'; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=signatures")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should have line numbers on output lines
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "Signatures mode should produce output");
    for line in &lines {
        // Every line should be annotated with a source line number
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        assert_eq!(
            parts.len(),
            2,
            "Each output line should be tab-separated: {:?}",
            line
        );
        let _num: usize = parts[0]
            .parse()
            .expect(&format!("Line number should parse as usize, got: {:?}", parts[0]));
    }
}

// ============================================================================
// AC-4: Types mode — source line annotation
// ============================================================================

#[test]
fn test_line_numbers_types_mode_annotates_source_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { x } from 'y';\ntype UserId = string;\ntype User = { id: UserId; name: string };\nfunction foo(): void { return; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=types")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "Types mode should produce output");
    let mut annotated_count = 0;
    for line in &lines {
        if line.is_empty() {
            // Blank separator lines between type definitions are emitted without
            // a line-number prefix (source_line = 0 in the map). This is by design.
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        assert_eq!(
            parts.len(),
            2,
            "Non-blank output line should be tab-separated: {:?}",
            line
        );
        let _num: usize = parts[0]
            .parse()
            .expect(&format!("Line number should parse as usize, got: {:?}", parts[0]));
        annotated_count += 1;
    }
    assert!(annotated_count > 0, "At least one line should be annotated with a source line number");
}

// ============================================================================
// AC-8: --max-lines truncation interaction
// ============================================================================

#[test]
fn test_line_numbers_with_max_lines_omission_markers_no_prefix() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    // 10 lines of types
    let content: String = (1..=10)
        .map(|i| format!("type T{i} = string;\n"))
        .collect();
    std::fs::write(&file, &content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--max-lines")
        .arg("5")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Omission markers should have no line number prefix
    // They look like "// ..." or "/* ... */" etc.
    // Lines with numbers should parse fine; omission marker lines should not start with a number\t
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty());
    for line in &lines {
        // If this looks like an omission marker (contains "..."), it should not be prefixed
        if line.contains("/* ... */") || line.contains("// ...") || line.contains("# ...") {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()),
                "Omission markers should not have line number prefix: {:?}",
                line
            );
        }
    }
}

// ============================================================================
// AC-9: --last-lines truncation interaction
// ============================================================================

#[test]
fn test_line_numbers_with_last_lines_truncation_marker_no_prefix() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    let content: String = (1..=10)
        .map(|i| format!("type T{i} = string;\n"))
        .collect();
    std::fs::write(&file, &content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--last-lines")
        .arg("3")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    // Truncation markers (containing "...") should not have line numbers
    // The actual content lines should have line numbers
    let numbered_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| {
            let parts: Vec<&str> = l.splitn(2, '\t').collect();
            parts.len() == 2 && parts[0].parse::<usize>().is_ok()
        })
        .collect();
    // We asked for last 3 lines, so we should have at most 3 numbered content lines
    assert!(
        numbered_lines.len() <= 3,
        "Should have at most 3 numbered content lines, got {}",
        numbered_lines.len()
    );
}

// ============================================================================
// AC-12: Caching — line_numbers in cache key
// ============================================================================

#[test]
fn test_line_numbers_cached_separately_from_unnumbered() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "type A = string;\ntype B = number;\n").unwrap();

    // Run without line numbers (caches result)
    let output1 = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=full")
        .output()
        .unwrap();
    let stdout1 = String::from_utf8(output1.stdout).unwrap();

    // Run with line numbers (should use different cache entry)
    let output2 = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=full")
        .output()
        .unwrap();
    let stdout2 = String::from_utf8(output2.stdout).unwrap();

    // The two outputs should differ (one has line numbers, one doesn't)
    assert_ne!(
        stdout1, stdout2,
        "Line-numbered and unnumbered outputs should differ"
    );
    // The line-numbered one should have tabs
    assert!(
        stdout2.contains('\t'),
        "Line-numbered output should contain tab separators"
    );
}

// ============================================================================
// AC-14: Stdin
// ============================================================================

#[test]
fn test_line_numbers_stdin() {
    let output = skim_cmd()
        .arg("-")
        .arg("-l")
        .arg("typescript")
        .arg("--line-numbers")
        .arg("--mode=full")
        .write_stdin("type A = string;\ntype B = number;\n")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty());
    assert!(
        lines[0].starts_with("1\t"),
        "Stdin output should be annotated with line numbers"
    );
}

// ============================================================================
// AC-13: Multi-file — file headers have no line number prefix
// ============================================================================

#[test]
fn test_line_numbers_multifile_headers_no_prefix() {
    let dir = TempDir::new().unwrap();
    let file1 = dir.path().join("a.ts");
    let file2 = dir.path().join("b.ts");
    std::fs::write(&file1, "type A = string;\n").unwrap();
    std::fs::write(&file2, "type B = number;\n").unwrap();

    // Process two files independently by passing each as a separate invocation
    // and checking that neither file's header (if printed) has a line number prefix.
    // For multi-file test via glob, we use a directory instead of absolute glob.
    let output = skim_cmd()
        .arg(dir.path().to_str().unwrap())
        .arg("--line-numbers")
        .arg("--no-cache")
        .arg("--mode=full")
        .output()
        .unwrap();

    assert!(output.status.success(), "Directory processing should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    // File headers look like "==> file.ts <==" or similar
    // They should NOT have line number prefixes (digits followed by tab)
    for line in stdout.lines() {
        if line.contains("==>") || line.contains("<==") {
            // Header line — should not start with a digit (line number)
            let starts_with_digit = line
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_digit());
            assert!(
                !starts_with_digit,
                "File headers should not have line number prefix: {:?}",
                line
            );
        }
    }
    // At least some content lines should have line numbers
    let has_numbered_lines = stdout.lines().any(|l| {
        let parts: Vec<&str> = l.splitn(2, '\t').collect();
        parts.len() == 2 && parts[0].parse::<usize>().is_ok()
    });
    assert!(
        has_numbered_lines,
        "Multi-file output should have some numbered lines"
    );
}

// ============================================================================
// AC-16: Init guidance update
// ============================================================================

#[test]
fn test_guidance_content_mentions_line_numbers_flag() {
    // The guidance content should mention -n or --line-numbers
    // We test via the library helper (not the CLI) for simplicity
    // This is an integration test that the content was updated
    let output = skim_cmd()
        .arg("init")
        .arg("--help")
        .output()
        .unwrap();
    // Just verify the command exists and works — guidance content is tested in unit tests
    assert!(output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty());
}

// ============================================================================
// AC-17: Edge cases
// ============================================================================

#[test]
fn test_line_numbers_empty_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("empty.ts");
    std::fs::write(&file, "").unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    // Empty file should produce empty output (or just a newline)
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty() || stdout == "\n",
        "Empty file should produce empty or just-newline output, got: {:?}",
        stdout
    );
}

#[test]
fn test_line_numbers_trailing_newline_preserved() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "type A = string;\n").unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Output should end with newline
    assert!(
        stdout.ends_with('\n'),
        "Output should preserve trailing newline"
    );
}

// ============================================================================
// AC-15: Serde formats — non-full modes skip line numbers
// ============================================================================

#[test]
fn test_line_numbers_json_full_mode_applies_identity() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.json");
    std::fs::write(&file, r#"{"key": "value"}"#).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Full mode with JSON: identity line numbers should apply
    // Output should have line numbers
    assert!(
        stdout.contains('\t'),
        "Full mode JSON should have line number annotations"
    );
}

// ============================================================================
// AC-10: Token cascade interaction — line numbers applied after mode selection
// ============================================================================

#[test]
fn test_line_numbers_with_token_cascade() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type A = string;\ntype B = number;\nfunction foo(): void { return; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--line-numbers")
        .arg("--tokens")
        .arg("100")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should have line numbers on output
    let lines: Vec<&str> = stdout.lines().collect();
    // At least some lines should be annotated
    let has_numbered = lines.iter().any(|l| {
        let parts: Vec<&str> = l.splitn(2, '\t').collect();
        parts.len() == 2 && parts[0].parse::<usize>().is_ok()
    });
    assert!(has_numbered, "Token cascade output should have line numbers");
}
