# HarnessMesh MCP：agent 之间互相开会话

`hmesh mcp`（或单独的 `hmesh-mcp` 二进制）是一个本机 stdio MCP 服务器，提供六个工具。它有两种用法：

1. **HarnessMesh 自动注入**（主要用法）：HarnessMesh 通过 ACP 打开的每个会话，都会在 `session/new` 里把 `hmesh mcp --session <会话ID>` 作为 MCP 服务器交给 agent。agent 就能和本机、已配对电脑上的其他 agent 开会话、追问、拿回答复。它打开的会话记作「由这个会话调用」，用来限制调用链。
2. **手动接入**：把 `hmesh mcp` 加进任意 MCP 客户端（例如你平时在终端用的 Claude Code）。这时工具代表本机 owner，打开的会话算你自己的：权限请求在面板里等你回答。

## 前提

- 最省事的是每台电脑跑一次 `hmesh setup`：只想让本机 agent 调用别的电脑，用 `--mcp-only`；还要把本机 agent 共享出去，用 `--full`（见下面「用 hmesh setup 安装」）。手动装就是 `hmesh init` + `hmesh daemon start`。
- 完成一次配对：面板「配对节点」→「配对新电脑」粘贴对方的配对码，对方批准一次即可（CLI 是 `hmesh pair request --code` 和对方的 `hmesh pair approve`）。**配对就是信任边界**：配对后对方能看到你登记的 agent，并能和它们开会话。只装 MCP 的电脑不接受连入，配对必须从它发起。
- 完整安装的电脑用 `hmesh agent detect` 看本机装了哪些 ACP agent（`opencode`、`claude-agent-acp`、`codex-acp`、`pi-acp`、`dsh`），再 `hmesh agent register` 登记（`hmesh setup --full` 会替你登记）。登记只做一次 ACP `initialize` 握手，不调用模型。Codex 与 Claude Code 缺少 ACP 适配器时，可在面板的「接入项目与 Agent」中安装推荐版本。

## 用 hmesh setup 安装

```bash
hmesh setup --mcp-only        # 只装 MCP：本机 agent 调用别的电脑，不接受连入
hmesh setup --full            # 完整安装：同时把本机 agent 共享给配对的电脑
```

| | 只装 MCP | 完整安装 |
|---|---|---|
| 节点配置 | `expose_to_tailnet = false`，只监听 `127.0.0.1`；`network` 是 `tailscale`（并记下 `tailnet_address`）或 `direct` | `expose_to_tailnet = true`，监听本机 Tailnet IP，或直连网络里的所有网卡（`listen_address = "::"`，只对 `trusted_networks` 里认过的网络开门） |
| 需要 Tailscale 吗 | 不需要：找不到 Tailscale 时就用本机自己的网络往外连 | 用 Tailscale 时需要；`--network direct` 时不需要 |
| 别的电脑发来的请求（含配对申请、`hello`） | 一律拒绝（`PERMISSION_DENIED`） | 按配对和访问模式处理 |
| 本机 ACP agent | 不需要，也不登记 | 登记已装的每个 |

