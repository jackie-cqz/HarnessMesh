# HarnessMesh

**English** | [简体中文](README_CN.md)

One place to work with the coding agents on all of your own computers. HarnessMesh talks to agents such as OpenCode, Claude Code, Codex, Pi and DeepSeek Harness over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP). You chat with any of them from a local web panel, whichever computer it runs on. They can also hand work to each other, across computers, through a built-in MCP server.

![A session on studio: the backend agent finds the bug, asks the reviewer agent on laptop to check the fix, and applies its suggestion](docs/images/session.png)

<sub>Screenshots in this README: two computers ("studio" and "laptop") simulated on one machine, with a scripted demo agent. No model was called. The panel's interface is in Chinese.</sub>

## What you get

- **One panel for every agent.** Start a session with any agent on this computer or a paired one, keep a multi-turn conversation, and follow its thinking, plan and tool calls as they happen. You can stop or close it at any time. Each session can switch model, mode and reasoning level between turns.
- **Agents call each other.** Every session gets the HarnessMesh MCP server, so an agent can open a session with another agent (`mesh_send`), follow up, and read its reply (`mesh_wait`), even on another computer. Calls go at most three levels deep and never loop back to an agent already in the chain.
- **You stay in charge.** When an agent wants to edit a file or run a command, the question comes back to whoever opened the session: 允许一次 (allow once) or 拒绝 (reject). In sessions an agent opened, they are answered automatically; set the calling computer to `confirm` and they wait for you instead.
- **Pair a computer in two steps.** Paste the other computer's pairing code; it approves once with a six-character check code. Then both sides can use each other's agents. Revoking a pairing closes the sessions that computer opened.
- **Model service per agent.** Each agent can use its own login, an API key, or a custom endpoint (31 provider presets). Keys stay on the execution computer. Pi, OpenCode and DSH can select Chat Completions, Responses or Anthropic Messages for a custom endpoint. See the [model service options](docs/local-panel.md#各-agent-可选的协议).
- **Work beyond chat.** The 工作待办 (inbox) collects whatever waits for you. One-shot tasks come with artifact checks, templates and multi-stage workflows, and you can see usage per session and per project.

## Screens

**The other computer sees who called it.** The reviewer session on laptop shows that studio's backend agent opened it, and the call chain studio → laptop.

![The reviewer session on laptop, opened by studio's backend agent](docs/images/delegated.png)

**Permission questions come back to you.** Here the frontend agent on laptop wants to run `npm install`; you answer from studio, where you opened the session.

![A permission request from an agent on another computer](docs/images/permission.png)

**Pairing.** On one computer, paste the other's pairing code. The dialog then shows a check code. On the other computer, the request appears on 配对节点 (paired computers) and in the inbox; type the check code and approve. The first computer finishes by itself.

| 1. Send the request | 2. Approve on the other computer |
|---|---|
| ![The pairing dialog showing the check code](docs/images/pair-sent.png) | ![The incoming request with the check code typed in](docs/images/pair-approve.png) |

**Start from any agent.** The start page lists every ready agent, with the computer it runs on.

![The start page with agents from both computers](docs/images/start.png)

## How it works

```mermaid
flowchart LR
  subgraph A["Your computer"]
    panel["Panel in your browser<br/>127.0.0.1 only"] --> d1["hmesh daemon"]
    d1 <-->|ACP| a1["OpenCode · Claude Code<br/>Codex · Pi · DSH"]
    a1 -.->|"MCP: mesh_send"| d1
  end
  subgraph B["A paired computer"]
    d2["hmesh daemon"] <-->|ACP| a2["its agents"]
  end
  d1 <==>|"mTLS over Tailscale"| d2
```

- Each computer runs one **daemon** (`hmesh daemon start`). It starts the agents you registered and keeps sessions, tasks and their events in a local SQLite database.
- The **panel** (`hmesh panel`) listens on 127.0.0.1 only and opens with a private link printed in the terminal. Owner credentials stay in the CLI process.
- Computers talk over Tailscale with TLS 1.3 and mutual authentication. Each side pins the other's certificate fingerprint. No SSH or cloud relay is needed.

## Get started

You need:

- [Rust](https://rustup.rs) 1.90 or newer.
- Node.js 20 or newer, for the helper scripts.
- At least one ACP agent you have installed and signed in to yourself.
- [Tailscale](https://tailscale.com), to connect more than one computer.

HarnessMesh never installs agents. `hmesh agent detect` shows what it found and how to install the rest:

| Agent | Command HarnessMesh looks for | Install |
|---|---|---|
| OpenCode | `opencode` (runs `opencode acp`) | [opencode.ai](https://opencode.ai) |
| Claude Code | `claude-agent-acp` | `npm install -g @agentclientprotocol/claude-agent-acp` |
| Codex | `codex-acp` | `npm install -g @zed-industries/codex-acp` |
| Pi | `pi-acp` | `npm install -g pi-acp` |
| DeepSeek Harness | `dsh --profile acp` | `npm install -g @deepseek-ai/dsh@0.2.2-rc.5` (Node.js 24.9+) |
| Any other ACP agent | the command and arguments you give it | — |

### On one computer

```sh
git clone https://github.com/jackie-cqz/HarnessMesh.git
cd HarnessMesh
cargo build --release --locked -p hmesh-cli
./target/release/hmesh agent detect
./target/release/hmesh panel --start-daemon
```

1. Open the link the panel prints.
2. On **Agents**, use **接入项目与 Agent** (add a project and agent) to register an agent for a project directory.
3. Go to **新会话** (new session), pick the agent and send your first message.

Registering an agent runs only an ACP `initialize` handshake. No model is called until you send a message. Data lives in `~/.harnessmesh`; pass `--data-dir /absolute/path` to use another directory.

### Across computers (Tailscale)

Run this on each computer, from the repository. It builds the CLI, picks the computer's Tailscale IP, creates a node, registers every ACP agent already installed there, and keeps the daemon running:

```sh
node scripts/start-tailnet.mjs --name studio
```

Then open the panel on one of them:

```sh
./target/debug/hmesh --data-dir "$PWD/.hmesh-v0/tailnet-darwin/node" panel
```

To pair:

1. On computer B, open **配对节点 → 配对新电脑** and copy **这台电脑的配对码** (this computer's pairing code).
2. On computer A, paste it in the same dialog and send the request. A shows a check code.
3. On computer B, type the check code and approve.

On Windows the node directory is `.hmesh-v0/tailnet-win32/node`. See the [Tailscale guide](docs/quickstart-tailnet.md) for options, firewall notes and the command-line equivalents (`hmesh pair show`, `hmesh pair request --code`, `hmesh pair approve`).

### Let your own agent use the mesh

Sessions started by HarnessMesh get the MCP tools automatically. To give them to an agent you run yourself, for example Claude Code in a terminal:

```sh
claude mcp add -s user harnessmesh -- /absolute/path/to/hmesh mcp
```

Codex takes the same server in `~/.codex/config.toml`. The six tools and their limits are described in [MCP](docs/mcp.md).

## Security in brief

- **Pairing is the trust boundary.** Only paired computers see your agents. Unpaired or revoked computers can neither see nor reach them.
- **One approval, checked both ways.** The computer that asked trusts the other only after the other computer's owner approves and accepts its certificate.
- **Pinned certificates.** Every connection uses TLS 1.3 with a pinned certificate. Anonymous routes exist only for the pairing handshake.
- **Owner controls stay local.** The owner API and the panel listen on loopback only.
- **Per-computer access mode.** Each paired computer is either `auto` (its sessions start right away) or `confirm` (you approve first): `hmesh peers access <peer> confirm`.
- **Keys stay put.** Model keys never leave the agent's computer and are not shown again after saving. HarnessMesh does not read agents' own credential files.

**Limits.** Agents run as your user. If you allow an agent to run shell commands, it can do anything you can on that computer, including using HarnessMesh as the owner. The six-character check code guards against approving the wrong request, not against a determined attacker on your tailnet: when it matters, compare the full fingerprint with `hmesh pair approve --expect-fingerprint`. Details are in [security](docs/security.md).

## Documentation

| Topic | Document |
|---|---|
| The panel, page by page | [docs/local-panel.md](docs/local-panel.md) |
| Agents calling agents (MCP tools, limits, manual setup) | [docs/mcp.md](docs/mcp.md) |
| Two computers over Tailscale | [docs/quickstart-tailnet.md](docs/quickstart-tailnet.md) |
| Command-line walkthrough on one computer | [docs/quickstart-v0.md](docs/quickstart-v0.md) |
| Security model | [docs/security.md](docs/security.md) |
| Tasks, templates, workflows, usage and budgets | [docs/development-workflows.md](docs/development-workflows.md) |
| Backups, upgrades, worktrees and queueing | [docs/local-reliability.md](docs/local-reliability.md) |
| Managed HTTP services and tunnels | [docs/managed-services.md](docs/managed-services.md) |
| Prebuilt runtime packages (no Rust needed) | [docs/runtime-packages.md](docs/runtime-packages.md) |
| Wire protocol | [docs/protocol.md](docs/protocol.md) |
| What is built and what has been accepted on real hardware | [docs/implementation-status.md](docs/implementation-status.md) |

## Development

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked -p hmesh-cli -p hmesh-fixture
npm ci && npx playwright install chromium
npm run test:panel   # and test:sessions-panel, test:pairing-panel, …
```

None of the checks call a model or read your agent configuration. They use `test-support/hmesh-fixture`, a scripted ACP agent; a run with it never counts as a real agent run. A real agent run needs a separate, explicit opt-in and your own credentials.

## Status

Version 0.0.2. Developed on macOS; the CI workflow covers macOS, Windows and Linux. The panel's interface is in Chinese. Runtime packages are unsigned development builds without an installer or automatic updates.
