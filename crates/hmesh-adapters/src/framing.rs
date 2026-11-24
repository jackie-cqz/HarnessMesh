use hmesh_core::{ErrorCode, MeshError, Result};
use serde_json::Value;

/// Largest single ACP frame accepted from an agent (tool inputs and diffs can
/// be large). The task's cumulative stdout budget remains a separate,
/// potentially tighter cap.
pub const MAX_ACP_FRAME_BYTES: usize = 4 * 1024 * 1024;
pub fn native_record_limit(output_budget: usize) -> usize {
    MAX_ACP_FRAME_BYTES.min(output_budget)
}

/// LF is the only delimiter. Unicode line separators are valid JSON string data.
pub struct JsonlDecoder {
    pending: Vec<u8>,
    max_record_bytes: usize,
    largest_record_bytes: usize,
    record_count: usize,
}
impl JsonlDecoder {
    pub fn new(max_record_bytes: usize) -> Self {
        Self {
            pending: vec![],
            max_record_bytes,
            largest_record_bytes: 0,
            record_count: 0,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        let mut records = vec![];
        for byte in bytes {
            if *byte == b'\n' {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                if !self.pending.is_empty() {
                    self.largest_record_bytes = self.largest_record_bytes.max(self.pending.len());
                    records.push(
                        serde_json::from_slice(&self.pending)
                            .map_err(|_| MeshError::input("harness emitted malformed JSONL"))?,
                    );
                    self.record_count += 1;
                }
                self.pending.clear();
            } else {
                if self.pending.len() >= self.max_record_bytes {
                    return Err(MeshError::new(
                        ErrorCode::RateLimited,
                        format!(
                            "harness JSONL record exceeds {} byte native message limit",
                            self.max_record_bytes
                        ),
                    ));
                }
                self.pending.push(*byte);
            }
        }
        Ok(records)
    }
    pub fn largest_record_bytes(&self) -> usize {
        self.largest_record_bytes
    }
    pub fn record_count(&self) -> usize {
        self.record_count
    }
    pub fn finish(self) -> Result<()> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(MeshError::input(
                "harness stdout ended with an incomplete JSONL record",
            ))
        }
    }
}