- 节点建在 `~/.harnessmesh`（`--data-dir` 可改）。对端只认证书指纹，不认地址，所以以后从只装 MCP 换成完整安装、换网络（再跑一次 `hmesh setup`），指纹和已有配对都不变。直连网络监听所有网卡的电脑换了局域网地址不用重跑 setup。
- 网络：`--network tailscale`（装了 Tailscale 时默认就是它），或 `--network direct`：用电脑本来就在的网络，比如局域网、ZeroTier、WireGuard。直连的完整安装监听所有网卡，但只对认过的网络开门：新装时交互地列出本机所在的网络让你选（有默认路由的那个排在前面、是默认选项）；`--listen 192.168.1.20` 表示认这个地址所在的网络，`--trust 10.0.0.0/24` 再认一个子网（可重复）；Tailscale 在运行时会问是否也接受 Tailnet 里的电脑（默认是），`--with-tailscale` / `--without-tailscale` 预先回答。没人可问时什么网络都不认，也不开 Tailscale，只认这几个参数写明的。读不到网关硬件地址的网络只按网段认，setup、`hmesh networks` 和面板都会注明。重跑 setup 遇到以前只监听一个地址的配置，会迁移成监听所有网卡、认那个地址所在的网络。默认只接受私有网段的连接，`--allow-from 192.168.1.0/24` 可以收得更紧，用公网地址时必须写明。之后换了网络或者局域网地址变了，都不用重跑 setup：地址变了会自动更新并告诉配对的电脑；到了没认过的网络，别的电脑连不进来，用 `hmesh networks list` 看、`hmesh networks trust <子网>` 认（`hmesh networks forget <子网>` 取消），或在面板「配对节点」页「这台电脑」卡片上点「信任这个网络」。Windows 防火墙问的时候只勾「专用网络」。直连网络里，只有打开面板「配对新电脑」或运行 `hmesh pair show` 之后的 10 分钟内，这台电脑才回应配对申请。安全细节见 [安全说明](security.md)。
- 改 HarnessMesh 以外的东西之前都会先问：开机自启（macOS、Windows；Linux 给提示），以及把 MCP 服务器加到 Claude Code（`claude mcp add -s user harnessmesh -- <hmesh> mcp`）和 Codex（在 `config.toml` 末尾追加 `[mcp_servers.harnessmesh]`，`tool_timeout_sec = 660`）。已经有 `harnessmesh` 这一项的客户端不再改动。
- 没有终端可问时（例如 `--json`），没预先回答的一律当作不要：不带 `--connect` 就一个客户端都不碰，不带 `--startup` 就不加开机自启，直连的完整安装不带 `--listen` / `--trust` 就不认任何网络，不带 `--with-tailscale` 就不接受 Tailnet 里的电脑。`--connect claude,codex` 或 `--connect none`、`--startup` 或 `--no-startup` 预先回答。客户端配置里写的是节点目录的绝对路径；Codex 的 `config.toml` 是符号链接时，写的是它指向的文件，链接保留。`--name`、`--port`、`--tailnet-address`、`--network`、`--listen`、`--trust`、`--with-tailscale`、`--allow-from`、`--project`、`--agents` 见 `hmesh setup --help`。
- setup 会在节点目录写 `setup.json`。有这个标记时，`hmesh mcp` 每次先向 daemon 确认它在运行（被强杀或崩溃时留下的 `daemon.json` 不算），没在运行就自己在后台把它启动起来（最多等 15 秒；起不来时工具返回 daemon 日志里的原因）。daemon 收到 SIGTERM 也会正常退出、清理 `daemon.json`。绑定到会话的服务器（`--session`）和手动 `hmesh init` 的节点不会这样做。
- 名片和地址同步：每个 daemon 启动时把自己的名片（地址、端口、是否只装 MCP）发给每台已配对的电脑，没送达就每 5 分钟重试，所以换装法、换端口后对方会更新。监听所有网卡的电脑在认过的网络上的地址变了，几秒内就把新名片和地址列表推给配对的电脑。只装 MCP 的电脑不接受连入，收不到别人的新名片，但它在启动、配对完成时和此后每 5 分钟向每台配对的电脑拉一次地址列表：名片地址，加上对方的主机名（默认 `本机名.local`，可以在 `node.toml` 的 `node.host_names` 里改成公司内网的域名，`[]` 表示不用）。所以对方的局域网 IP 变了，只要主机名还解析得出来，就照样连得上（`.local` 靠 mDNS 解析：macOS 自带，Linux 要装 Avahi 和 nss-mdns，Windows 还没测过）；连上的那个地址会记下来，下次先试。直连网络的电脑还有第二条路：配对后对方把一把「找回密钥」交给它，接受连入的电脑一直在认过的局域网上广播一个每 10 分钟换一次的标记（`_hmesh-find._tcp`，不带电脑名和指纹），所有已知地址都连不上时，它在局域网里听 1.5 秒（只在认过的局域网所在的网卡上听；只装 MCP 的电脑没有认过的网络，在默认路由所在的那张网卡上听），听到对方的标记就连过去（照样核对证书指纹），并记下那个地址；同一台 30 秒内最多找一次。撤销任何一台配对时换一把新密钥，几秒内推给其余能叫到的配对电脑；被撤销的那台拿不到新密钥，认不出新标记。两台电脑完全连不到对方时（都在 NAT 后面），可以经自己的中继：服务器上 `hmesh relay serve`，要被找到的电脑 `hmesh relay use`（粘贴中继码），配对的电脑随地址列表拿到中继地址，经它连过去（中继只转发内层 TLS，看不到内容）；第一次配对用 `hmesh pair show` 打印的中继配对码。都连不上时可以手动加地址：`hmesh peers address <电脑名或ID> 192.168.1.30:38880`（可以写多个，`--clear` 清除），面板电脑卡片上的「改地址」也一样，手动地址只存在本机。配对码仍然只带 IP。

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
hmesh peers address <对端名字或节点ID> studio.local:38880   # 连不上时多试这个地址（只存在本机）
hmesh peers address <对端名字或节点ID> --clear              # 清除手动地址
hmesh pair join 192.168.1.20 --passcode 123456               # 按地址配对：IP 或主机名，不带端口就用 39880
hmesh relay serve --listen 0.0.0.0:7443 --dir ~/hmesh-relay  # 在服务器上跑中继，打印中继码（--public-address 写公网 IP）
hmesh relay show --dir ~/hmesh-relay --rotate-token          # 再打印中继码；--rotate-token 换令牌，旧的连接全断
hmesh relay use < relay-code.txt                             # 这台电脑一直连着中继（码从标准输入读，不进命令行）
hmesh relay forget                                           # 不再用任何中继
hmesh pair request --code                                    # 经对方的中继配对：粘贴对方 hmesh pair show 打印的中继配对码
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
- `crates/hmesh-cli/tests/two_nodes.rs`：只装 MCP 的电脑能向外配对和调用，对方发来的配对申请、`hello` 和节点请求都被拒；换了端口的电脑会把新名片发给配对的电脑；直连网络的电脑只在配对窗口开着时回应配对，地址没了时启动会说明怎么办；口令配对：错口令谁都不信任，对上的口令双方互信、不用批准，用一次作废，错满 3 次口令作废而配对码照样能用，只发起握手不确认的也照样扣次数；macOS 上附近的电脑在窗口开着时能被列出，口令用掉后撤回；已配对的电脑交换地址列表：对方换了地址（127.0.0.1 换成 ::1）后靠 `localhost` 主机名连上并记下 `last_good`，手动地址能连上名片地址已失效的电脑，排在前面的另一台电脑的地址被跳过，排在最前面的地址不回包时对方照样显示在线；监听所有网卡的电脑（`HMESH_TEST_NETWORKS` 把 ::1 当成一个局域网）在没认过的网络上连接在 TLS 之前就被断开、owner 经 127.0.0.1 照常，`hmesh networks trust` 之后不重启就能配对、调用（读不到网关硬件地址，`trust` 和 `list` 都标明只按网段），`forget` 之后立刻又关上；它的地址变了，配对的电脑几秒内收到新名片和地址列表；地址来回变（A、B、再回到 A）时每次都推，第三次的名片虽然以前推过同一个地址也照样推。按主机名配对（`hmesh pair join localhost:<端口>`）能配上，填的地址记成手动地址，连不上的地址和解析不出来的主机名（不带端口时按 39880）都说明卡在哪；找回密钥和口令不出现在日志、审计、事件、`hmesh doctor`、`daemon status` 和 `peers list` 里，对方撤销配对后 `peers list` 标出 `not_accepted`；撤销一台配对电脑后，其余能叫到的配对电脑（两台监听所有网卡的电脑，::1 当局域网）10 秒内就存下新的找回密钥，不等 5 分钟一轮，被撤销的那台仍只有旧的（不靠 mDNS，各平台都跑）；macOS 上对方换了端口、所有已知地址都失效时，靠它广播的标记找回它并记下新地址（找的一方只往外连，`HMESH_TEST_NETWORKS` 把回环当成它默认路由所在的网络），撤销配对后被撤销的一方只有旧密钥，算不出新标记、也找不到它。
- `crates/hmesh-cli/tests/setup.rs` 里直连的完整安装：新装时监听所有网卡，没人可问时什么网络和 Tailscale 都不认，`--listen`、`--trust`、`--with-tailscale` 才认，只按网段认的会说明；旧地址已经不在时重跑也什么都不认；以前只监听一个地址的配置重跑 setup 后迁移成监听所有网卡、认那个地址所在的网络。
- `crates/hmesh-transport/tests/netinfo.rs`：macOS、Linux、Windows 的路由表和 ARP 表输出样例都能读出默认网关和它的硬件地址；围栏逐条核对 owner、认过的网络、没认过的网络、Tailscale 地址和来源网段；发 100.64/10 地址的 CGNAT Wi-Fi 网卡不算 Tailscale，认了 Tailscale 也被围栏断开，隔壁的 Tailnet 范围地址进不了本机的 Tailscale 地址；Tailscale 在运行（`Running`）但连不上协调服务器（`Self.Online` 为 false）时本机地址照样算 Tailscale 的，停了、没登录就都不算。`crates/hmesh-transport/tests/tailscale.rs`：问本机地址时用 `status --json --peers=false`（假的 CLI 只认这组参数）。`crates/hmesh-core/tests/networks.rs`：子网加网关硬件地址的匹配规则、只能信任此刻所在的网络、配置校验。
- `scripts/pairing-panel-smoke.mjs` 的 trust-network 阶段：「这台电脑」卡片提示网络没认过，点「信任这个网络」后 daemon 记下网段和网关，深色和窄屏不溢出。
- `crates/hmesh-transport/tests/routes.rs`：多地址拨号只在请求没发出去时换地址（返回错误或超时的写请求不会再发到别的地址），跳过证书不对的电脑，记住连上的地址，全部失败时逐条说明原因，所有地址都失败时问「找回」钩子、在它给的地址上连上并记下（找回钩子只给另一台电脑的地址，或什么都没听到时，错误里也说明），在找回的地址上请求已经发出去之后失败的（读完请求就断开、回答超过调用方的期限），不再试找回给的下一条地址，写请求只执行一次，回的是错误也算它的回答，第一次接触可以用主机名；第一条地址不回包（包丢掉、连上了但 TLS 不应答、文档用的 192.0.2.1）时 750 毫秒内用上下一条；排在最前面的是慢但通的路（每个包单程延迟半个来回的本机代理，来回 260 和 400 毫秒）、后面的地址失效时，在调用方等的 1.5 秒内照样用上它；第一条连上后回答再慢也不探测、不在别的地址再发；第一条恰好在另一条握手成功的同一刻连上时，请求也只执行一次（`client.rs` 的单元测试在连接建好的那一刻放弃，确认这条连接不被使用、请求不发）。
- `scripts/pairing-nearby-smoke.mjs`（`npm run test:pairing-nearby`，macOS）：真实浏览器里的「附近的电脑」和口令配对。
- `scripts/pairing-address-smoke.mjs`（`npm run test:pairing-address`）：真实浏览器里的「按地址配对」：端口被拒、主机名解析不出来、地址写法不对都用中文说明，按主机名配上后卡片显示「上次连上：…，经 …」，对方停掉后卡片标「不可达」并说明原因；粘贴的中继配对码照样被接受，中继没接通时说明「中继没有接通它」，卡片把经中继的那条记成「经 中继」；深色和窄屏截图不溢出。`crates/hmesh-daemon` 的单元测试核对标记的 HMAC（用 Python 的 hmac 另外算的值）、这 10 分钟和上 10 分钟都认、撤销后换钥匙、只在认过的局域网上广播、同一台 30 秒内只找一次；找的时候只在会广播标记的网卡上找（没认过局域网就不找，只往外连的电脑在默认路由所在的网卡上找），找回用的 mDNS 实例只打开给定地址所在的网卡（读 mdns-sd 的运行状态核对）；后台推送进行到一半时撤销一台、它还没收到这一轮的地址列表（在它被问自己的列表时撤销），它就什么都收不到，其余的电脑这一轮收到开始时那一版、下一轮收到新钥匙。
- `crates/hmesh-cli/tests/relay.rs`：本机模拟内置中继：中继（`hmesh relay serve`）在 127.0.0.1；被找的电脑监听所有网卡但一个网络都没认（没有直连入口），`hmesh relay use` 后一直连着中继；另一台只往外连。配对窗口关着时经中继的配对申请被拒；令牌不对被拒；用中继配对码配对、经中继列 agents、和它的 fixture agent 开会话拿回答复；调试构建里 `HMESH_RELAY_TAP` 记下的转发字节里找不到会话原文、`fixture:echo` 和请求路径；中继码和中继配对码都经标准输入交给 CLI（`hmesh relay use`、`hmesh pair request --code`），令牌不出现在三方的日志、审计、`hmesh doctor`（中继显示 `hidden`）、`daemon status`、`peers list`；重启中继后被找的电脑自动重连，另一台再次调用、开第二轮会话都成功；中继换令牌后旧的控制连接断开，`relay use` 新码后替换旧的并重新连上。`crates/hmesh-transport/tests/relay.rs`：同一进程里经中继调用（不开本机端口）、每次只执行一次、连接留给下一次调用、记住的是中继地址而显示的只是它的位置；中继在来回 150 毫秒的代理后面、名片地址不回包时，第一次调用在 1.5 秒内、第二次在 750 毫秒内都答上（不留连接时第一次就超时）；令牌不对、未知命令、格式不对一律不回一个字就断；目标不在线 `ERR offline`；换令牌后被找的电脑被断开、旧令牌不再能用；中继接错了电脑时证书对不上、请求不发；别的电脑拿自己的证书 `ACCEPT` 发给被叫电脑的编号被断开，被叫的那台照样接得上；被叫的电脑满了回 `BUSY`，叫的一方马上收到 `ERR busy`；登记满了 `ERR full`，转发满了、同一台电脑叫的份额用完 `ERR busy`（另一台照样接通），一句不说的连接 5 秒后被断；同一来源超过限速在 TLS 之前被关；127.0.0.1 开满它的 16 条不说话的连接后，第 17 条马上被关，::1 照样登记、接通、答上，位置全满时占得最多的来源让出最早的一条；`CONNECT` 带着叫的一方的指纹，没配对又不在配对的被叫电脑回 `CLOSED`（叫的一方收到 `ERR closed`，原因 `not_accepted`），配对的那台照样接通；内层出示别的证书的连接被断、什么都不执行；经中继进来的连接闲置到期被被叫的一方关掉，下一次调用开新连接；被叫的一方读完请求头就断开时调用报错、请求只执行一次、不开第二条连接；留着的连接在下一次调用前被对方关掉时，调用换新连接、只执行一次。`crates/hmesh-daemon` 的 `a_computer_behind_a_far_relay_is_listed_online_with_its_agents`：只能经来回 150 毫秒外的中继找到的电脑，在 `/local/peers`（1.5 秒和 750 毫秒）里显示在线并列出 agent；`a_computer_not_paired_cannot_take_the_places_of_the_paired_ones`：拿着中继令牌、没配对的电脑在配对窗口外每次都被 `ERR closed` 挡回，配对窗口开着时只占得到没配对的电脑共用的 16 个位置（用不说话的连接占着也一样），配对的电脑始终接得进来；不出示证书的连接在配对窗口外握完手就断（连申请状态都查不到），窗口开着时照常回答。`relay.rs` 的单元测试覆盖行协议（512 字节、只读到换行为止、超时、未知命令和格式错误）、按来源限速（IPv6 按 /64）、每来源的未鉴权名额和满了时让位（各占一条时，最早的一条挂满 1 秒才让位）、同一个 IPv6 /48 的总名额、被叫电脑按配对的电脑分位置、空闲的一对连接被断开。`crates/hmesh-security/tests/relay.rs`：中继地址和中继配对码的读写、错误写法和链路本地地址被拒、地址列表里中继地址原样保留，中继身份只生成一次、文件 0600、换令牌。
- `crates/hmesh-transport/tests/pinning.rs`：证书里没写着拨的那个地址也能靠指纹连上，换一张证书就连不上。
- 以上只用 fixture，不调用模型。

