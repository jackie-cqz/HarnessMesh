# Windows ↔ Mac：Tailscale 直连

两台电脑各自在本机启动 HarnessMesh。会话、任务、事件、产物核验和受管 HTTP RPC 使用 Tailnet TCP + TLS 1.3/mTLS，不需要 SSH。配对之后，在任一台电脑的面板「会话」页就能直接和另一台电脑上的 agent 对话。

下面的历史记录早于 ACP 迁移：2026-10-30 已完成 [Windows → Mac 的真实 Pi/Flash 任务](tailnet-mac-smoke.md)：本机批准、write/read、五字段 JSON/hash、同请求去重和事件补读通过。[Mac → Windows 的 DSH/Pro 执行端检查](tailnet-reverse-smoke.md) 也已通过 write/read 和独立 JSON/hash；Mac 发起端的反向读取/补读结论、完整工作区测试和前后端受管服务联调仍待独立验收。

## 用 hmesh setup 安装（推荐）

每台电脑编译一次，再选一种装法：

```sh
cargo build --release --locked -p hmesh-cli
./target/release/hmesh setup --full        # 完整安装：共享本机 agent，接受配对的电脑连入
./target/release/hmesh setup --mcp-only    # 只装 MCP：本机 agent 调用别的电脑，不接受任何连入
```

