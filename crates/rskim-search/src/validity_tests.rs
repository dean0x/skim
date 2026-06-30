//! Unit tests for the validity-marker module (validity.rs, #376).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn sample() -> ValidityMarker {
    ValidityMarker {
        idx_len: 0x0102_0304_0506_0708,
        idx_mtime_ns: -1_234_567_890_123,
        post_len: 0xAABB_CCDD,
        post_mtime_ns: 9_999_999_999_999_999,
        checksum: 0xDEAD_BEEF,
    }
}

#[test]
fn marker_size_is_52_bytes() {
    // Pins the on-disk record width (AD-376-2). A drift here would silently
    // change marker semantics across binaries.
    assert_eq!(MARKER_SIZE, 52);
    assert_eq!(sample().encode().len(), MARKER_SIZE);
}

#[test]
fn encode_decode_round_trips() {
    let m = sample();
    let bytes = m.encode();
    let back = ValidityMarker::decode(&bytes).expect("well-formed marker must decode");
    assert_eq!(m, back);
}

#[test]
fn decode_rejects_wrong_length() {
    // Truncated, zero-length, and over-long buffers are all marker misses (AC6).
    assert!(ValidityMarker::decode(&[]).is_none());
    assert!(ValidityMarker::decode(&[0u8; MARKER_SIZE - 1]).is_none());
    assert!(ValidityMarker::decode(&[0u8; MARKER_SIZE + 1]).is_none());
}

#[test]
fn read_marker_absent_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.skverify");
    assert!(read_marker(&path).is_none());
}

#[test]
fn read_marker_garbage_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.skverify");
    std::fs::write(&path, b"not a valid marker record at all").unwrap();
    assert!(read_marker(&path).is_none());
}

#[test]
fn read_marker_zero_length_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.skverify");
    std::fs::write(&path, b"").unwrap();
    assert!(read_marker(&path).is_none());
}

#[test]
fn write_then_read_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rt.skverify");
    let m = sample();
    write_marker_best_effort(dir.path(), &path, &m);
    let back = read_marker(&path).expect("written marker must read back");
    assert_eq!(m, back);
}

#[test]
fn current_signature_carries_header_checksum() {
    // The signature must embed the header checksum field verbatim (AD-376-2),
    // and reflect the actual file lengths.
    let dir = tempfile::tempdir().unwrap();
    let idx_path = dir.path().join("index.skidx");
    let post_path = dir.path().join("index.skpost");
    std::fs::write(&idx_path, vec![0u8; 100]).unwrap();
    std::fs::write(&post_path, vec![0u8; 250]).unwrap();

    let sig = current_signature(&idx_path, &post_path, 0x1234_5678)
        .expect("signature over existing files must be Some");
    assert_eq!(sig.idx_len, 100);
    assert_eq!(sig.post_len, 250);
    assert_eq!(sig.checksum, 0x1234_5678);
}

#[test]
fn current_signature_missing_file_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let idx_path = dir.path().join("index.skidx");
    let post_path = dir.path().join("index.skpost");
    std::fs::write(&idx_path, vec![0u8; 10]).unwrap();
    // post_path intentionally absent.
    assert!(current_signature(&idx_path, &post_path, 0).is_none());
}

#[test]
fn unlink_marker_is_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u.skverify");
    // Unlinking an absent marker must not panic.
    unlink_marker_best_effort(&path);
    // Unlinking an existing marker removes it.
    std::fs::write(&path, sample().encode()).unwrap();
    assert!(path.exists());
    unlink_marker_best_effort(&path);
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn written_marker_is_owner_only_0o600() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perm.skverify");
    write_marker_best_effort(dir.path(), &path, &sample());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    // AC9: sidecar carries 0o600 (atomic_write contract).
    assert_eq!(mode & 0o777, 0o600, "marker must be owner-only on Unix");
}
