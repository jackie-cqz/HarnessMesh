# 受管 HTTP 服务、SSH 隧道与联调（SSH 检查点）

Daemon 现在负责独立资源的租约、进程树、健康状态和有限重启。服务、SSH 控制隧道和 HTTP connection 均由本机 owner 创建；模型的五个 MCP 工具没有获得这些管理权限。完整 V0.1/V0.2 门禁保持独立，当前范围是 Windows ↔ Linux 的 HTTP/SSH 检查点。

## 使用顺序

所有命令先带 `hmesh --data-dir <绝对 Node 目录>`；下列 `--config` 文件为 JSON，未知字段被拒绝。需要已运行、已配对的两个 Node，且后端项目已有登记的 Agent。

1. 网关 owner 运行 `tunnel add --config tunnel.json`，管理 SSH 精确转发。
2. 后端 owner 运行 `service publish --config service.json`，登记限定 peer/项目/端口的服务。
3. 发起端运行 `service list --peer <Node UUID>`，再 `service connect --config connection.json`。
4. `connection list` 给出本机 `127.0.0.1` HTTP endpoint，供前端的 UPSTREAM 或测试使用。
5. 发起端运行 `integration start --config integration.json --allow-model`；`integration show/watch <UUID>` 查看固定契约测试及修复进度。
6. `service revoke <UUID>`、`connection revoke <UUID>`、`tunnel revoke <UUID>` 回收指定资源。`service/tunnel restart <UUID>` 仅重启仍有效的租约，不延长授权时间。

CLI 同时支持 `--json`。管理列表只展示资源状态，不返回启动参数或环境变量值。

## Service

```json
{
  "id": "11111111-1111-4111-8111-111111111111",
  "project_id": "backend",
  "name": "backend-api",
  "target_port": 8011,
  "allowed_peer_ids": ["22222222-2222-4222-8222-222222222222"],
  "allowed_restart_peer_ids": ["22222222-2222-4222-8222-222222222222"],
  "lease_seconds": 1800,
  "health_path": "/api/health",
  "request_timeout_seconds": 30,
  "process": {
    "executable": "/absolute/path/to/node",
    "cwd": "/absolute/registered/backend",
    "args": ["/absolute/registered/backend/server.mjs"],
    "env": {"PORT": "8011", "DATA_FILE": "/absolute/private/todos.json", "NODE_LABEL": "Linux"}
  }
}
```

Windows 的 executable/cwd 使用实际绝对 Windows 路径。cwd 必须等于登记项目的 root；远端不能改变它。启动前拒绝已占用的目标端口，不接管现有 PID。`process: null` 可以只登记外部服务，此时撤销只撤销访问，Daemon 不停止或重启外部进程。

`allowed_peer_ids` 是服务 owner 的精确项目/服务授权，与 task grant 分开。restart peer 必须同时在访问名单中；仅委派重启这一份固定启动配置的权限。服务卡只公开 id、项目、名称、协议、租约、健康和 revision，不公开目标端口、启动命令、凭据或环境。

## SSH Tunnel

```json
{
  "id": "33333333-3333-4333-8333-333333333333",
  "project_id": "backend",
  "name": "linux-control",
  "ssh_executable": "C:/Windows/System32/OpenSSH/ssh.exe",
  "hostname": "linux.example",
  "user": "developer",
  "port": 22,
  "identity_file": "C:/Users/you/.ssh/id_linux",
  "known_hosts_file": "C:/Users/you/.ssh/known_hosts",
  "forwards": [{"direction": "local", "bind_port": 39181, "target_port": 39181}],
  "lease_seconds": 1800
}
```

这些字段由 owner 明确填写，不由模型提供 SSH argv。每个 tunnel 最多两个 local/remote forward，两侧目标和监听地址固定 `127.0.0.1`，无 SOCKS、CONNECT 或任意目标主机字段。使用绝对 ssh、密钥、known_hosts 路径，专用空配置屏蔽个人 SSH config 中额外的转发、ProxyCommand、LocalCommand 等。强制 BatchMode、IdentitiesOnly、StrictHostKeyChecking、ExitOnForwardFailure；不写个人 SSH config/known_hosts，不改变已有 7892 转发。SSH server 端也必须允许所需回环转发；remote direction 的实际监听范围需由 server 管理员保持为回环。

控制路径继续由执行端独立 Node 的 mTLS 身份认证。SSH 进程存活只标记 `ssh_process_running`，不等同于远端服务健康。首启加最多三次重试，退避 2/4/8 秒；耗尽为 `failed_needs_input`，owner 检查后可显式 restart。没有永久重连风暴。

## Connection 与 HTTP 边界

```json
{
  "id": "44444444-4444-4444-8444-444444444444",
  "peer_id": "55555555-5555-4555-8555-555555555555",
  "service_id": "11111111-1111-4111-8111-111111111111",
  "local_port": 18081,
  "lease_seconds": 1800
}
```

`local_port: 0` 可分配临时端口。有效期取本地申请与远端服务到期时间中较早者。每个请求使用当前 peer 信任和服务白名单，返回缓冲数据前再次检查授权。每个本地 connection 最多八个并发请求，HTTP 请求/响应各最多 256 KiB；无代理环境、自动重定向或网络错误自动重放。仅转发 method、path/query、content-type 和正文；不会携带浏览器 Cookie、Authorization 或任意 header。WebSocket、SSE、通用 TCP 和需额外 header 的应用另行实现。

