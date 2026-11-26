//! Agent Client Protocol boundary. Every agent is driven over ACP; protocol
//! fixtures are not real-run evidence.
pub mod acp;
pub mod compatibility;
pub mod executable;
pub mod framing;
pub mod own_login;
pub mod protocol;
pub use acp::AcpTaskDriver;
pub use executable::{ResolvedExecutable, probe, resolve_executable, resolve_executable_with_node};
pub use framing::{JsonlDecoder, native_record_limit};
pub use protocol::ObservedEvent;
