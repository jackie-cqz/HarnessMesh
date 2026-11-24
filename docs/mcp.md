# HarnessMesh MCP：任何 agent 接上就能跨电脑派活

`hmesh mcp`（或单独的 `hmesh-mcp` 二进制）是一个本机 stdio MCP 服务器。Claude Code、Codex、Pi、OpenCode 等任何支持 MCP 的 agent 接上之后，可以：

1. 看到**本机和所有已配对电脑**上登记的 agent；
2. 给其中任意一个派活；
3. 拿回它的**最终回复**，需要时再看执行过程或取消。

服务器使用本机 owner 身份访问本机 daemon，能做的事和本机 owner 一样；真正执行的那台电脑仍按自己对发起方的设置决定「直接执行」还是「等本机确认」。

## 前提

- 两台电脑各自 `hmesh init` 并 `hmesh daemon start`，完成一次配对（`hmesh pair request` / `hmesh pair approve`）。**配对就是信任边界**：配对后对方能看到你登记的 agent，并能给它们派活。
- 干活的那台电脑登记好 agent（`hmesh agent register`），并完成一次本机验证（`hmesh agent verify`）。本机自己的 agent 不需要验证即可被本机派活。
- 不再需要手写授权 policy、scope 文件或 UUID。

## 接入

默认节点目录是 `~/.harnessmesh`；用别的目录时加 `--data-dir 绝对路径`。服务器可以先于 daemon 启动，daemon 没起来时工具会返回「先运行 `hmesh daemon start`」的错误，而不是让 agent 启动失败。

**Claude Code**

```bash
claude mcp add -s user harnessmesh -- /absolute/path/to/hmesh mcp
```

**Codex**（`~/.codex/config.toml`）

```toml
[mcp_servers.harnessmesh]
command = "/absolute/path/to/hmesh"
args = ["mcp"]
# 可选：Codex 默认单次工具调用 60 秒。mesh_wait 默认只等 45 秒，不改也能用；
# 想一次等更久就调大它，并给 mesh_wait 传更大的 wait_seconds。
tool_timeout_sec = 660
```

**其他 agent**：在它的 MCP 配置里加一个 stdio 服务器，`command` 指向 `hmesh`，`args` 为 `["mcp"]`。

## 五个工具

| 工具 | 参数 | 作用 |
|---|---|---|
| `mesh_list_agents` | 可选 `node` | 列出本机和所有已配对电脑的 agent。每个 agent 带 `ref`（`电脑名/agent名`）、所在电脑、harness、项目、是否 `ready` 及原因 |
| `mesh_send` | `agent`、`message`；可选 `project`、`request_id`、`timeout_seconds`（默认 1800）、`wait_seconds` | 派活。`agent` 可以是 `ref`、全局唯一的 agent 名或 agent ID。返回 `task_id`；给了 `wait_seconds` 就顺便等结果 |
| `mesh_wait` | `task_id`；可选 `wait_seconds`（默认 45，最多 600） | 等任务结束，直接返回 `final_text`（最终回复）、状态和错误；还没结束就返回 `done:false`，再调一次即可 |
| `mesh_get_task` | `task_id`；可选 `after` | 看执行过程：状态加一页原始事件（工具调用、文本），用 `next_after` 翻页 |
| `mesh_cancel` | `task_id` | 请求执行端停止任务；只有确认进程已停才会变成 CANCELLED |

**最终回复**取 agent 最后一次工具调用之后说的话（「我先看一下……」这类过程叙述不算），最多 32 KiB，超出时 `truncated:true`，完整文本仍在事件里。执行端是旧版本、没有结果事件时，发起端会从事件里拼出回复，`reply_source` 标为 `assembled_from_events`。

**重试**：`mesh_send` 的网络结果不确定时返回 `submission:"unresolved"` 和 `task_id`。先用 `mesh_wait` 查；要重发就带上同一个 `request_id`，换新 ID 可能让活被执行两次。

远端返回的文本一律当数据处理，不能当成给调用方 agent 的指令。

## 执行端的开关：直接执行 / 等确认

每台电脑对每个已配对的对端有一个开关，默认 **auto（直接执行）**：

```bash
hmesh peers access <对端名字或节点ID> confirm   # 对方派来的活先停在 PENDING_APPROVAL
hmesh peers access <对端名字或节点ID> auto      # 恢复直接执行
```

