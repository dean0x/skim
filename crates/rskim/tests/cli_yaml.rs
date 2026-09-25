//! YAML integration tests for CLI
//!
//! Tests YAML structure extraction with various modes and fixtures.

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

// ============================================================================
// Basic Structure Tests
// ============================================================================

#[test]
fn test_yaml_simple_structure() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    fs::write(
        &file_path,
        // Long values give the key-only projection something to strip. The
        // original 60-byte document saved 38 B / 13 t, under the 76 B / 22 t
        // marker the ADR-001 guard now charges, so raw was served and every
        // "should NOT contain values" assertion below saw the values.
        // Measured: raw 615 B / 132 t → 146 B / 47 t, margin +393 B / +63 t.
        r#"name: John Doe
age: 30
email: john@example.com
active: true
department: Platform Infrastructure Engineering, Northern Europe Division
biography: Maintains the ingestion pipeline and the regional failover tooling
mailingAddress: 1188 Riverside Parkway, Springfield, Illinois, United States
subscriptionTier: enterprise annual contract with premium support included
notificationPreference: weekly digest delivered as rich text electronic mail
onboardingNotes: transferred from the data platform team in the third quarter
escalationPath: primary on-call rotation then the regional engineering manager
"#,
    )
    .unwrap();

    let output = common::skim()
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
        // Sized so the key-only projection is a real saving: margin
        // +360 B / +70 t against the 76 B / 22 t structure marker.
        r#"user:
  name: John Doe
  address:
    street: 123 Main St
    city: Springfield
    postalCode: 62704-1188
    country: United States of America
    deliveryNotes: leave parcels with the building concierge before six o'clock
  preferences:
    theme: dark
    locale: en-US-POSIX-extended
    timezone: America/Chicago
    digestSchedule: weekly on monday morning before the standing review meeting
    accessibility: prefers reduced motion and high contrast throughout the site
  biography: Maintains the ingestion pipeline and the regional failover tooling
  escalationPath: primary on-call rotation then the regional engineering manager
"#,
    )
    .unwrap();

    let output = common::skim()
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
        // Margin +197 B / +47 t against the 76 B / 22 t structure marker.
        r#"---
apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
  namespace: production-eu-west
  annotations: managed-by-the-platform-release-pipeline
data:
  endpoint: https://orders.internal.example.com/v2/events/ingest
---
apiVersion: v1
kind: Secret
metadata:
  name: app-secrets
  namespace: production-eu-west
  annotations: rotated-nightly-by-the-credential-controller
data:
  token: PLACEHOLDER-NOT-A-REAL-CREDENTIAL
"#,
    )
    .unwrap();

    let output = common::skim()
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
    // `assert_ne!(full_output, structure_output)` needs structure mode to be
    // SERVED. The original 42-byte document saved 17 B / 8 t against a 76 B / 22 t
    // marker, so all four modes — full included — returned the identical raw
    // bytes, and the one assertion distinguishing full from structure failed
    // while the three equality assertions passed for the wrong reason.
    // Measured: raw 452 B / 105 t → 129 B / 30 t, margin +299 B / +53 t.
    let yaml_content = r#"name: Test
value: 42
nested:
  key: value
description: A configuration document used to prove that the serde backed modes
summary: all collapse to the same key only projection for every YAML input given
endpoint: https://orders.internal.example.com/v2/events/ingest
owner: platform-infrastructure@example.com
escalationPath: primary on-call rotation then the regional engineering manager
retentionPolicy: ninety days of hot storage followed by archival to cold tier
"#;
    fs::write(&file_path, yaml_content).unwrap();

    // Get output for each mode
    let structure_output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let signatures_output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let types_output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("types")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let full_output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("full")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Serde-based modes (Structure/Signatures/Types) all produce the same
    // key-only structure extraction for YAML
    assert_eq!(structure_output, signatures_output);
    assert_eq!(structure_output, types_output);

    // Full mode returns original source unchanged (documented contract)
    assert_eq!(full_output, yaml_content.as_bytes());
    assert_ne!(
        full_output, structure_output,
        "Full mode should differ from structure extraction"
    );
}

// ============================================================================
// Auto-Detection Tests
// ============================================================================

#[test]
fn test_yaml_auto_detection_yaml_extension() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("config.yaml");
    // The name is about detection, but `contains("value").not()` is the only
    // falsifiable evidence that the YAML transform ran at all — so the fixture
    // has to be big enough for that transform to be SERVED. The original
    // 10-byte document saved 7 B / 2 t against a 76 B / 22 t marker.
    // Measured: raw 419 B / 96 t → 64 B / 27 t, margin +279 B / +47 t.
    fs::write(
        &file_path,
        r#"key: value
endpoint: https://orders.internal.example.com/v2/events/ingest
owner: platform-infrastructure@example.com
description: A configuration document used by the auto detection test suite
region: eu-west-1 primary with automatic failover to the secondary region
escalationPath: primary on-call rotation then the regional engineering manager
retentionPolicy: ninety days of hot storage followed by archival to cold tier
"#,
    )
    .unwrap();

    common::skim()
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
    // Same shape as the `.yaml` case above: the `.yml` alias is the subject, and
    // the transform must be served for `contains("value").not()` to mean anything.
    fs::write(
        &file_path,
        r#"key: value
endpoint: https://orders.internal.example.com/v2/events/ingest
owner: platform-infrastructure@example.com
description: A configuration document used by the auto detection test suite
region: eu-west-1 primary with automatic failover to the secondary region
escalationPath: primary on-call rotation then the regional engineering manager
retentionPolicy: ninety days of hot storage followed by archival to cold tier
"#,
    )
    .unwrap();

    common::skim()
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
    // stdin is charged the marker exactly as a single file read is (`process_stdin`
    // passes `batch: false`), so the 21-byte document saved 10 B / 6 t against a
    // 76 B / 22 t marker and was served raw, leaking `Test` and `42`.
    // Measured: raw 406 B / 94 t → 44 B / 22 t, margin +286 B / +50 t.
    let yaml_content = r#"name: Test
value: 42
endpoint: https://orders.internal.example.com/v2/events/ingest
owner: platform-infrastructure@example.com
description: A configuration document streamed through standard input by a test
region: eu-west-1 primary with automatic failover to the secondary region
escalationPath: primary on-call rotation then the regional engineering manager
retentionPolicy: ninety days of hot storage followed by archival to cold tier
"#;

    let output = common::skim()
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

    common::skim()
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

    common::skim().arg(&file_path).assert().success();
}

#[test]
fn test_yaml_invalid_syntax() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("invalid.yaml");
    fs::write(&file_path, "invalid: [unclosed").unwrap();

    common::skim()
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
        // The near-miss of the set: the original 85-byte document cleared the
        // TOKEN gate by 5 but failed the BYTE gate by 15, and the guard requires
        // both. Margin is now +207 B / +47 t against the 76 B / 22 t marker.
        r#"items:
  - id: 1
    name: First
    description: the first element of the ordered collection under test
  - id: 2
    name: Second
    description: the second element of the ordered collection under test
tags:
  - admin
  - user
metadata:
  owner: platform-infrastructure@example.com
  endpoint: https://orders.internal.example.com/v2/events/ingest
"#,
    )
    .unwrap();

    let output = common::skim()
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

    common::skim()
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

    common::skim()
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

    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"))
        .stderr(predicate::str::contains("reduction"));
}
