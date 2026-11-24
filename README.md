# HarnessMesh

Rust runtime for authorized coding-agent tasks between local-first Mesh Nodes.

**Use it from any agent.** Add `hmesh mcp` as a stdio MCP server to Claude Code, Codex, Pi or any other MCP client. The agent can then list the agents registered on this computer and on every paired computer, send one of them a task, and get its final reply (`mesh_list_agents`, `mesh_send`, `mesh_wait`). Pairing is the trust boundary: paired computers see each other's agents, and their tasks run immediately unless the receiving owner switches that peer to confirmation (`hmesh peers access <peer> confirm`). See [MCP](docs/mcp.md).

0.0.2 provides managed Pi, DeepSeek Harness and OpenCode tasks, a local owner panel, immutable artifacts, persistent workflows and maintenance tools. Real Windows↔Mac Tailnet/mTLS and [Windows → Orion/Linux SSH](docs/ssh-first-smoke.md) checkpoints are recorded in the [implementation status](docs/implementation-status.md). Full V0.1/V0.2, Codex and the three-machine Gateway remain later work.

[Direct Tailscale setup for Windows and Mac](docs/quickstart-tailnet.md) supports literal Tailnet IPs, candidate discovery, pinned mTLS and a separate loopback owner API. On Mac, run `node scripts/start-tailnet.mjs` after cloning to build and start a dedicated Node with the local Pi/DeepSeek V4 Flash configuration; SSH is not required. The [real Windows → Mac acceptance](docs/tailnet-mac-smoke.md) passed owner approval, Pi write/read, five-field JSON/hash verification, deduplication, event replay and completed-task recovery after restarting the Windows origin. The [Mac → Windows execution-owner checkpoint](docs/tailnet-reverse-smoke.md) also passed DSH/Pro write/read and independent JSON/hash verification. Mac-origin status, event and verification readback also passed; duplicate submission, recovery, workspace checks and managed-service integration remain separate.

[Bidirectional SSH and native MCP delegation](docs/ssh-bidirectional-mcp.md) now pass: execution survives a cut tunnel and origin restart, replay/deduplication remain correct, and Orion Pi uses native MCP to discover and delegate Windows DSH. That acceptance predates the current MCP model; see [MCP](docs/mcp.md) for today's setup.

The [SSH Todo example](examples/ssh-todo/README.md) runs a Pi-authored API on Orion and a DSH-authored browser client on Windows. Real HTTP, browser operations and backend restart persistence passed; its [acceptance report](docs/evidence/latest-todo-integration.json) also preserves the Pi authoring output-limit failures separately from application test results.

The native-message limit has since been fixed. A [fresh real Pi/Flash regression](docs/evidence/latest-pi-output-regression.json) completed successfully with a 375,131-byte native message and 14 passing generated-backend tests; original failed tasks remain unchanged.

[Daemon-managed HTTP services and SSH tunnels](docs/managed-services.md) add owner-configured leases, scoped mTLS HTTP connections, bounded process restart and frozen deterministic integration contracts with at most two repair tasks. This is a two-node SSH checkpoint; generic TCP, streaming, OS startup installation and the complete V0.2 gates remain separate.

The [real managed-service acceptance](docs/evidence/latest-managed-services.json) passed a missing-field failure → Pi repair → managed restart → fixed-contract retest in one cycle, plus DSH client CRUD, backend/client tests, persistence after restart, automatic SSH recovery and access revocation. Windows 52 / Linux 51 Rust tests passed. Initial proxy/deadline failures are retained alongside the final result.

```powershell
cargo +stable build --locked -p hmesh-cli
.\target\debug\hmesh.exe --data-dir .\.hmesh-v0\node-a init --name local-a --port 38880
.\target\debug\hmesh.exe --data-dir .\.hmesh-v0\node-a daemon start
```

Use Rust 1.90 or newer. `rust-toolchain.toml` and CI pin 1.90.0; `+stable` also works when the installed stable toolchain is at least that version.

See [V0 quickstart](docs/quickstart-v0.md), [protocol](docs/protocol.md), [security boundaries](docs/security.md), and [implementation/acceptance status](docs/implementation-status.md).

A [local management panel](docs/local-panel.md) supports task creation, owner approval, original-request recovery, scoped artifact verification and Node/Agent/service management. Run `hmesh --data-dir YOUR_NODE_DIR panel` and open the private access link. Owner mTLS credentials stay in the CLI process. Optional `--start-daemon` owns only a Daemon started here; compatible existing Daemons are preserved. [Native runtime packages](docs/runtime-packages.md) run without Rust or a frontend build. Opening the panel does not call models.

[Local reliability and onboarding](docs/local-reliability.md) adds completion-time artifact snapshots, opt-in Git worktrees with reviewed patch application, offline identity-preserving backups, scoped/fair queueing, a reference-only Agent setup wizard, versioned runtime preflight and owner-managed login startup. Schema 7 upgrades require a pre-upgrade backup; old successful tasks remain explicitly marked as current-file verification.

[Reusable development workflows](docs/development-workflows.md) in 0.0.2 (schema 8) add immutable task templates, persistent approval-gated stages and bounded repair, structured diagnostic export, reviewed offline retention, reported usage/project budgets and executable Harness compatibility checks. The owner panel includes development and operations views. Back up an existing Node with its current binary before upgrading; the new panel preserves an older running Daemon and requests an explicit upgrade.

Development checks do not call models or read personal Harness configurations:

```powershell
cargo +stable fmt --all --check
cargo +stable clippy --locked --workspace --all-targets -- -D warnings
cargo +stable test --locked --workspace
```

Real Harness smoke requires a separate explicit opt-in and an authorized model credential. A successful Harness run and a passed artifact verification are separate results. Never count the `test-support/hmesh-fixture` process as a real Harness.

