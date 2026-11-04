# 可复用开发流程与本机运行管理

本轮增加任务模板、持久化阶段工作流、脱敏诊断、离线保留管理、用量/预算和 Harness 兼容性检查。数据库为 schema 8；升级已有节点前先停止旧 Daemon 并使用旧版本创建私有备份。新面板不会替换运行中的旧实例。

普通开发与回归使用临时节点及明确的 `hmesh-fixture`，不调用真实模型。`template run` / `workflow start` 会创建待审批任务；执行端批准后才可能调用真实模型。原有显式 `agent verify --allow-model` 仍是独立操作。

## 模板

面板“开发流程”→“从任务表单保存模板”：选好节点、Agent、项目、目标和限额，再保存名称、可选工具限制及 JSON 工件断言。模板名称使用 ASCII 字母、数字、点、横线或下划线。目标可含 `{{goal}}`，它只替换成本次输入，不执行 shell、表达式或嵌套插值。

```sh
hmesh --data-dir NODE template save --file template.json
hmesh --data-dir NODE template list
hmesh --data-dir NODE template run TEMPLATE_UUID --revision 1 --goal '具体任务' --run-id FIXED_RUN_UUID
```

模板 JSON 示例（先替换示例 UUID 和项目）：

```json
{
  "id": "00000000-0000-4000-8000-000000000001",
  "revision": 1,
  "name": "implement-and-check",
  "peer_id": "00000000-0000-4000-8000-000000000002",
  "agent_id": "00000000-0000-4000-8000-000000000003",
  "project_id": "my-project",
  "goal": "实现以下要求并保留可核验结果：{{goal}}",
  "limits": {"timeout_seconds": 600, "max_output_bytes": 16777216},
  "expected_tools": ["read", "write", "edit"],
  "verification": {"artifact": "out/health.json", "expected": {"status": "ok"}}
}
```

同一 UUID 的修订从 1 连续递增。已保存修订不可覆盖，相同内容重发幂等。库最多保存 256 个修订，总正文最多 512 KiB。`expected_tools=null` 表示沿用 Agent 工具；填写列表则要求执行端当前配置精确匹配，它不会授予新工具。Agent 工件 allowlist 和当前项目授权仍优先。

单模板运行也保存为一个阶段的工作流，携带独立 run/task/request ID。重复使用同一个 run ID 和相同内容会读回原运行；修改内容后使用旧 ID会被拒绝。面板在发送前把原创建操作保存进当前标签页 sessionStorage；响应丢失后刷新仍可“检查原开发操作”，不会自动换 ID 创建另一组任务。

## 多阶段工作流

面板可选择模板排列 1–8 个阶段，每阶段可设置 0–2 次修复。所有可能任务的执行时限合计不能超过 owner 声明的 `max_total_timeout_seconds`，总上限 14400 秒。展开后的初始运行最多 512 KiB。

```sh
hmesh --data-dir NODE workflow start --file workflow.json
hmesh --data-dir NODE workflow list
hmesh --data-dir NODE workflow show RUN_UUID
hmesh --data-dir NODE workflow cancel RUN_UUID
hmesh --data-dir NODE workflow accept RUN_UUID
hmesh --data-dir NODE workflow archive RUN_UUID
hmesh --data-dir NODE workflow list --archived
```

`workflow.json` 为 `{id,name,stages,max_total_timeout_seconds}`；每个阶段为 `{name,template,goal,repair,max_repairs}`，`template` / `repair` 嵌入完整模板修订，必须与已经保存的同 ID/修订完全一致；更改内容须先保存新的修订，不在执行中解析可变的“最新版”。不需要修复时 `repair=null,max_repairs=0`。

创建时先持久化全部固定 task/request ID，再向执行端提交。每个新任务都要求独立审批；工作流、模型和原任务批准都不能批准后续任务。执行端成功且可选 JSON 断言通过后，才进入下一阶段；执行成功与验收通过分别记录。

