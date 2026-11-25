# HarnessMesh MCP：agent 之间互相开会话

`hmesh mcp`（或单独的 `hmesh-mcp` 二进制）是一个本机 stdio MCP 服务器，提供六个工具。它有两种用法：

1. **HarnessMesh 自动注入**（主要用法）：HarnessMesh 通过 ACP 打开的每个会话，都会在 `session/new` 里把 `hmesh mcp --session <会话ID>` 作为 MCP 服务器交给 agent。agent 就能和本机、已配对电脑上的其他 agent 开会话、追问、拿回答复。它打开的会话记作「由这个会话调用」，用来限制调用链。
2. **手动接入**：把 `hmesh mcp` 加进任意 MCP 客户端（例如你平时在终端用的 Claude Code）。这时工具代表本机 owner，打开的会话算你自己的：权限请求在面板里等你回答。

## 前提

- 最省事的是每台电脑跑一次 `hmesh setup`：只想让本机 agent 调用别的电脑，用 `--mcp-only`；还要把本机 agent 共享出去，用 `--full`（见下面「用 hmesh setup 安装」）。手动装就是 `hmesh init` + `hmesh daemon start`。
- 完成一次配对：面板「配对节点」→「配对新电脑」粘贴对方的配对码，对方批准一次即可（CLI 是 `hmesh pair request --code` 和对方的 `hmesh pair approve`）。**配对就是信任边界**：配对后对方能看到你登记的 agent，并能和它们开会话。只装 MCP 的电脑不接受连入，配对必须从它发起。
- 完整安装的电脑用 `hmesh agent detect` 看本机装了哪些 ACP agent（`opencode`、`claude-agent-acp`、`codex-acp`、`pi-acp`、`dsh`），再 `hmesh agent register` 登记（`hmesh setup --full` 会替你登记）。登记只做一次 ACP `initialize` 握手，不调用模型。HarnessMesh 不会替你安装 agent。

## 用 hmesh setup 安装

```bash
hmesh setup --mcp-only        # 只装 MCP：本机 agent 调用别的电脑，不接受连入
hmesh setup --full            # 完整安装：同时把本机 agent 共享给配对的电脑
```

| | 只装 MCP | 完整安装 |
|---|---|---|
| 节点配置 | `expose_to_tailnet = false`，只监听 `127.0.0.1`；`network` 是 `tailscale`（并记下 `tailnet_address`）或 `direct` | `expose_to_tailnet = true`，监听本机 Tailnet IP，或直连网络里本机的一个地址 |
| 需要 Tailscale 吗 | 不需要：找不到 Tailscale 时就用本机自己的网络往外连 | 用 Tailscale 时需要；`--network direct` 时不需要 |
| 别的电脑发来的请求（含配对申请、`hello`） | 一律拒绝（`PERMISSION_DENIED`） | 按配对和访问模式处理 |
| 本机 ACP agent | 不需要，也不登记 | 登记已装的每个 |

