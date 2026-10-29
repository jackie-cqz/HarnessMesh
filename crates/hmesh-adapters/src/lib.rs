//! Version-specific harness boundaries. Protocol fixtures are not real-run evidence.
pub mod executable;
pub mod framing;
pub mod protocol;
pub use executable::{ResolvedExecutable, probe, resolve_executable};
pub use framing::{JsonlDecoder, native_record_limit};
pub use protocol::{ObservedEvent, ProtocolObserver};