执行失败或断言未通过时，可在预先声明的限额内创建修复任务。修复目标必须保持原 peer/project，且始终核验原阶段的同一断言；修复模板自带的断言不能替换它。拒绝、过期、取消和 `UNKNOWN` 不触发自动修复。次数用完进入 `needs_input`。全部阶段通过进入 `review`，owner 审阅后显式确认 `completed`；确认不合入 Git diff。

主列表最多保留 64 个工作流记录。完成/取消后可显式归档释放名额，历史仍可按原 UUID 查看，原创建请求仍去重；归档列表显示最近 1000 条摘要并注明截断。未结束或需要处理的工作流不能归档。

正常重启后继续观察原任务，必要时仅恢复已持久化的原请求。运行中的执行端任务重启后变为 `UNKNOWN` 时，工作流停下等待处理，不重放模型调用。网络暂不可达时保留原 ID/最近错误并继续只读观察；权限、配置等错误进入 `needs_input`。取消使用版本比较更新，旧 worker 不能覆盖新取消状态。

各阶段沿用各自 Agent 的目录模式。共享项目的阶段可看到先前写入；独立 worktree 之间不自动复制文件或合并修改。需要共享代码状态的实现/测试阶段应配置同一共享项目，或由 owner 审阅并合入独立 worktree 后再开始下一组流程。工作流提供持久顺序编排，不提供远程 shell 或通用 DAG 执行器。

## 脱敏诊断

```sh
hmesh --data-dir NODE doctor --report
hmesh --data-dir NODE doctor --report --out NEW_REPORT.json
```

面板“诊断与用量”支持导出同一结构化报告。报告包含节点 ID、网络模式、Agent 可执行文件/凭据引用有效性、队列等待原因、最近失败的错误码/trace/分类及处理建议。超时、权限、协议、预算和不确定执行分别呈现；工作流的工件验收失败单独列出。

报告不包含 prompt、输出正文、原始错误消息、项目绝对路径、模型密钥或凭据文件内容。引用检查会在本机读取已登记的引用，仅导出有效性布尔值，不调用模型。默认 `doctor --peer UUID` 保留原有实际连通诊断；`--report` 是可导出的本机结构化摘要，不主动探测所有远端。报告文件必须是新文件，Unix 权限为 0600。

## 磁盘与保留管理

在线面板显示快照、diff、事件及 SQLite 页/空闲页占用。完整运行目录统计和清理要求 Daemon 离线：

```sh
hmesh --data-dir NODE daemon stop
hmesh --data-dir NODE storage status
hmesh --data-dir NODE storage plan --older-than-days 30 --keep-latest 20 --out NEW_PLAN.json
hmesh --data-dir NODE storage apply --plan NEW_PLAN.json --sha256 REVIEWED_PLAN_HASH
hmesh --data-dir NODE storage compact
```

默认保留最近 20 个任务、近 30 天的任务、所有未结束任务、`UNKNOWN` 以及未结束工作流引用。清理计划绑定节点、固定候选和快照/运行目录内容摘要；文件变化、计划篡改、保留范围变化或运行锁冲突均拒绝。清理不删除 task/attempt/事件/核验证据。快照正文置空并标记 `purged_by_owner`，之后明确不可用，不回退读取当前文件。清理文件与 SQLite 不构成跨资源事务；若文件删除失败，保留已记录的快照清理事件，重新预览剩余数据，不盲目重发旧计划。

共享目录任务的旧 managed-run 目录可清理，外部项目目录不进入计划。未应用 worktree 始终保留。已应用且没有新增修改的 worktree 可单独清理：

```sh
hmesh --data-dir NODE workspace prune TASK_UUID --sha256 ALREADY_APPLIED_PATCH_HASH
```

它要求成功任务、已应用状态、相同 patch hash，且没有未结束工作流引用；重新计算 diff 后才调用限定的 Git worktree remove。数据库中的 patch 和历史仍保留。随后 `storage plan` 可清理其剩余 managed-run 文件。

