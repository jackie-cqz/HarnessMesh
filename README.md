# HarnessMesh

Rust runtime for authorized coding-agent tasks between local-first Mesh Nodes.

V0 supports **two independent processes on one Windows computer**, with Pi, DeepSeek Harness and OpenCode as managed subprocesses. A first [Windows → Orion/Linux SSH smoke](docs/ssh-first-smoke.md) has also passed using Pi 1.4.0 and DeepSeek V4 Flash. Full V0.1, Codex and the three-machine Gateway remain later work.

[Direct Tailscale setup for Windows and Mac](docs/quickstart-tailnet.md) supports literal Tailnet IPs, candidate discovery, pinned mTLS and a separate loopback owner API. On Mac, run `node scripts/start-tailnet.mjs` after cloning to build and start a dedicated Node with the local Pi/DeepSeek V4 Flash configuration; SSH is not required. The [real Windows → Mac acceptance](docs/tailnet-mac-smoke.md) passed owner approval, Pi write/read, five-field JSON/hash verification, deduplication, event replay and completed-task recovery after restarting the Windows origin. The [Mac → Windows execution-owner checkpoint](docs/tailnet-reverse-smoke.md) also passed DSH/Pro write/read and independent JSON/hash verification. Mac-origin status, event and verification readback also passed; duplicate submission, recovery, workspace checks and managed-service integration remain separate.

[Bidirectional SSH and native MCP delegation](docs/ssh-bidirectional-mcp.md) now pass: execution survives a cut tunnel and origin restart, replay/deduplication remain correct, and Orion Pi uses native MCP to discover and delegate Windows DSH. [MCP setup](docs/mcp.md) is explicitly scoped to owner-enabled running parents; target execution requires independent approval.

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

A [local management panel](docs/local-panel.md) shows Nodes, Agents, tasks/events and managed services. Run `hmesh --data-dir YOUR_NODE_DIR panel`, then open the full access link printed in that terminal. The browser uses a per-launch token; owner mTLS credentials stay in the CLI process. Opening the panel does not start models or restart the Daemon.

Development checks do not call models or read personal Harness configurations:

```powershell
cargo +stable fmt --all --check
cargo +stable clippy --locked --workspace --all-targets -- -D warnings
cargo +stable test --locked --workspace
```

Real Harness smoke requires a separate explicit opt-in and an authorized model credential. A successful Harness run and a passed artifact verification are separate results. Never count the `test-support/hmesh-fixture` process as a real Harness.