setup 从 Tailscale 取本机 IP（Mac App Store 版的 CLI 位置同下文；也可 `--tailnet-address`），在 `~/.harnessmesh` 建节点，在后台启动 daemon（日志在节点目录的 `daemon.log`）。`--full` 还会登记本机已装的每个 ACP agent，选项 `--agents`、`--project` 和下面脚本的同名选项含义相同。开机自启和接入 Claude Code、Codex 都先问你，细节见 [MCP 说明](mcp.md#用-hmesh-setup-安装)。之后用 `hmesh panel` 打开面板。

只装 MCP 的电脑不开 Tailnet 监听，所以不需要防火墙放行，但配对只能从它发起：在它的面板里粘贴完整安装那台电脑的配对码。两台只装 MCP 的电脑之间不能配对。

不用 Tailscale 也行：在同一个局域网里，或者已经用着 ZeroTier、WireGuard 之类的网络时，用 `hmesh setup --full --network direct`。它在所有网卡上监听，但只对你认过的网络开门：它会列出本机所在的网络让你选（有默认路由的那个是默认选项；`--listen 192.168.1.20` 表示认这个地址所在的网络，`--trust 10.0.0.0/24` 再认一个），Tailscale 在运行时还会问是否也接受 Tailnet 里的电脑；没有终端可问时只认这些参数写明的。局域网地址变了不用重跑 setup，配对的电脑几秒内就知道；到了没认过的网络，别的电脑连不进来，用 `hmesh networks trust <子网>` 或面板「信任这个网络」认下它。Windows 防火墙问的时候只勾「专用网络」。直连网络里，配对码只在打开「配对新电脑」之后的 10 分钟内有效；「附近的电脑」里没列出对方时（比如在另一个网段），在「按地址配对」填它的 IP 或主机名和它屏幕上的口令。配对后对方的地址变了，已知地址都连不上时会在局域网里按对方的标记找回它。macOS 上，开机自启的 daemon 第一次连局域网里的电脑时，可能要在「系统设置 → 隐私与安全性 → 本地网络」里允许 hmesh。细节见 [MCP 说明](mcp.md#用-hmesh-setup-安装) 和 [安全说明](security.md)。

两台电脑都在 NAT 后面（比如一台在家、一台在公司），或者公司 Wi-Fi 开了客户端隔离、互相连不到时，可以用一台有公网 IP 的云服务器搭桥：自建 Tailscale 中继、让它当 WireGuard 中心，或者在上面跑 HarnessMesh 自带的中继（`hmesh relay serve`），见 [经一台云服务器连](cloud-server.md)。

## 从源码在前台启动（开发时用；Mac、Windows、Linux 相同）

需要 Node.js 20+、通过 rustup 安装的 Rust stable >= 1.90、已登录的 Tailscale，以及你自己装好并登录过的 ACP agent（任选：`opencode`、`claude-agent-acp`、`codex-acp`、`pi-acp`、`dsh`）。DSH 0.2.2-rc.5 需要 Node.js 24.9 或更高版本。启动脚本仅登记已经可用的 ACP 命令。需要 Codex、Claude Code、Pi 或 DSH 时，可以在执行端面板中安装推荐版本并登记。

```sh
git clone https://github.com/jackie-cqz/HarnessMesh.git
cd HarnessMesh
node scripts/start-tailnet.mjs --name mac-a
```

脚本编译 CLI，从本机 Tailscale 状态选择确切 IP，创建独立 Node 和项目目录，把本机已安装的每个 ACP agent 登记为 `<名字>-<类型>`（例如 `mac-a-opencode`、`mac-a-claude-code`；官方 DSH 使用 `mac-a-dsh-acp`），再保持前台运行。登记只做一次 ACP `initialize` 握手，不调用模型，不读取、不复制任何凭据：agent 用它自己的登录和模型设置。保持终端打开；Ctrl+C 正常停止本次 Daemon。

常用选项：

- `--agents opencode,claude-code`：只登记这些；列出的某个没装就报错，不会去装。
- `--project /absolute/dir`：agent 工作的项目目录（默认 `<root>/project`）。
- `--root`、`--port`（默认 39880）、`--tailscale-executable`、`--skip-build`，完整说明见 `--help`。

默认 Node 目录是仓库内 `.hmesh-v0/tailnet-<平台>/node`。启动后打开面板：

```sh
./target/debug/hmesh --data-dir "$PWD/.hmesh-v0/tailnet-darwin/node" panel
```

Tailscale CLI 自动发现常规位置。Mac App Store 版使用 `/Applications/Tailscale.app/Contents/MacOS/Tailscale` 并设置 `TAILSCALE_BE_CLI=1`；也可传 `--tailscale-executable /absolute/path`。

## Windows 启动和候选

在仓库内执行同一个脚本，同样只登记本机已安装的 ACP agent：

```powershell
node .\scripts\start-tailnet.mjs
```

默认 Node 为 `.hmesh-v0/tailnet-win32/node`，端口 39880。两台机器的同一端口互不冲突；本机端口占用时用新的专用 `--root` 和 `--port`，不停止已有应用。Mac 防火墙若询问是否允许该二进制入站连接，按本次测试需求允许；Tailnet grants/ACL 也须允许两端各自的 39880/TCP。

候选发现不要求先初始化 Node：

```powershell
.\target\debug\hmesh.exe --json peers candidates
```

候选仅含主机名、DNS 名、OS、Tailnet IP 和在线状态，不导入 Tailscale 账户、node key 或 Mesh 信任。实际 endpoint 使用字面 IP；当前不以 MagicDNS 作为证书或 Node 身份。名片和配对码只带 IP；配对之后，两台电脑之间交换的地址列表可以带主机名（默认 `本机名.local`，可在 `node.toml` 的 `node.host_names` 里改），对方换了地址时用来找回它，见 [安全说明](security.md) 的「地址列表」。

## 配对和首条任务

最简单的是用面板：Mac 打开面板「配对节点」→「配对新电脑」，复制「这台电脑的配对码」发给自己；Windows 在同一个弹窗里粘贴它、点「发出配对申请」，弹窗显示 6 位校验码；Mac 的「配对节点」页（或「工作待办」）出现这条申请，输入校验码后点「批准」。Windows 几秒内自动完成，两边就都配好了。申请 5 分钟内有效，过期重新申请。

只用命令行时，先在 Mac 本机取配对码（`pair show` 的 `pair_code`，形如 `hmesh://100.80.20.12:39880/<Mac指纹>`），Windows 请求 Mac：

```powershell
$hmesh = '.\target\debug\hmesh.exe'
$node = '.\.hmesh-v0\tailnet-win32\node'
& $hmesh --data-dir $node pair request --code hmesh://100.80.20.12:39880/<Mac指纹>
```

Mac 核对 Windows 的完整指纹后批准，Windows 不需要再批准：

```sh
"$HM" --data-dir "$MAC_NODE" pair approve <Windows请求ID> --expect-fingerprint <Windows指纹>
```

配对完成后不需要写 policy：两边都能看到对方的 agent。在 Windows 打开面板，进入「会话」→「新会话」，电脑选 Mac、Agent 选 `mac-a-opencode` 之类，写第一条消息即可。agent 要用工具时，权限请求出现在**你打开会话的那台电脑**的面板里，点「允许一次」或「拒绝」。

只用命令行时：

```powershell
& $hmesh --data-dir $node agents list --all
& $hmesh --data-dir $node session open --agent <Mac-Agent-ID> --peer <Mac-Node-ID> --message "Summarize this project in three bullet points."
& $hmesh --data-dir $node session events <Session-ID> --wait 10
```

想让 Mac 先批准再开始，在 Mac 上执行 `hmesh peers access <Windows节点> confirm`；之后 Windows 打开的会话会停在「等待批准」，Mac 在面板或用 `hmesh session approve <Session-ID>` 批准。发出消息就会调用模型并可能产生费用。

受管服务复用 [HTTP 配置](managed-services.md)，**Tailnet 路径跳过 tunnel add**；后端继续仅在 Mac 回环监听，经 HarnessMesh mTLS 转发到 Windows 的回环 endpoint。服务租约和重启/修复预算不变。

## 网络边界和开发验收

`init --network tailscale` 才开启 Tailnet。Daemon 每次启动读取本机 `status --json`，要求在线、运行且监听 IP 属于本机；不自动执行 up/login/set。IP 变化时需新实例和重新配对，不静默修改旧证书。IPv4 仅允许 100.64.0.0/10，IPv6 仅允许 fd7a:115c:a1e0::/48；拒绝公网、LAN、wildcard、任意 DNS、URL credentials 和非 origin 路径。地址范围不代替 Tailnet ACL 或 Mesh 认证。

新证书含确切 Tailnet IP 和回环 SAN。公开监听只接受 Tailnet 源地址，`/local/*` 完全拒绝；owner CLI 经同端口的独立 `127.0.0.1` 监听，需要本机证书和 owner token。原回环模式与 SSH 检查点保持兼容。

```powershell
cargo +stable fmt --all --check
cargo +stable clippy --locked --workspace --all-targets -- -D warnings
cargo +stable test --locked --workspace
cargo +stable build --locked -p hmesh-cli
node scripts/check-tailnet-local.mjs --run-local-network
```

最后一项是显式 opt-in 的**单机两个 Daemon、真实 Tailnet IP/TLS**检查，覆盖双向配对、错误指纹、未认证拒绝、携带 owner 凭据的公开管理接口拒绝、撤销和清理；不调用模型，不能替代两台电脑验收。

已完成的 Windows 单机检查及源码/二进制 hash 见 [单机证据](evidence/latest-tailnet-local.json)，两台设备的真实任务见 [Mac 检查点](evidence/latest-tailnet-mac.json)。Mac 完整工作区测试和 CI 结果仍应独立记录，不能用单条真实任务代替。

参考：[Tailscale CLI 官方说明](https://tailscale.com/docs/reference/tailscale-cli?tab=macos)。

启动配置的无模型回归检查：`node --test scripts/tailnet-config.test.mjs`。

Mac 若终端 PATH 中没有 Node，可用 `sh scripts/start-tailnet.sh`，它会再找一遍常见的 Node 位置。开发与验收边界汇总见 [Mac development baseline](mac-development-baseline.md)。
