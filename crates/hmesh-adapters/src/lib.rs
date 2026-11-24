//! Agent Client Protocol boundary. Every agent is driven over ACP; protocol
//! fixtures are not real-run evidence.
pub mod acp;
pub mod compatibility;
pub mod executable;
pub mod framing;
pub mod protocol;
pub use acp::AcpTaskDriver;
pub use executable::{ResolvedExecutable, probe, resolve_executable};
pub use framing::{JsonlDecoder, native_record_limit};
pub use protocol::ObservedEvent;