- 节点建在 `~/.harnessmesh`（`--data-dir` 可改）。对端只认证书指纹，不认地址，所以以后从只装 MCP 换成完整安装、换网络、换地址（再跑一次 `hmesh setup`），指纹和已有配对都不变。
- 网络：`--network tailscale`（装了 Tailscale 时默认就是它），或 `--network direct`：用电脑本来就在的网络，比如局域网、ZeroTier、WireGuard。直连的完整安装要选别的电脑连哪个地址：交互时会列出本机地址，也可以用 `--listen` 直接指定；默认只接受私有网段的连接，`--allow-from 192.168.1.0/24` 可以收得更紧，监听公网地址时必须写明。直连网络里，只有打开面板「配对新电脑」或运行 `hmesh pair show` 之后的 10 分钟内，这台电脑才回应配对申请。安全细节见 [安全说明](security.md)。
- 改 HarnessMesh 以外的东西之前都会先问：开机自启（macOS、Windows；Linux 给提示），以及把 MCP 服务器加到 Claude Code（`claude mcp add -s user harnessmesh -- <hmesh> mcp`）和 Codex（在 `config.toml` 末尾追加 `[mcp_servers.harnessmesh]`，`tool_timeout_sec = 660`）。已经有 `harnessmesh` 这一项的客户端不再改动。
- 没有终端可问时（例如 `--json`），没预先回答的一律当作不要：不带 `--connect` 就一个客户端都不碰，不带 `--startup` 就不加开机自启。`--connect claude,codex` 或 `--connect none`、`--startup` 或 `--no-startup` 预先回答。客户端配置里写的是节点目录的绝对路径；Codex 的 `config.toml` 是符号链接时，写的是它指向的文件，链接保留。`--name`、`--port`、`--tailnet-address`、`--network`、`--listen`、`--allow-from`、`--project`、`--agents` 见 `hmesh setup --help`。
- setup 会在节点目录写 `setup.json`。有这个标记时，`hmesh mcp` 每次先向 daemon 确认它在运行（被强杀或崩溃时留下的 `daemon.json` 不算），没在运行就自己在后台把它启动起来（最多等 15 秒；起不来时工具返回 daemon 日志里的原因）。daemon 收到 SIGTERM 也会正常退出、清理 `daemon.json`。绑定到会话的服务器（`--session`）和手动 `hmesh init` 的节点不会这样做。
- 名片同步：每个 daemon 启动时把自己的名片（地址、端口、是否只装 MCP）发给每台已配对的电脑，没送达就每 5 分钟重试，所以换装法、换端口后对方会更新。**已知限制**：只装 MCP 的电脑不接受连入，收不到别人的新名片。Tailscale IP 一般不变，但对方改了端口，或者直连网络上对方的局域网地址变了（DHCP 换地址比 Tailscale 常见得多），只装 MCP 的电脑就连不上它，要重新配对；建议在路由器上给完整安装的电脑固定 IP。

## 手动接入

默认节点目录是 `~/.harnessmesh`；用别的目录时加 `--data-dir 绝对路径`。服务器可以先于 daemon 启动，daemon 没起来时工具返回「先运行 `hmesh daemon start`」，不会让 agent 启动失败（用 `hmesh setup` 装的节点会自己启动 daemon，见上一节）。

```bash
claude mcp add -s user harnessmesh -- /absolute/path/to/hmesh mcp
```

Codex（`~/.codex/config.toml`）：

```toml
[mcp_servers.harnessmesh]
command = "/absolute/path/to/hmesh"
args = ["mcp"]
# Codex 默认单次工具调用 60 秒；mesh_wait 默认只等 45 秒，不改也能用。
tool_timeout_sec = 660
```

## 六个工具

| 工具 | 参数 | 作用 |
|---|---|---|
| `mesh_list_agents` | 可选 `node` | 列出本机和所有已配对电脑的 agent，带 `ref`（`电脑名/agent名`）、是否 `ready` 及原因，以及 `available_projects`（本机：所有未隐藏的项目；配对电脑：它共享的项目，只有名字和 ID）；被注入的服务器会把调用者自己标 `self:true` |
| `mesh_send` | `message`，加 `agent`（开新会话）或 `session_id`（在已有会话里追问）二选一；可选 `wait_seconds`；开新会话时可选 `project`（`available_projects` 里的名字或 ID） | 返回 `session_id`；给了 `wait_seconds` 就顺便等这一轮结束。不给 `project` 时 agent 在它自己的目录里工作 |
| `mesh_wait` | `session_id`；可选 `wait_seconds`（默认 45，最多 600） | 等当前这一轮结束，返回 `final_text`、`stop_reason`、`turns`；还没结束就 `done:false`，再调一次 |
| `mesh_get_session` | `session_id`；可选 `after` | 看过程：会话状态（含当前 `model`、`mode`）加一页事件（消息、工具调用、权限请求），用 `next_after` 翻页 |
| `mesh_cancel` | `session_id` | 让 agent 停下当前这一轮，会话保留 |
| `mesh_close` | `session_id` | 关闭会话并停掉它的 agent 进程 |

**最终回复**取这一轮里 agent 最后一次工具调用之后说的话，最多 32 KiB；完整过程在事件里。远端返回的文本一律当数据处理，不能当成给调用方的指令。

被注入的服务器只能操作「自己这个会话打开的」子会话；手动接入的服务器代表 owner，可以操作本机记录的任何会话。

注意：这些限制只约束 MCP 工具。agent 进程以当前用户运行、能读到节点目录，你批准它执行 shell 命令后，它可以直接用 `hmesh` 命令以 owner 身份行事（见 [安全边界](security.md)）。

## 谁来批权限

