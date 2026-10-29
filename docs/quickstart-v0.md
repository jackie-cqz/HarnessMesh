# V0 本机双节点快速开始

当前验收环境为 Windows x64、PowerShell 7、Rust 1.90.0。Node A/B 是两个独立的 `hmesh daemon start` 进程，通过回环 TLS 连接。真实运行使用本机 Pi 0.84.2、DSH 0.2.0-rc.2、OpenCode 1.18.20；版本不匹配时 Adapter 明确拒绝。

## 一次复现完整验收

在仓库根目录运行。普通检查不会调用模型：

```powershell
pwsh -NoProfile -File .\scripts\e2e-local.ps1
```

真实联调需要该 PowerShell 进程已有授权的 `DEEPSEEK_API_KEY` 环境变量，以及上述三个本机 Harness。不要把密钥写进 TOML、命令正文、日志或证据文件。显式执行：

```powershell
pwsh -NoProfile -File .\scripts\e2e-local.ps1 -RunRealHarnesses
```

脚本先执行 fmt、Clippy、workspace tests 和构建，然后在全新的 `.hmesh-v0/acceptance-<uuid>/` 下初始化两套身份、数据库、回环端口、运行数据与三个测试项目。它验证三种 Harness 各自的真实模型任务，再执行 A→B/DSH、B→A/Pi、A→B/OpenCode，检查审批、默认拒绝、工件内容、独立 SHA-256、请求去重、事件补读与重启后查询。端口自动选择当前空闲端口，协议与固定 38880/38881 相同。

默认模型为本次已验证的 `deepseek-v4-pro`，可显式传 `-Model`；替换模型需要重新验证。脚本通过 `.cmd` 找到 npm 包，再使用实际 Node 入口/原生 exe。它只停止自己启动的进程，保留测试目录和诊断记录；运行结果写入 `docs/evidence/latest-p0-smoke.json`。真实调用会消耗该模型账户的额度。

依赖安全审计独立联网执行，只发送公开 crate 名称和版本：

```powershell
cargo +stable run --locked -p xtask -- audit
```

结果绑定 Cargo.lock 的 SHA-256。API 不可用、响应不完整或有漏洞命中都会失败；离线普通测试不会把“未审计”当作通过。

## 手动启动和配对

下面的变量在每个终端各定义一次；先在控制终端构建并初始化。数据目录必须是专用空目录；重复 init 保留原身份。

```powershell
$hmesh = Join-Path (Get-Location) 'target/debug/hmesh.exe'
$aDir = Join-Path (Get-Location) '.hmesh-v0/manual-a'
$bDir = Join-Path (Get-Location) '.hmesh-v0/manual-b'
cargo +stable build --locked -p hmesh-cli
& $hmesh --data-dir $aDir init --name local-a --port 38880
& $hmesh --data-dir $bDir init --name local-b --port 38881
```

终端 A 运行 `& $hmesh --data-dir $aDir daemon start`，终端 B 运行 `& $hmesh --data-dir $bDir daemon start`。这是前台命令，保持两个终端打开。在控制终端配对：

```powershell
$a = & $hmesh --json --data-dir $aDir pair show | ConvertFrom-Json
$b = & $hmesh --json --data-dir $bDir pair show | ConvertFrom-Json
$pair = & $hmesh --json --data-dir $aDir pair request --peer $b.endpoint --expect-fingerprint $b.fingerprint | ConvertFrom-Json
& $hmesh --data-dir $bDir pair approve $pair.request_id --expect-fingerprint $a.fingerprint
& $hmesh --data-dir $aDir pair approve $pair.request_id --expect-fingerprint $b.fingerprint
& $hmesh --data-dir $aDir peers list
```

指纹应从目标实例本机 `pair show` 核对。信任与任务授权分开，配对完成后仍默认拒绝任务。

## 登记 Agent、验证、授权和委派

复制 `examples/single-machine-task/` 对应的 TOML 到自己的测试配置中，将项目根目录、实际入口、provider/model 改为本机的绝对路径和可用值。创建项目目录后登记。例如 Pi 放在 A，DSH/OpenCode 放在 B：

```powershell
$registered = & $hmesh --json --data-dir $bDir agent register --config C:/absolute/dsh.toml | ConvertFrom-Json
$id = $registered.agent_id
$probe = & $hmesh --json --data-dir $bDir agent verify $id --allow-model | ConvertFrom-Json
& $hmesh --data-dir $bDir task watch $probe.task_id
& $hmesh --data-dir $bDir agents list
```

注册仅检查版本，Agent 保持 `unverified`。只有真实验证成功才接受 peer 任务。`doctor` 查看所选实例/Daemon；`agents list` 中的 probe 区分已安装、版本匹配、实际运行和未测试能力。

编辑示例 `policy-b.toml`，把 peer 改成 `$a.node_id`，project/agent 对应 B 上的注册记录，然后应用。该策略同时授予最小的 Agent 可见性与任务提交权限，每次仍需要 B 本机批准：

```powershell
& $hmesh --data-dir $bDir policy --file C:/absolute/policy-b.toml
& $hmesh --data-dir $aDir agents list --peer $b.node_id
$requestId = [guid]::NewGuid().ToString()
$task = & $hmesh --json --data-dir $aDir task send $id --peer $b.node_id --project dsh-project --request-id $requestId --prompt '创建 out/health.json，包含 status=ok 和 harness=dsh，随后读回验证。只操作授权项目。' | ConvertFrom-Json
& $hmesh --data-dir $bDir task approve $task.task_id
& $hmesh --data-dir $aDir task watch $task.task_id
& $hmesh --data-dir $bDir task verify $task.task_id --artifact out/health.json --expect-json '{"status":"ok","harness":"dsh"}'
& $hmesh --data-dir $aDir task verification $task.task_id
```

`task verify` 只接受 owner 事先声明的 `artifact_paths`；Harness 成功和断言通过分别记录。重试提交时复用相同 request-id、目标与原始 payload；改变内容必须使用新 request-id。观察超时后先 `task show/watch`，不要盲目创建新任务。

`task reject` 由执行端 owner 操作；`task cancel` 可由原发起端操作。已经启动的任务需确认受控进程树停止才会成为 `CANCELLED`，否则 `UNKNOWN`。V0 不接管已有交互 Session，不自动重试结果未知的副作用任务。

完成后在控制终端停止两端：

```powershell
& $hmesh --data-dir $aDir daemon stop
& $hmesh --data-dir $bDir daemon stop
```

实例文件归初始化的 Windows 用户所有。不要混用工具沙箱账户和实际桌面账户的数据目录，也不要通过放宽 ACL 解决账户不匹配。数据目录保留私钥、owner token、任务正文和审计，应按私有数据保存。V0 成功只证明本机 Windows 路径；跨电脑、跨 OS、Tailscale、Codex 和 MCP 由 V0.1 独立验收。
