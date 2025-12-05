//! YAML integration tests for CLI
//!
//! Tests YAML structure extraction with various modes and fixtures.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

// ============================================================================
// Basic Structure Tests
// ============================================================================

#[test]
fn test_yaml_simple_structure() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    fs::write(
        &file_path,
        r#"name: John Doe
age: 30
email: john@example.com
active: true
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain keys
    assert!(stdout.contains("name"));
    assert!(stdout.contains("age"));
    assert!(stdout.contains("email"));
    assert!(stdout.contains("active"));

    // Should NOT contain values
    assert!(!stdout.contains("John Doe"));
    assert!(!stdout.contains("30"));
    assert!(!stdout.contains("john@example.com"));
    assert!(!stdout.contains("true"));
}

#[test]
fn test_yaml_nested_structure() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    fs::write(
        &file_path,
        r#"user:
  name: John Doe
  address:
    street: 123 Main St
    city: Springfield
  preferences:
    theme: dark
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain nested keys
    assert!(stdout.contains("user"));
    assert!(stdout.contains("name"));
    assert!(stdout.contains("address"));
    assert!(stdout.contains("street"));
    assert!(stdout.contains("city"));
    assert!(stdout.contains("preferences"));
    assert!(stdout.contains("theme"));

    // Should NOT contain values
    assert!(!stdout.contains("John Doe"));
    assert!(!stdout.contains("123 Main St"));
    assert!(!stdout.contains("Springfield"));
    assert!(!stdout.contains("dark"));
}

// ============================================================================
// Multi-Document Tests
// ============================================================================

#[test]
fn test_yaml_multi_document() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("multi.yaml");
    fs::write(
        &file_path,
        r#"---
apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
---
apiVersion: v1
kind: Secret
metadata:
  name: app-secrets
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain document separator
    assert!(stdout.contains("---"));

    // Should contain keys from all documents
    assert!(stdout.contains("apiVersion"));
    assert!(stdout.contains("kind"));
    assert!(stdout.contains("metadata"));
    assert!(stdout.contains("name"));

    // Should NOT contain values
    assert!(!stdout.contains("ConfigMap"));
    assert!(!stdout.contains("Secret"));
    assert!(!stdout.contains("app-config"));
    assert!(!stdout.contains("app-secrets"));
}

// ============================================================================
// Mode Tests (All Modes Should Be Identical)
// ============================================================================

#[test]
fn test_yaml_modes_identical() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    let yaml_content = r#"name: Test
value: 42
nested:
  key: value
"#;
    fs::write(&file_path, yaml_content).unwrap();

    // Get output for each mode
    let structure_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let signatures_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let types_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("types")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let full_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("full")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // All modes should produce identical output for YAML
    assert_eq!(structure_output, signatures_output);
    assert_eq!(structure_output, types_output);
    assert_eq!(structure_output, full_output);
}

// ============================================================================
// Auto-Detection Tests
// ============================================================================

#[test]
fn test_yaml_auto_detection_yaml_extension() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    fs::write(&file_path, "key: value").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("key"))
        .stdout(predicate::str::contains("value").not());
}

#[test]
fn test_yaml_auto_detection_yml_extension() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yml");
    fs::write(&file_path, "key: value").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("key"))
        .stdout(predicate::str::contains("value").not());
}

// ============================================================================
// Stdin Tests
// ============================================================================

#[test]
fn test_yaml_from_stdin() {
    let yaml_content = r#"name: Test
value: 42
"#;

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language")
        .arg("yaml")
        .write_stdin(yaml_content)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    assert!(stdout.contains("name"));
    assert!(stdout.contains("value"));
    assert!(!stdout.contains("Test"));
    assert!(!stdout.contains("42"));
}

#[test]
fn test_yaml_from_stdin_yml_alias() {
    let yaml_content = "key: value";

    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language")
        .arg("yml")
        .write_stdin(yaml_content)
        .assert()
        .success()
        .stdout(predicate::str::contains("key"));
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_yaml_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.yaml");
    fs::write(&file_path, "").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_yaml_invalid_syntax() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("invalid.yaml");
    fs::write(&file_path, "invalid: [unclosed").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid YAML"));
}

#[test]
fn test_yaml_sequences() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sequences.yaml");
    fs::write(
        &file_path,
        r#"items:
  - id: 1
    name: First
  - id: 2
    name: Second
tags:
  - admin
  - user
"#,
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain keys
    assert!(stdout.contains("items"));
    assert!(stdout.contains("id"));
    assert!(stdout.contains("name"));
    assert!(stdout.contains("tags"));

    // Should NOT contain values
    assert!(!stdout.contains("First"));
    assert!(!stdout.contains("Second"));
    assert!(!stdout.contains("admin"));
    assert!(!stdout.contains("user"));
}

// ============================================================================
// Real-World Fixtures
// ============================================================================

#[test]
fn test_yaml_kubernetes_fixture() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("kubernetes.yaml");
    fs::write(
        &file_path,
        r#"---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web-app
  namespace: production
spec:
  replicas: 3
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        - name: web
          image: myapp:1.0.0
          ports:
            - containerPort: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: web-service
spec:
  selector:
    app: web
  ports:
    - protocol: TCP
      port: 80
      targetPort: 8080
"#,
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("apiVersion"))
        .stdout(predicate::str::contains("kind"))
        .stdout(predicate::str::contains("metadata"))
        .stdout(predicate::str::contains("spec"))
        .stdout(predicate::str::contains("Deployment").not())
        .stdout(predicate::str::contains("apps/v1").not());
}

#[test]
fn test_yaml_github_actions_fixture() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("ci.yaml");
    fs::write(
        &file_path,
        r#"name: CI Pipeline

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Run tests
        run: cargo test
"#,
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("name"))
        .stdout(predicate::str::contains("on"))
        .stdout(predicate::str::contains("jobs"))
        .stdout(predicate::str::contains("steps"))
        .stdout(predicate::str::contains("ubuntu-latest").not())
        .stdout(predicate::str::contains("actions/checkout").not());
}

// ============================================================================
// Token Counting Tests
// ============================================================================

#[test]
fn test_yaml_show_stats() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    fs::write(
        &file_path,
        r#"database:
  host: localhost
  port: 5432
  name: mydb
  credentials:
    username: admin
    password: secret123
"#,
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"))
        .stderr(predicate::str::contains("reduction"));
}
