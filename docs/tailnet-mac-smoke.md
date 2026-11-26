# Windows → Mac 的真实 Tailnet / Pi 检查点

2026-10-30，Windows 发起任务，经 Tailscale + TLS 1.3/mTLS 到 Mac，Mac 本机批准后由 Pi 1.4.0 / `deepseek-v4-flash` 执行 write/read。没有使用 SSH。节点、项目、Agent 与执行批准分别校验，完整 V0.1/V0.2 仍未完成。

## 实际结果

| 项目 | 结果 |
|---|---|
| 执行端 | Mac `100.80.20.12:39880`，Node `b42a0802-eee1-4799-bc3a-fb66e19a4e71` |
| Agent | `mac-pi-flash-key` / `5583c679-a688-4653-92c1-ee960fde64ea` |
| 项目 | `tailnet-demo`，仅向指定 Windows Node 和新 Agent 授权 |
| 请求 | `7545688d-ce3a-4679-9e36-66161c9599c1` |
| 任务 | `ac2697bc-611a-4d3f-a800-06f1cc5f96e8` |
| Attempt | `a0ecd2df-9ede-47f2-93c5-f6523aca63be` |
| 执行 | `SUCCEEDED`，收到 Pi 的 write/read 开始与完成事件 |
| 批准前 | `PENDING_APPROVAL`，无执行 attempt |
| 工件 | `out/health.json`，154 字节 |
| JSON 核验 | `status`、`harness`、`model`、`request_id`、随机 `marker` 共五项通过 |
| 请求去重 | 同一 request/task ID 重发返回同一 attempt，事件历史不变 |
| 事件补读 | 共 23 条事件，after=12 返回的 11 条与保存的尾部 event ID 完全一致 |
| 发起端恢复 | Windows 后台进程退出后，以原身份/目录重启，仍读到同一已完成任务及 attempt |

SHA-256：`f0d12fa09dbcced7525a4fc6d85ec10abaded1575a093408986aab1810efa4fa`。

Mac owner 的确定性核验结果通过 mTLS 返回 Windows；它与运行器的 artifact 事件及 owner 报告的 hash 一致。模型名来自 owner 选择的 Agent 配置，JSON 中的 model 字段本身不承担模型身份认证。凭据和模型配置文件未导出。

## 网络与权限

独立 Node HTTPS 检查覆盖确切 IP 的证书 SAN、明确指纹、TLS 1.3、双向客户端证书、未认证注册表拒绝、公开 owner API 拒绝和未登记项目拒绝。检查使用全新 TLS 连接，避免 HTTPS 连接复用影响“错误指纹”探针。应用层检查不需要 SSH 或远端 owner token。

## 保留的失败与范围

首次 Mac 本机模型验证 `080aeb56-d239-4274-8346-c5cf93a9e3ca` 因凭据引用不含所需 DeepSeek API key 失败，发生在 Harness 启动之前。Mac owner 修复本机凭据引用、重新登记新 Agent 并更新精确 Agent grant；新本机验证任务为 `bd1991a5-5353-4d57-8929-549a72f4b57c`，注册表显示 `passed_real_run`。旧失败记录未覆盖，旧 Agent 没有沿用新授权。

最后整理记录时，Windows 后台进程已经退出，原因未确定；恢复只读取原任务，没有重提模型任务。这验证完成任务的发起端恢复，不代表已经测试“任务执行中断网/重启”。

本次确认 Mac CLI/Daemon 能运行真实任务；未独立收集 Mac 二进制 hash、源码 commit 或完整 fmt/Clippy/工作区测试日志。本报告不覆盖 Mac→Windows 反向任务，后续的 Windows 执行端结果见 [反向检查点](tailnet-reverse-smoke.md)。Mac 受管 HTTP 前后端联调、这两台设备上的服务撤销/执行中断网、Codex 和三机 Gateway 尚未验收。两个 Daemon 保留供后续联调，不能视为已安装开机常驻服务。

结构化结果、身份、工具事件、hash、历史偏差与验证边界见 [latest-tailnet-mac.json](evidence/latest-tailnet-mac.json)。原 [单机 Tailnet 检查](evidence/latest-tailnet-local.json) 保留为独立历史记录。配置步骤见 [直连 quickstart](quickstart-tailnet.md)。
