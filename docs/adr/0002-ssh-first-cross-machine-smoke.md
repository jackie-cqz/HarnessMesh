# ADR 0002: First cross-machine integration through SSH

Accepted for the first Windows/Linux experiment, following explicit user selection of SSH and Linux's existing Pi/DeepSeek V4 Flash setup.

- Keep the same hmesh protocol, persisted tasks, approval, certificate pinning and mutual TLS. SSH transports the loopback TCP connections; no SSH remote-command execution replaces the task engine.
- Use a local forward to Linux's Daemon and a remote forward to the Windows Daemon. Both Node Cards retain loopback endpoints. Keep the existing 7892 user tunnel independent and unchanged.
- Build Linux hmesh natively with Rust 1.90.0 in a dedicated user directory, verify official distribution component hashes, and use an offline Cargo.lock vendor snapshot. Do not change system toolchains or shell profiles.
- Extend the pinned Pi adapter to the locally inspected/tested 1.4.0 protocol. Add explicit absolute `node_executable` for NVM's Node 24 because remote SSH defaults to Node 10. Keep actual entry paths under local-owner control.
- Add a Pi/DeepSeek-only model reference and API-key reference. Only the selected official-origin model metadata enters the isolated config. Read the chosen DeepSeek api_key into the child environment in memory, redact it, leave the source untouched and never transfer it to Windows. Other providers and OAuth imports remain unsupported.
- Mark this as a single-direction SSH smoke. It does not satisfy the complete Tailscale/Codex/MCP V0.1 milestone. Preserve V0's earlier evidence with its original binary hashes.
