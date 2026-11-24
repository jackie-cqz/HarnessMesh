# HarnessMesh

An agent hub for your own computers. HarnessMesh connects to coding agents over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP) and lets you talk to them, and lets them talk to each other, on this computer and on paired computers.

- **Sessions in one panel.** Open a session with OpenCode, Claude Code, Codex, Pi or any other ACP agent, keep a multi-turn conversation, watch its tool calls, answer its permission requests, stop or close it. Agents on a paired computer are used the same way from here. Run `hmesh --data-dir YOUR_NODE_DIR panel` and open the private link.
- **Agents call each other.** Every session gets `hmesh mcp` bound to it, so the agent can open sessions with other agents (`mesh_send`), follow up in them and read their replies (`mesh_wait`). Delegation is limited to three levels and never loops back to an agent already on the chain. A session another computer opened can call agents on this computer, but not reach further computers.
- **Pairing is the trust boundary.** Paired computers see each other's agents. Sessions they open start right away unless the receiving owner switches that peer to confirmation (`hmesh peers access <peer> confirm`). Whoever opened a session answers its permission requests; sessions an agent opened are answered automatically, or by the owner of the agent's computer in confirm mode. Revoking a pairing closes the sessions that peer opened.
- **Only installed agents.** `hmesh agent detect` lists the ACP commands on PATH (`opencode`, `claude-agent-acp`, `codex-acp`, `pi-acp`); HarnessMesh never installs an agent. Registering one runs only an ACP `initialize` handshake; no model is called until you send a message.
- **Model, mode and keys.** Switch a session's model, mode or reasoning level between turns (ACP session settings). Per agent, choose its own login, an API key or a custom endpoint, a default model and extra environment variables; keys stay on the agent's computer and are injected only into the agent process (`hmesh agent service`, or Agents → 模型服务 in the panel). OpenCode on a custom endpoint can offer a list of models, and sessions then switch only among them. A connection test (`--test`, or 测试连接) asks the service for its model list first and sends one minimal request only when it cannot list them. The dialog first says whether the agent is already set up on its own (its own ACP report or read-only status command; no credential files are read), and custom endpoints can start from 31 provider presets. A key may also come from an environment variable instead of being stored.

See [MCP](docs/mcp.md) and [security boundaries](docs/security.md). The one-shot task, artifact and workflow features below still work; they now drive agents over ACP too.

### Earlier acceptance records

These checkpoints predate the switch to ACP (they used each agent's native protocol). Their scripts were retired with that switch; the records stay as history.

[Direct Tailscale setup for Windows and Mac](docs/quickstart-tailnet.md) supports literal Tailnet IPs, candidate discovery, pinned mTLS and a separate loopback owner API. Run `node scripts/start-tailnet.mjs` after cloning to build and start a dedicated Node and register every ACP agent already installed on that computer; SSH is not required. The [real Windows → Mac acceptance](docs/tailnet-mac-smoke.md) passed owner approval, Pi write/read, five-field JSON/hash verification, deduplication, event replay and completed-task recovery after restarting the Windows origin. The [Mac → Windows execution-owner checkpoint](docs/tailnet-reverse-smoke.md) also passed DSH/Pro write/read and independent JSON/hash verification. Mac-origin status, event and verification readback also passed; duplicate submission, recovery, workspace checks and managed-service integration remain separate.

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

