use hmesh_core::development::UsageTotals;
use serde::{Deserialize, Serialize};

/// Normalized events a run records, independent of the agent behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ObservedEvent {
    Started,
    TextDelta { text: String },
    ToolEvent { name: String, phase: String },
    ApprovalRequested,
    Usage { usage: UsageTotals },
}
