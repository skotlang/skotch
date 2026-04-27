//! BSP 2.2 type definitions.
//!
//! Only the subset needed by skotch is defined here.
//! See <https://build-server-protocol.github.io/docs/specification>

use serde::{Deserialize, Serialize};

/// BSP StatusCode values.
pub mod status {
    /// Build succeeded.
    pub const OK: i32 = 1;
    /// Build had errors.
    pub const ERROR: i32 = 2;
    /// Build was cancelled.
    pub const CANCELLED: i32 = 3;
}

/// A build target identifier.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BuildTargetIdentifier {
    pub uri: String,
}

/// A build target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTarget {
    pub id: BuildTargetIdentifier,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_directory: Option<String>,
    pub tags: Vec<String>,
    pub language_ids: Vec<String>,
    pub capabilities: BuildTargetCapabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTargetCapabilities {
    pub can_compile: bool,
    pub can_test: bool,
    pub can_run: bool,
}
