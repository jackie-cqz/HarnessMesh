# Windows ↔ Mac：Tailscale 直连

两台电脑各自在本机启动 HarnessMesh。任务、事件、产物核验和受管 HTTP RPC 使用 Tailnet TCP + TLS 1.3/mTLS，不需要 SSH。候选可达、Node 信任、项目授权和 Agent 可运行状态分别记录。

2026-10-30 已完成 [Windows → Mac 的真实 Pi/Flash 任务](tailnet-mac-smoke.md)：本机批准、write/read、五字段 JSON/hash、同请求去重和事件补读通过。[Mac → Windows 的 DSH/Pro 执行端检查](tailnet-reverse-smoke.md) 也已通过 write/read 和独立 JSON/hash；Mac 发起端的反向读取/补读结论、完整工作区测试和前后端受管服务联调仍待独立验收。

## Mac 启动

需要 Node.js 20+、通过 rustup 安装的 Rust stable >= 1.90、已登录的 Tailscale，以及已配置 `deepseek/deepseek-v4-flash` 的 Pi。当前 Pi Adapter 的协议基线是 0.84.2 / 1.4.0；其他版本会明确显示 unsupported，不自动绕过探测。

```sh
git clone https://github.com/jackie-cqz/HarnessMesh.git
cd HarnessMesh
node scripts/start-tailnet.mjs
```

私密仓库使用 Mac 已配置的 GitHub 身份。脚本编译 CLI，从本机 Tailscale 状态选择确切 IP，创建独立 Node、项目和 Pi Agent，再保持前台运行。保持终端打开；Ctrl+C 正常停止本次 Daemon。不调用模型、不修改系统网络、SSH、Pi 登录配置或原项目。重复启动受 Daemon 锁保护，已有身份不会被重新生成。

默认目录和配置：

- Node：仓库内 `.hmesh-v0/tailnet-darwin/node`，在本机 Tailnet IP 和 `127.0.0.1` 分别监听 39880。
- 项目：`.hmesh-v0/tailnet-darwin/project`，仅 read/write/edit，允许核验 `out/health.json`，远端任务必须批准。
- Pi：解析 PATH 中 `pi` 的符号链接，使用运行脚本的 Node 解释器。
- 模型：引用 `~/.pi/agent/models.json` 的 Flash 定义。凭据引用本机 `~/.pi/agent/auth.json`，没有该文件时使用本机 `DEEPSEEK_API_KEY`；当前只支持 DeepSeek API key，不导入 OAuth。

路径不同可传展开后的绝对路径：

```sh
node scripts/start-tailnet.mjs \
  --pi /absolute/path/to/pi \
  --models "$HOME/.pi/agent/models.json" \
  --credential-source "$HOME/.pi/agent/auth.json"
```

Tailscale CLI 自动发现常规位置。Mac App Store 版使用 `/Applications/Tailscale.app/Contents/MacOS/Tailscale` 并设置 `TAILSCALE_BE_CLI=1`；也可传 `--tailscale-executable /absolute/path`。完整选项见 `node scripts/start-tailnet.mjs --help`。已有二进制可加 `--skip-build`。

启动输出和私有 `startup.json` 包含 Node Card、完整 fingerprint、Agent ID 和路径。Pi 登记只做版本探测，初始 `unverified` 正常。另开 Mac 终端，明确运行一次真实模型验证（会产生模型费用）：

凭据引用须包含 `deepseek` 下的 `type = "api_key"` 和有效 `key`；仅存在 auth.json 不代表符合该格式。本次首次验证曾因此失败，Mac owner 后来选择正确凭据引用并重新登记 Agent。手动重新登记后，以最新 Agent ID 和精确项目授权为准，旧 startup.json 中的 Agent ID 不会自动更新；不要把模型密钥复制到发起端。

```sh
HM="$PWD/target/debug/hmesh"
MAC_NODE="$PWD/.hmesh-v0/tailnet-darwin/node"
"$HM" --data-dir "$MAC_NODE" agent verify <Mac-Agent-ID> --allow-model
```

## Windows 启动和候选

在仓库内执行同一个脚本。Windows 只启动 Node，不自动新增或更改现有 DSH/Pi Agent：

```powershell
node .\scripts\start-tailnet.mjs
```

默认 Node 为 `.hmesh-v0/tailnet-win32/node`，端口 39880。两台机器的同一端口互不冲突；本机端口占用时用新的专用 `--root` 和 `--port`，不停止已有应用。Mac 防火墙若询问是否允许该二进制入站连接，按本次测试需求允许；Tailnet grants/ACL 也须允许两端各自的 39880/TCP。

候选发现不要求先初始化 Node：

```powershell
.\target\debug\hmesh.exe --json peers candidates
```

候选仅含主机名、DNS 名、OS、Tailnet IP 和在线状态，不导入 Tailscale 账户、node key 或 Mesh 信任。实际 endpoint 使用字面 IP；当前不以 MagicDNS 作为证书或 Node 身份。

## 配对和首条任务

先在两端核对 `node show` 的 node_id、endpoint 和完整 fingerprint。下列占位符必须替换为实际值；请求会过期，不复用旧请求。

Windows 请求 Mac：

```powershell
$hmesh = '.\target\debug\hmesh.exe'
$node = '.\.hmesh-v0\tailnet-win32\node'
& $hmesh --data-dir $node pair request --peer https://100.69.246.117:39880 --expect-fingerprint <Mac指纹>
```

Mac 批准请求，并反向请求 Windows：

```sh
"$HM" --data-dir "$MAC_NODE" pair approve <Windows请求ID> --expect-fingerprint <Windows指纹>
"$HM" --data-dir "$MAC_NODE" pair request --peer https://100.99.145.22:39880 --expect-fingerprint <Windows指纹>
```

Windows 批准反向请求：

```powershell
& $hmesh --data-dir $node pair approve <Mac请求ID> --expect-fingerprint <Mac指纹>
```

Mac owner 新建 `mac-policy.toml`，将 UUID 换成实际值：

```toml
[default]
action = "deny"
[[grant]]
peer = "<Windows-Node-ID>"
agent = "<Mac-Agent-ID>"
project = "tailnet-demo"
actions = ["agent.read", "task.submit"]
require_approval = true
```

```sh
"$HM" --data-dir "$MAC_NODE" policy --file "$PWD/mac-policy.toml"
```

Windows 发送作用于独立项目的任务：

```powershell
& $hmesh --data-dir $node agents list --peer <Mac-Node-ID>
& $hmesh --data-dir $node task send <Mac-Agent-ID> --peer <Mac-Node-ID> --project tailnet-demo --prompt 'Use write then read to create out/health.json containing {"status":"ok","model":"deepseek-v4-flash"}. Work only in the registered project.'
```

Mac 用 `task approve <Task-ID>` 批准，Windows 用 `task watch <Task-ID>` 等待。任务成功后，**Mac owner** 在本机执行 `task verify <Task-ID> --artifact out/health.json --expect-json '{"status":"ok","model":"deepseek-v4-flash"}'`；Windows 用 `task verification <Task-ID>` 读取经 mTLS 返回的核验结果。`task verify` 是执行端本机管理操作，不能在发起端代替 Mac 读取工件。链路暂断时使用相同 request ID 查询/补读，不重新创建副作用任务。

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