## 历史验收（旧模型）

2026-10-29 与 2026-11-01 的 Pi 原生 MCP 真机委派是在旧的「managed Pi 父任务 + scope 文件」模型下完成的，证据见 [双向与 MCP 验收](ssh-bidirectional-mcp.md)。那套 `--parent-task` 与 scope 文件已随 ACP 迁移移除。

## 跨机委派的边界

- **只支持一跳**：A 的人开的会话可以叫 B 的 agent；B 上的这个子会话只能叫 B 本机的 agent，不能再叫 A 或第三台电脑。被另一台电脑拉起的会话，`mesh_list_agents` 只列出本机 agent；如果 agent 仍按名字去叫远端，工具会返回「cross-computer delegation is limited to one hop」。
- **取消会传染**：A 取消或关闭父会话时，HarnessMesh 会把这次委派在 B 上的子会话一起取消（只对通过 `mesh_send` 开的远端会话生效，最多等 5 秒，对端已经失联时只能作罢）。
- **对端失联**：`mesh_wait` / `mesh_get_session` 不会无限等待；对端不可达时立即返回「lost contact with the computer running this session」，并提示报告 owner 而不是重试。本机保存的远端会话副本连续两次刷新失败（约 60 秒）后会标为关闭并写一条错误事件，避免它一直显示为「进行中」。
