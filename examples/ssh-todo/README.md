# Mesh Tasks：Pi 服务端 + DSH 客户端

一个通过 SSH 联调的简单待办板。Orion 上的 Pi / DeepSeek V4 Flash 编写服务端，Windows 上的 DSH / DeepSeek V4 Pro 编写客户端。两端按同一份 `api-contract.json` 工作；业务进程使用 Node.js 内置模块，无 npm 依赖。

```text
浏览器 http://127.0.0.1:5173
  → Windows 客户端 /api 代理（DSH 编写）
  → Windows 127.0.0.1:18080
  → SSH LocalForward
  → Orion 127.0.0.1:8000（Pi 编写）
  → Orion 独立 JSON 数据文件
```

代码生成阶段通过 Rust HarnessMesh 的真实 Managed Run 执行，服务端任务经 SSH 内的 HTTPS/mTLS 委派，并由 Orion owner 批准。模型任务和前后端服务是不同进程；日常使用页面不会调用模型。

## 使用当前已部署的示例

服务启动后打开 `http://127.0.0.1:5173`。可以新增待办、勾选完成、刷新读取服务端数据。数据保存在 Orion 本次专用目录的 `data/todos.json`，不是浏览器 localStorage。

在仓库根目录用 PowerShell 7 管理本次记录的服务：

```powershell
pwsh -NoProfile -File .\examples\ssh-todo\stop.ps1
pwsh -NoProfile -File .\examples\ssh-todo\start.ps1
```

这些命令读取 `.hmesh-v0/todo-active.txt` 指向的私有运行记录，核对进程身份后只停止/启动本次前端、Orion 后端和专用应用隧道。停止后代码和 Orion 数据保留。需要使用最初初始化实例的 Windows 账户，并保持 `cqz_Orion` SSH 可达。

## 本次真实验收

Orion 服务端 10 项测试、本机客户端 4 项测试全部通过。独立 HTTP 验收覆盖新增、完成、读取、非法参数、未知 ID 与超大请求；浏览器也实际执行新增、勾选、刷新和页面重载。已核对 Orion JSON 文件，并重启后端确认两条验收待办和完成状态保留。

2026-10-28 的 DSH 生成任务为 `SUCCEEDED`。Pi 的首次任务触发累计输出额度，后续独立任务写完全部服务端文件后触发当时的 64 KiB JSONL 单条记录保护，因此原任务状态仍为 `FAILED`。本目录服务端是这些真实 Pi 产物的原样导出，文件 hash 与 Orion 一致；后续测试通过不代表该原始模型任务成功。

2026-10-29 已修复该原生消息限制：Pi 单条最多 4 MiB，内部事件仍限制 64 KiB，正文脱敏后分段保存，累计额度与超时继续生效。新的隔离 Pi/Flash 任务重新生成完整服务端并正常 `SUCCEEDED`，实测最大消息 375,131 字节；新生成服务端 14 项测试通过，原前后端 14 项测试与 HTTP/持久化也重跑通过。原任务终态和本目录原代码保留，新生成文件留在私有回归目录。见 [修复回归证据](../../docs/evidence/latest-pi-output-regression.json) 和 [ADR 0003](../../docs/adr/0003-native-output-limits.md)。

详情见 [验收记录](../../docs/evidence/latest-todo-integration.json) 和 [页面截图](../../docs/evidence/ssh-todo.png)。代码生成用的 Daemon/Mesh 隧道已停止，应用前后端和专用 SSH 应用转发继续运行供试用。

## 独立运行代码

`server/` 是从 Orion 导出的 Pi 实际生成文件；`client/` 是本机 DSH 实际生成文件。分别进入目录即可运行：

```sh
node --test server.test.mjs
node server.mjs
```

```sh
node --test client.test.mjs
node serve.mjs
```

后端默认监听回环地址 8000，`DATA_FILE` 指定数据文件；客户端默认监听回环地址 5173，`UPSTREAM` 默认是 `http://127.0.0.1:18080`。跨机使用时需保留对应 SSH 转发。两个子目录各有 Harness 编写的运行说明。

## 生成与验收入口

- `backend-task.md` / `frontend-task.md`：交给两个 Harness 的真实任务说明。
- `scripts/todo-session.ps1` / `scripts/todo-remote.py`：本次隔离实例、审批、生成、导出、测试、应用启动和停止的受控 runner。
- `scripts/check-todo.mjs`：独立的真实 HTTP 断言与后端重启后持久化检查。
- `.hmesh-v0/`：私有身份、任务记录、模型运行日志及服务进程状态，不提交。

脚本目前针对已核查的 Orion 环境，包括 NVM Node 24 和已有 Linux hmesh 构建目录；不是通用安装器。服务仅监听回环地址，SSH 为手动管理的专用转发；本示例不代表自动服务发布、Tailscale 或完整 V0.2 已完成。