扫描拒绝符号链接、Windows reparse point 和特殊文件；每个运行目录最多扫描 20000 个条目/1 GiB、64 层目录，超限保留并要求 owner 检查。`storage compact` 显式执行 SQLite checkpoint/VACUUM，让空闲页空间归还文件系统。

## 用量、价格与预算

```sh
hmesh --data-dir NODE usage
hmesh --data-dir NODE budget list
hmesh --data-dir NODE budget set --file budget.json
```

预算示例中的价格是 **演示值，不是供应商现价**：

```json
{
  "project_id": "my-project",
  "max_runtime_seconds": 600,
  "max_reported_tokens": 100000,
  "input_usd_per_million": 1.0,
  "output_usd_per_million": 2.0,
  "pricing_model": "deepseek-v4-flash",
  "pricing_provider": "deepseek",
  "price_source": "owner-provided-example-rates",
  "price_as_of": "2026-11-04"
}
```

只需时限时，将 token/价格/来源/日期/模型/Provider 相关字段置 null。预算在执行端按项目保存，适用于新 attempt；已有任务本身的更短时限继续优先。执行过程中不重新定价或扩大已冻结时限。

用量从固定协议的数值字段读取：Pi `message_end.message.usage`，OpenCode `step_finish.part.tokens`。按消息/step ID 去重并绑定 task/attempt；输出字节不折算 token。没有可信报告的字段保持 null；DSH 固定 fixture 尚未建立用量格式，保持未知。OpenCode 没有报告 total 时不会自己相加猜测 total。

`max_reported_tokens` 在 Harness 报告累计 total 达到上限后停止受控进程。报告可能只在一轮请求结束后到达，因此它不是供应商硬 token 或费用上限；没有 total 报告时只执行时限限制。页面和 CLI 明确显示这一点。

费用按 owner 提供的当前输入/输出价格估算，必须附来源、日期及精确模型/Provider，并匹配该 attempt 记录的配置。缓存 token 非零或未报告时不使用普通输入价格估算。更改价格会重新计算历史视图，标记 `current-owner-rates; estimate only`；它不是供应商账单。统计包含排队/执行耗时、终态执行成功率和最近 500 个任务，面板展示最近 30 条；执行成功率不等于工件验收通过率。

## Harness 兼容性

```sh
hmesh --data-dir NODE compatibility
hmesh --data-dir NODE compatibility --agent-id AGENT_UUID
```

默认矩阵运行 Pi/DSH/OpenCode 的固定脱敏协议 fixture，检查成功边界、逐字节分片、非零退出、缺失完成事件、畸形 JSON 和过大消息。固定版本表与原有版本探测共用同一个来源，升级版本不会自动加入 allowlist。

`--agent-id` 只对该已登记 Agent 执行隔离的 `--version`，检查当前版本是否受支持、是否与登记时一致，不调用模型、不修改 Agent 验证状态。无法识别的原始版本输出不导出。版本变化时通过 Agent 更新流程重新登记，再显式验证真实执行。面板的 Agent 诊断卡提供相同检查。

协议 fixture 与本机版本入口检查，不代表真实模型认证、全部原生功能或 Windows 进程行为已通过。已有无模型进程回归负责超时、取消、子进程和大输出；真实 Harness 的实际认证/运行验收继续单独保留证据。

## 自动回归

```sh
cargo +stable fmt --all --check
cargo +stable clippy --locked --workspace --all-targets -- -D warnings
cargo +stable test --locked --workspace
cargo +stable build --locked -p hmesh-cli -p hmesh-fixture
node scripts/development-smoke.mjs
npm run test:panel
npm run test:development-panel
```

双本机 TLS Node 回归覆盖模板幂等/冲突、固定 ID、审批、发起端重启、两阶段与有限修复、冻结工件契约、UNKNOWN 不重放、取消、用量上限、诊断导出、版本入口及离线清理预览。浏览器回归覆盖模板保存、丢失响应后刷新恢复、阶段创建/审阅/取消、预算表单、诊断下载及手机布局。三平台 CI 运行 CLI/进程回归，Linux 另运行浏览器回归；它们都不调用真实模型。