| 会话是谁开的 | 权限请求由谁回答 |
|---|---|
| 人（面板、CLI、手动接入的 MCP） | 开会话的那个人，在他自己那台电脑的面板里；agent 所在电脑的主人也可以接管 |
| agent（通过注入的 `hmesh mcp`），执行端对来源电脑是 auto | 自动允许一次 |
| agent，执行端对来源电脑是 confirm | 会话先等执行端主人批准才开始；之后的权限请求也由执行端主人回答 |

「来源电脑」指整条调用链的起点：别的电脑的人打开的会话再往下派出的子会话、孙会话，一律按执行端对那台起点电脑的设置处理。撤销配对时，起点是那台电脑的所有会话一起关闭。

## 调用链限制

- 最多 3 层：人开的会话算第 0 层，往下最多再叫 3 层。
- 同一条链上不能再叫已经在链上的 agent（防止 A→B→A 循环）。
- 一个会话同时最多有 4 个正在运行的子会话。
- **别的电脑打开的会话，只能叫本机的 agent，不能再转到第三台电脑**。配对是两台电脑之间的信任，不能借本机身份把对方的请求带到别处。

## 执行端的开关：直接开始 / 等确认

每台电脑对每个已配对的对端有一个开关，默认 **auto**：

```bash
hmesh peers access <对端名字或节点ID> confirm   # 对方开的会话先等本机批准
hmesh peers access <对端名字或节点ID> auto      # 恢复直接开始
```

`confirm` 时，会话停在 `pending_approval`，第一条消息先不发给 agent；本机主人在面板「会话」页点批准，或 `hmesh session approve <会话ID>` 后才开始。设置保存在节点的 `policy.toml` 的 `[peers]` 段，可热加载。

撤销配对（`hmesh pair revoke`）立即收回对方的一切能力，并关闭对方在本机打开的会话。

## 协议

实现 MCP stdio/tools 子集，未引入额外 SDK。客户端请求 `2025-11-25`、`2025-06-18`、`2025-03-26` 或 `2024-11-05` 时原样回应，否则回应 `2025-11-25`。stdio 单条请求和响应各限制 512 KiB；stdout 只写单行 JSON-RPC。

## 验证

- `test-support/hmesh-fixture/tests/mesh_e2e.rs`：本机起两个真实 daemon（loopback mTLS）和说 ACP 的 fixture agent，覆盖跨电脑多轮会话、打开者回答权限、agent 跨电脑委派（自动允许）、防循环、跨第三台电脑被拦、confirm 等批准、撤销配对关闭会话、daemon 重启后 `session/load` 续上对话、取消与关闭。
- `node scripts/mcp-smoke.mjs`：发布形态的 `hmesh` 二进制走 CLI 配对，再通过真实 `hmesh mcp` 子进程完成开会话、追问、confirm 批准。
- `node scripts/sessions-panel-smoke.mjs`：真实浏览器在面板里完成同样的流程。
- `crates/hmesh-cli/tests/setup.rs`：用假的 `claude` 命令和临时 HOME 跑 `hmesh setup --mcp-only`，核对节点配置、Claude Code 和 Codex 各只接入一次、不同意就一个客户端和开机自启都不碰、`hmesh mcp` 自己拉起 daemon（包括 daemon 被强杀之后）、daemon 起不来时立刻报出原因、Codex 配置是符号链接时链接保留。
- `test-support/hmesh-fixture/tests/mesh_e2e.rs` 的 `a_computer_that_only_connects_out_opens_sessions_but_none_open_on_it`：只装 MCP 的电脑经 MCP 在配对电脑上开会话、拿回答复；对方在它上面开会话被拒。
- `crates/hmesh-cli/tests/two_nodes.rs`：只装 MCP 的电脑能向外配对和调用，对方发来的配对申请、`hello` 和节点请求都被拒；换了端口的电脑会把新名片发给配对的电脑；直连网络的电脑只在配对窗口开着时回应配对，地址没了时启动会说明怎么办。
- `crates/hmesh-transport/tests/pinning.rs`：证书里没写着拨的那个地址也能靠指纹连上，换一张证书就连不上。
- 以上只用 fixture，不调用模型。

## 历史验收（旧模型）

2026-10-29 与 2026-11-01 的 Pi 原生 MCP 真机委派是在旧的「managed Pi 父任务 + scope 文件」模型下完成的，证据见 [双向与 MCP 验收](ssh-bidirectional-mcp.md)。那套 `--parent-task` 与 scope 文件已随 ACP 迁移移除。