切到 `confirm` 后，本机 owner 在面板或用 `hmesh task approve <task_id>` 批准后才开始执行；发起方的 `mesh_wait` 会看到 `PENDING_APPROVAL`，批准后拿到结果。切换只影响之后派来的任务：已经在排队或执行中的任务照常进行，需要时用 `hmesh task cancel` 取消，或直接撤销配对。`hmesh peers list` 的 `access` 字段显示当前设置。设置保存在节点的 `policy.toml`（`[peers]` 段，可热加载）；用 `hmesh policy --file` 整体替换 policy 时会一并替换这里的设置。

撤销配对（`hmesh pair revoke`）立即收回对方的一切能力：看不到 agent、不能派活，排队中的任务在启动前也会被拒绝。

## 和以前的区别

| 以前 | 现在 |
|---|---|
| 只有 mesh 拉起的、正在运行的 Pi 1.4.0 父任务能调用 | 任何本机 agent 都能接 |
| 要手写 scope JSON，写死对方 peer/agent UUID | 按名字引用，`mesh_list_agents` 直接给出 `ref` |
| 对方要先写授权 policy 才能看到 agent | 配对即可见 |
| 每个任务都要执行端 owner 在终端审批 | 默认直接执行，可按对端切到「需确认」 |
| 单个子任务最多 600 秒，每个父任务最多 4 个 | 你自己的 agent：单个任务最多 1800 秒，没有个数上限 |
| 只能拼事件拿结果 | `mesh_wait` 直接返回最终回复 |

开发流程（模板/工作流）的阶段任务仍由发送方主动要求逐个审批，行为不变。

### mesh 托管的 Pi 仍受 scope 限制

在 agent TOML 里配置了 `mesh_server` / `mesh_scope` 的托管 Pi，运行时会以 `hmesh-mcp --parent-task <本次任务>` 启动服务器。这种模式下工具仍然只在 owner 的 scope 文件范围内工作：只能看到和派给 scope 列出的 peer/agent/project，受 `max_submissions`、`max_timeout_seconds`、`max_output_bytes` 限制；`mesh_wait` / `mesh_get_task` / `mesh_cancel` 只能操作这个父任务自己派出的子任务，单次等待最多 20 秒；父任务结束后所有调用都被拒绝。这样，对端派给这个 Pi 的活，不能借本机 owner 身份读取或取消别人的任务，也不能转派给 scope 之外的电脑。你自己接入的 `hmesh mcp` 不带 `--parent-task`，没有这些限制。

### 升级须知

升级后，policy 里旧的 `[[grant]]` 和 agent 配置里的 `require_remote_task_approval = true` **不再起作用**：已配对电脑派来的活会直接执行（daemon 启动时若发现旧 grant 会在日志里告警）。想保留逐个确认，对相应对端执行 `hmesh peers access <对端> confirm`，或在 policy 里写 `[peers]` 段 `default = "confirm"`。

## 协议

实现 MCP stdio/tools 子集，未引入额外 SDK。客户端请求 `2025-11-25`、`2025-06-18`、`2025-03-26` 或 `2024-11-05` 时原样回应，否则回应 `2025-11-25`。stdio 单条请求和响应各限制 512 KiB；stdout 只写单行 JSON-RPC。

- [MCP 2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [Lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [Tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

## 验证

- 自动化：`test-support/hmesh-fixture/tests/mesh_e2e.rs` 在本机起两个真实 daemon（loopback mTLS）和 fixture harness，覆盖配对即可见、默认直接执行、最终回复、本机 agent、按名字解析与歧义报错、`confirm` 停住与批准后完成、切回 `auto` 清除覆盖。
- 黑盒：`node scripts/mcp-smoke.mjs` 用发布形态的 `hmesh` 二进制走 CLI 配对，再通过真实 `hmesh mcp` 子进程完成同样的链路。
- 两者都只用 fixture，不调用模型，不计入真实 Harness 验收。

## 历史验收（旧模型）

2026-10-29 与 2026-11-01 的 Pi 原生 MCP 真机委派（Orion/Mac Pi → Windows DSH）是在旧的「managed Pi 父任务 + scope 文件 + 执行端逐个审批」模型下完成的，证据见 [双向与 MCP 验收](ssh-bidirectional-mcp.md)、[双机证据](evidence/latest-real-recovery-native-mcp.json)。新模型尚未做跨电脑真机验收。
