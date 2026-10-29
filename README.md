# HarnessMesh

Rust runtime for authorized coding-agent tasks between local-first Mesh Nodes.

V0 supports **two independent processes on one Windows computer**, with Pi, DeepSeek Harness and OpenCode as managed subprocesses. A first [Windows → Orion/Linux SSH smoke](docs/ssh-first-smoke.md) has also passed using Pi 1.4.0 and DeepSeek V4 Flash. Full V0.1, Tailscale, Codex, managed SSH gateways and service publishing remain later work.

[Bidirectional SSH and native MCP delegation](docs/ssh-bidirectional-mcp.md) now pass: execution survives a cut tunnel and origin restart, replay/deduplication remain correct, and Orion Pi uses native MCP to discover and delegate Windows DSH. [MCP setup](docs/mcp.md) is explicitly scoped to owner-enabled running parents; target execution requires independent approval.

The [SSH Todo example](examples/ssh-todo/README.md) runs a Pi-authored API on Orion and a DSH-authored browser client on Windows. Real HTTP, browser operations and backend restart persistence passed; its [acceptance report](docs/evidence/latest-todo-integration.json) also preserves the Pi authoring output-limit failures separately from application test results.

The native-message limit has since been fixed. A [fresh real Pi/Flash regression](docs/evidence/latest-pi-output-regression.json) completed successfully with a 375,131-byte native message and 14 passing generated-backend tests; original failed tasks remain unchanged.

```powershell
cargo +stable build --locked -p hmesh-cli
.\target\debug\hmesh.exe --data-dir .\.hmesh-v0\node-a init --name local-a --port 38880
.\target\debug\hmesh.exe --data-dir .\.hmesh-v0\node-a daemon start
```

Use Rust 1.90 or newer. `rust-toolchain.toml` and CI pin 1.90.0; `+stable` also works when the installed stable toolchain is at least that version.

See [V0 quickstart](docs/quickstart-v0.md), [protocol](docs/protocol.md), [security boundaries](docs/security.md), and [implementation/acceptance status](docs/implementation-status.md).

Development checks do not call models or read personal Harness configurations:

```powershell
cargo +stable fmt --all --check
cargo +stable clippy --locked --workspace --all-targets -- -D warnings
cargo +stable test --locked --workspace
```

Real Harness smoke requires a separate explicit opt-in and an authorized model credential. A successful Harness run and a passed artifact verification are separate results. Never count the `test-support/hmesh-fixture` process as a real Harness.