服务 HTTP 操作预算 `request_timeout_seconds` 默认为 10 秒，owner 可设为 1..30 秒。Mesh 调用预算在其上增加 5 秒，独立契约 GET 测试再留出调用余量；应用前端和测试客户端需要配置更大的预算。示例为服务 30 秒、Mesh 35 秒、前端/客户端 45 秒。Linux 的共享存储 fsync 曾测到 8.3/10.2 秒，原固定 5 秒代理使写请求返回 503，失败记录保留；延长明确预算不会重放原写请求。回归测试覆盖 1 秒超时、3 秒成功以及每次写入只发生一次。

隧道暂时失联时 endpoint 保留到租约结束，调用失败且不会重放；恢复 SSH 后下一次新请求重新认证。明确撤销/到期关闭 endpoint，不能通过 reconnect 复活旧租约。已经发出的写请求若失联，其业务结果可能未知，调用方须查询业务状态。

## IntegrationRun

JSON 配置对应 `hmesh_core::managed::IntegrationSpec`：id、connection_id、backend_peer_id/agent_id/project_id/service_id，完整 `contract` 字符串及其原始 UTF-8 字节 SHA-256、revision，GET 测试数组和修复预算。

```json
"tests": [
  {"name": "health-fields", "path": "/api/health", "expected_status": 200,
   "equals": {"/status": "ok", "/service": "linux-todo-api", "/node": "Linux"}}
]
```

equals 的键为 JSON Pointer，值为精确预期。当前测试计划为确定性 GET/JSON 断言，固定保存，不执行模型提供的测试命令。contract 至多 32 KiB、整个测试计划至多 16 KiB、最多 16 个请求，max_fix_cycles 为 0/1/2，单次修复 10..600 秒并显式指定输出额度。同一 Node 一次仅运行一个 IntegrationRun。保留契约 hash、测试计划 hash、响应 hash/长度与断言结果，不保存响应正文或原始服务日志。

测试失败后先持久化新的 repair UUID 和轮次，再复用原任务引擎向精确后端 Agent 提交。远端原有任务审批仍生效：没有预先授权时停在 `waiting_approval`，需执行端 owner 批准；自动模式只在 owner 已为这份独立项目配置相应 grant 时成立。模型没有 approve/policy 权限。仅任务 `SUCCEEDED` 后请求 owner 预先委派的固定服务 restart，等待新 revision 健康，再运行原测试。

状态包括 testing、repair_submitting、waiting_approval、repair_running、restarting_service、verified、needs_input、cancelled。失败超限、失联、任务未知或服务重启未确认均停止；不换 UUID 猜测重跑。cancel 请求取消已知子任务，但最终是否停止仍以任务状态为准。终态由数据库条件更新保护，迟到 worker 不会覆盖 cancelled/verified/needs_input。

## 生命周期与当前限制

租约为 5..3600 秒，不隐式续期；过期后发布/连接须使用新 id。每种资源当前最多保存 16 份登记，IntegrationRun 最多 64 份，达到限额明确拒绝。SQLite schema 4 保存配置/租约/报告，schema 2/3 的旧任务与委派记录保留。

正常关闭 Daemon 会停止自己启动的进程树、关闭监听、停 SSH，并将资源设为 disabled。Daemon 再启动保留可查询配置和报告，但不自动恢复旧租约或重放联调。Windows Job Object 负责所属进程树，Unix 使用独立进程组；Unix Daemon 被 SIGKILL 后可能留有子进程，重启不会按历史 PID 猜测杀进程或接管占用端口。此时 owner 检查实际进程再创建新租约。尚未实现 OS 开机常驻安装、进程崩溃后的无缝接管、三机 Gateway 或通用 TCP proxy。

服务配置中的 env 是 owner 提供的本机进程环境，存于私有数据库，当前没有专门的 service credential store；勿将秘密写入可分享示例。与 Harness 相同，这些进程使用 owner 的 OS 权限，目录和工具配置不构成 OS 沙箱。

## 真实验收脚本

`scripts/managed-smoke.ps1` 将既有 SSH/Todo 目录作为只读模板，新建两端独立 Node、项目和数据目录。控制端口 39180/39181，后端 8011，本机代理 18081，前端 5174；原 8000/18080/5173 和 7892 保持原用途。

依次执行 build、build-status；通过后执行 prepare -RunRealHarness、status、services、integrate -RunRealHarness、status、collect、cut-tunnel、check-tunnel、revoke、cleanup。prepare 会调用真实 Pi/DSH，integrate 最多调用两次真实修复。只读 status 可重复执行；提交动作已有 UUID 时拒绝重新提交。完整诊断在被忽略的 `.hmesh-v0/`，公开报告需人工筛选后保存脱敏证据。

prepare 默认 `-ModelNetwork direct`，只影响新建隔离 Agent 的模型请求环境；需要原代理时显式指定 `original-proxy`。当前 Linux 实测官方 API 直连可用、原代理请求超时，未修改用户代理或个人认证。已完成的验收与恢复步骤在 [结构化证据](evidence/latest-managed-services.json) 中，最后的真实 DSH 客户端按原字节导出为 [contract-check.mjs](../examples/managed-http/contract-check.mjs)。collect 包含真实 CRUD、后端/客户端测试与受管重启后的持久化检查，会写入独立测试数据。
