//! Agent-permission sidecar manifests for skim init.
//!
//! Tracks the per-agent `skim-permissions.json` sidecar file that records
//! which agent-native allowlist entries skim wrote, the permission tier in
//! use, and a hash of the target config file at write time.
//!
//! The `PermissionsProtocol` trait and tier-specific writers live in later
//! subtasks. This module currently provides only the sidecar I/O layer.

pub(crate) mod sidecar;
