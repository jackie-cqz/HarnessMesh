# Managed Pi 的 Mesh 工具入口

`hmesh-mcp` 是本机 stdio MCP 工具服务器。当前真实客户端为 **Pi 1.4.0 的原生 MCP 支持**，执行端可以是已经通过真实验证的 Pi、DSH 等 Mesh Agent。入口绑定一个正在 RUNNING 的 Managed Parent；通过 owner 配置启用。

## 启用

在 owner 登记的 Pi Agent TOML 的 `[adapter]` 中增加两个绝对路径：

```toml
mesh_server = "/absolute/path/to/hmesh-mcp"
mesh_scope = "/absolute/private/path/to/mesh-scope.json"
```

Windows 的服务器路径为绝对 `.exe` 路径。登记阶段只允许 Pi Adapter，且探测到 Pi 1.4.0 才接受该能力。其他 Agent 配置不填这些字段，继续沿用原有隔离运行方式。

scope JSON 由 owner 创建，示例值中的 UUID 需替换为真实配对节点、Agent 和项目：

```json
{
  "targets": [{
    "peer_id": "11111111-1111-4111-8111-111111111111",
    "agent_id": "22222222-2222-4222-8222-222222222222",
    "project_id": "allowed-worker-project"
  }],
  "max_submissions": 1,
  "max_timeout_seconds": 300,
  "max_output_bytes": 1048576
}
```

每个 scope 最多 16 个精确目标绑定，每个父任务最多 1–4 个子请求，单个子任务最多 600 秒。单条目标同时匹配 peer、Agent、project 才允许提交；子任务还必须通过执行端的原有信任、项目 grant 和审批。

Runtime 在当前 attempt 的私有目录生成一个固定内容的注册扩展，只显式加载 `builtin:mcp` 与该扩展，注册 `harnessmesh` stdio 服务器，并把 `mcp__harnessmesh__*` 加入本次工具列表。用户和项目扩展仍不自动加载。注册不写入用户的 `mcp.json`；MCP 子进程的 `DEEPSEEK_API_KEY` 被置空。

服务器由 Pi 使用以下形式启动，parent ID 和实例目录由 Runtime 决定：

```text
hmesh-mcp --data-dir ABSOLUTE_NODE_DIR --parent-task RUNNING_PARENT_UUID
```

## 五个工具

| 工具 | 参数 | 作用 |
|---|---|---|
| `mesh_list_peers` | `{}` | 仅列 scope 中已配对且可信的节点；连接状态标 not_checked |
| `mesh_list_agents` | 可选 peer_id | 查询 scope 中目标的实时 Agent 信息，仅返回允许的 Agent/project |
| `mesh_submit_task` | peer_id、agent_id、project_id、request_id、goal；可选 timeout_seconds、max_output_bytes | 以稳定 UUID 提交子任务，始终要求独立执行端审批 |
| `mesh_get_task` | task_id；可选 after、wait_seconds | 读取本父任务的子任务摘要及有限事件页，wait_seconds 最多 20 秒 |
| `mesh_cancel_task` | task_id | 请求取消本父任务的子任务；继续遵守原有停止证明与 UNKNOWN 语义 |

工具参数拒绝未知字段，不接受 executable、cwd、环境变量、审批 token 或任意 owner API 路径。协议和参数错误返回 JSON-RPC 错误；工具执行错误通过 `isError` 返回。任务本身失败与“查询任务”调用失败分开表示。

提交前在 SQLite schema 3 的 `tool_submissions` 中原子预留额度，保存 parent/request/child/peer 和完整请求的绑定。相同 request_id 仅可复用相同 payload；跨进程重启仍共享额度，冲突请求和新请求超额均拒绝。失联时返回稳定 task_id、未确定的观察结果与 automatic_retry=false；额度不会因失联释放，必须先检查既有任务。

每次调用重读 owner scope 并检查父任务仍在运行。get/cancel 仅允许属于该父任务、且仍在当前 scope 内的子任务；不能借任意 task_id 读取其他项目的任务。服务器的 owner TLS 身份和私有 token 仅用于内部回环 RPC；工具不暴露配对、策略修改、登记、批准或通用文件读取操作。

事件结果只来自已有脱敏事件接口，每次工具结果最多携带约 96 KiB 的事件。通过 `next_after` 继续读到空页；终态不表示此前的事件已全部消费。远端正文保持不可信数据语义。stdio 请求和完整响应各限制 512 KiB；stdout 只写单行 UTF-8 JSON-RPC。

## 已验证的链路

2026-10-29：Orion Pi/Flash 真实调用发现节点、发现 Agent、提交和查询工具，委派 Windows DSH/Pro。子任务先保持 PENDING_APPROVAL 且没有 attempt，由 Windows owner CLI 独立批准后执行。父子任务均 SUCCEEDED，子文件 nonce、父文件 nonce 和 artifact SHA-256 独立核对一致。

MCP cancel 的实际模型调用不计入这次四工具 smoke；其任务归属拒绝路径、协议边界和后端取消入口由自动化测试覆盖。DSH 作为 MCP 发起端的接入尚未核查。工作目录、工具 allowlist 与 scope 均不构成 OS 用户隔离：Pi、MCP 服务器和当前 owner 仍具有各自进程的系统权限。

完整证据和 SSH 断线测试见 [双向与 MCP 验收](ssh-bidirectional-mcp.md)。普通自动化测试不调用模型；真机脚本为 `scripts/phase12-session.ps1` 和 `scripts/phase12-remote.py`。

协议实现锁定以下官方规范的 stdio/tools 子集，使用现有 Rust 依赖实现，不增加额外 SDK 或第三方 crate：

- [MCP 2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [Lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [Tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)

Pi 加载参数和原生注册 API 以 Orion 已安装包的 `docs/cli.md`、`docs/mcp.md`、`docs/extensions.md` 及实际执行为准。
