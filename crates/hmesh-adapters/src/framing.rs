use hmesh_core::{ErrorCode, HarnessKind, MeshError, Result};
use serde_json::Value;

/// Native snapshots/tool arguments are larger than normalized Mesh events. The
/// task's cumulative stdout budget remains a separate, potentially tighter cap.
pub fn native_record_limit(kind: HarnessKind, output_budget: usize) -> usize {
    let adapter_limit = match kind {
        HarnessKind::Pi => 4 * 1024 * 1024,
        HarnessKind::Dsh | HarnessKind::Opencode => 1024 * 1024,
    };
    adapter_limit.min(output_budget)
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
