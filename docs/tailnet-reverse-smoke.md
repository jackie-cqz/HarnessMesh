# Mac → Windows 的真实 Tailnet / DSH 检查点

2026-10-30，Mac Node 经现有 Tailnet/mTLS 配对向 Windows DSH 发起真实 write/read 任务。Windows 核对来源、项目和内容后在本机批准，任务成功；owner 的确定性核验与独立文件 JSON/hash 检查均通过。全程不使用 SSH。

## Windows 执行端结果

| 项目 | 实际值 |
|---|---|
| 发起 Node | Mac `b42a0802-eee1-4799-bc3a-fb66e19a4e71` |
| 执行 Node | Windows `a88af118-32ee-4b11-8ad6-9e04be5c36f6` |
| Agent | `windows-dsh-pro` / `60c5d1af-8e83-482f-afdc-e720f4be91e3` |
| Harness / 模型 | DSH `0.2.0-rc.2` / `deepseek-official` / `deepseek-v4-pro` |
| 项目 | `tailnet-reverse`，仅允许指定 Mac Node 和 Agent |
| 本机模型验证 | `2bf557fa-39bb-4f81-8f19-8cfeeb71f20f`，SUCCEEDED |
| 跨机请求 | `22d08d50-f105-4535-9c40-5a7ce2fd9836` |
| 跨机任务 | `71ce832e-377c-4544-a6d5-cd6765a31cab` |
| Attempt | `d021b156-2fa4-4003-ab65-4f7901384b06` |
| 批准前 | PENDING_APPROVAL，无 attempt |
| 执行结果 | SUCCEEDED，DSH write/read 均有开始与完成事件 |
| 产物 | `out/health.json`，168 字节 |
| 核验 | 5 个指定字段通过，独立 JSON 精确匹配及 SHA-256 一致 |
| 持久化 | 只读 SQLite 确认该请求绑定和执行 attempt 均各 1 个 |
| 事件 | Windows 保存 14 条事件；没有把该值当作 Mac 已补读的证明 |

SHA-256：`a20992b96633dd6c58bb8839739e3b53e80d6be25365c340545b319f43b97ce4`。

结果已写入原任务核验记录，Mac 发起 Node 可通过既有 mTLS Task verification 接口读取。Windows Agent 回到 idle，Daemon 保留运行供 Mac 读取结果。

## Mac 发起端读回

Mac 已通过 mTLS 读回同一任务的 SUCCEEDED 状态、原 attempt 和 Windows 发布的确定性核验结果；五个字段全部 passed，工件 SHA-256 与 Windows 报告一致。`task watch` 实际读到 seq 1–13，包含审批、write/read、工件与终态；随后通过 `task events --after 0` 保存当前返回的事件快照。不同采集时点的事件数量不混用。原 CLI 基于提交 cdc2c1c 在 Mac 使用 Rust stable 1.99.1 编译成功，尚未执行完整 workspace 检查。

Mac 原始读回证据见 [latest-tailnet-reverse-mac.json](evidence/latest-tailnet-reverse-mac.json)。

## 工件范围与边界

接收端准备时声明的工件为 `reverse.json`；Mac 的实际任务明确要求同一独立项目中的 `out/health.json`。Windows owner 在执行前对齐了精确工件声明，然后批准任务；没有扩展项目、工具或 peer 授权，也没有改变 Mac 提交的五字段 JSON 断言。

本报告证明 Mac 来源的可信入站任务、Windows 审批与执行端独立核验。单条请求只有一个 attempt 的观察，不等于已测试 Mac 重复提交。Mac 侧读取结论与事件监听已在上节独立收集；重复请求、专项游标分页以及反向执行中断网/取消/重启恢复仍未验证。

此前的 [Windows → Mac Pi/Flash 检查点](tailnet-mac-smoke.md) 已独立记录。两种任务方向都已有真实执行证据；Mac 完整工作区测试、受管前后端服务、Codex 与三机 Gateway 等仍未完成，不能据此宣称完整 V0.1/V0.2。

结构化证据见 [latest-tailnet-reverse.json](evidence/latest-tailnet-reverse.json)，包含实际请求绑定、attempt、工具事件、输出额度及二进制/依赖 hash，不含模型凭据。
