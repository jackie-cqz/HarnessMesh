# V0 P0 实施与验收状态

0.0.1 本机可靠性与接入：schema 7 增加任务完成时工件快照、按 attempt 绑定的独立 Git worktree 与离线精确 hash 应用、私有一致性备份与原路径恢复、项目/Agent 容量及持久化公平调度、面板接入向导与精确授权预览，以及版本化运行包、升级预检/备份和显式用户登录自启管理。本机 93 项 Rust、14 项 Node、27 项浏览器流程、10 项实际运行包检查通过；既有 mTLS、修复替身和 guardian 不重放流程复测通过。未调用真实模型、未迁移现有运行 Node、未在开发者 Mac 安装自启。原生 Mac/Windows 登录注册生命周期由 Native runtime CI 在一次性 runner 上检查；跨平台结果以对应 PR 的最终提交检查为准。见 [说明](local-reliability.md) 与 [脱敏本机记录](evidence/latest-local-reliability.json)。


2026-11-01 后续稳定性验收：Windows/Mac 真机原 task/attempt 恢复、Windows 固定动作执行器退出一次后拒绝重放、Mac Pi/DeepSeek V4 Flash 原生 MCP 发现并委派 Windows DSH、父子工件独立核验均通过。Mac 独立后端 SIGKILL 后仅重启一次、revision 不变、数据保留且最终停止。生命周期 schema 5 已实现待审批过期、终态事件逻辑归档、旧 Agent 退役/恢复，以及 Unix 已观察脱组子进程的身份核对与清理；真实父任务 124 条事件归档前后 hash 一致，退役后历史核验仍通过。此前提交 021813f 的 [三平台 CI 与依赖审计](https://github.com/jackie-cqz/HarnessMesh/actions/runs/41218230946) 全部成功；本次新增代码以最终提交的 CI 为准。见 [恢复与生命周期说明](lifecycle-and-recovery.md)、[双机证据](evidence/latest-real-recovery-native-mcp.json)、[后端崩溃证据](evidence/latest-managed-backend-crash.json)。未注入强制 Tailnet 断网/断电；Unix 快速未观察双 fork 和 Daemon 硬崩溃仍不属于已保证的 OS 进程隔离。


2026-11-01 Windows↔Mac 自动化真机验收通过：Mac 总控经受管 mTLS HTTP 自动收集 Windows 执行器结果，22 个阶段全部 passed。冻结 DSH 检查器与独立 CRUD 检查均通过；后端 revision 1→2 且 PID 改变后测试数据仍保留；注入固定 health 缺陷后原检查器退出 1，Pi/Flash 一次真实修复 SUCCEEDED，revision 4 后同一检查器与独立 CRUD 再通过。后端/执行器服务与双端转发连接已撤销，本轮后端进程已停止，原应用源文件 hash 未变。真实修复输出 266,846 字节，限额 16 MiB / 600 秒。结果由 Mac 经认证通道采集，不依赖人工贴回；原生 MCP 自主委派、强制断网和断电持久化不在本轮范围。见 [独立证据](evidence/latest-automated-tailnet-acceptance.json)。


2026-10-31 核心加固：新增按原 request ID 恢复提交、验收条件/attempt/配置摘要绑定、执行中授权复查、每任务事件预算以及 owner Agent 更新/停用/摘要接口。配置更新使模型验证失效，并拒绝修改仍有未结任务的 Agent。行为、测试边界和未覆盖门禁见 [核心加固说明](core-hardening.md)。本地 fixture 不作为真实跨机模型验收。

2026-10-30 Mac 开发基线：Apple Silicon / Rust stable 1.99.1 的 fmt、全工作区 Clippy（warnings denied）和 59 项 Rust 测试通过；3 项启动配置 Node 回归测试通过并接入 CI。启动脚本支持 managed Pi 启动器、空 auth 的安全回退与直接 models 凭据引用，按配置摘要复用登记，避免复用旧配置 Agent。本次未调用模型，未重启现有验收 Daemon；新配置 Agent 仍需独立验证和显式项目授权。


2026-10-30 Mac 发起端补充：已读回反向任务 SUCCEEDED、原 attempt、write/read 事件与五字段确定性核验通过结果，hash 与 Windows 报告一致。见 [Mac 读回证据](evidence/latest-tailnet-reverse-mac.json)。Mac CLI 编译已通过；重复提交、专项分页、反向故障恢复及完整 workspace 检查未验证。

2026-10-30 反向执行端检查点：Mac 向 Windows 的 `windows-dsh-pro` / DSH 0.2.0-rc.2 / `deepseek-v4-pro` 发起任务，Windows 本机审批后 SUCCEEDED，write/read、五字段 JSON、168 字节工件独立 hash 和单 attempt 持久化通过。结果已发布到原 Task verification 接口；Mac 侧读取结论、重复提交和补读尚未在 Windows 报告中独立收集。见 [反向检查点](tailnet-reverse-smoke.md) 与 [证据](evidence/latest-tailnet-reverse.json)。两种任务方向已有真实执行证据，Mac 完整工作区测试、前后端受管服务及完整 V0.1/V0.2 仍未完成。

2026-10-30 正向 Mac 真机检查点：Windows 经 Tailnet/mTLS 向 Mac Pi 1.4.0 / `deepseek-v4-flash` 派发真实 write/read 任务，批准前无 attempt，批准后 SUCCEEDED。五字段 JSON 核验、154 字节工件 hash、同请求去重、23 条事件与游标补读通过；发起端进程退出后，以原身份重启仍可读到同一成功任务/attempt。首次本机凭据失败保留；Mac owner 重新登记 API-key Agent 并更新精确 grant。见 [检查点说明](tailnet-mac-smoke.md) 和 [结构化证据](evidence/latest-tailnet-mac.json)。该报告保留原范围，后续反向任务见上方检查点。

2026-10-30 Tailnet 初始检查点（历史记录）：加入只读 TailscaleProvider、候选发现和显式 `init --network tailscale`；监听 IP 必须属于当前本机，证书包含该 IP SAN，owner 有独立回环入口。Windows 59 项 Rust 测试、fmt/Clippy 与 CLI/MCP 构建通过；单机两个真实 Tailnet 地址监听完成 mTLS、双向配对、错误指纹/未认证/公开 owner API 拒绝与撤销，无 SSH、无模型调用。当时仅验证 Windows→Mac 的 Tailscale ping，Mac 真机任务尚未执行；该项现由上方独立检查点覆盖。原 [单机证据](evidence/latest-tailnet-local.json) 保持原样。

2026-10-29 受管 HTTP/SSH 检查点：Daemon 已管理限定项目/peer 的服务租约、精确 SSH 转发、本地 mTLS HTTP 连接、进程树与有限重启。真实 Pi/Flash 一轮修复缺失字段，受管服务自动重启，原 GET 契约 verified；DSH/Pro 的同版本客户端修复前失败、修复后 CRUD 通过。后端 10 项与客户端 4 项测试、受管重启后数据保留、断 SSH 子进程自动恢复、撤销关闭入口通过。最终 Windows 52 / Linux 51 项 Rust 测试通过；Windows fmt/Clippy 通过。验收曾发现原 7892 代理请求超时及固定 5 秒/1 秒 HTTP 限制，历史失败保留，现使用独立 Agent 的已验证直连、owner 总预算与调用方余量，写请求不自动重放。见 [配置与限制](managed-services.md)、[最终证据](evidence/latest-managed-services.json)。验收进程已清理，原 Todo 应用保留；完整 V0.1/V0.2、Tailscale、Codex、三机 Gateway 与 OS 开机常驻仍未完成。

2026-10-28：SPEC 1.3 的 **V0 L01-L05 / E2E-00 在当前 Windows x64 电脑通过**。实现语言为 Rust，测试脚本为 PowerShell 7。没有使用第二台电脑、Tailscale、Codex、MCP、Hub 或服务代理。本结论不替代后续 V0.1 的跨设备/跨 OS 门禁。

后续 SSH 检查点已完成：Windows→Orion/Linux、Pi 1.4.0、`deepseek-v4-flash` 的真实任务、mTLS/审批、write/read、独立 JSON/hash、去重与事件补读通过，测试进程已停止，原认证文件未变。见 `docs/ssh-first-smoke.md` 和 `docs/evidence/latest-ssh-smoke.json`。本机适配后 fmt/Clippy/32 项测试通过，Linux CLI 本机构建通过；整个锁文件 232 个公开依赖的审计仍为零命中。下方 V0 证据与 30 项测试数量保留为该原始版本的历史记录，完整 V0.1 尚未完成。

SSH 应用检查点也已验收：`examples/ssh-todo` 的 Orion Pi/Flash 服务端与 Windows DSH/Pro 客户端，通过服务端 10 项、客户端 4 项测试、独立跨机 HTTP、浏览器新增/完成/刷新，以及真实后端重启后的数据保留。代码生成 Daemon/Mesh 隧道已停止，应用服务保留供试用。DSH 任务成功；Pi 写出完整文件但触发输出保护，任务仍为 FAILED，产物独立验收通过。显式累计输出额度选项及边界回归加入后，Rust fmt/Clippy/33 项测试与两端构建通过。见 [示例说明](../examples/ssh-todo/README.md)、[实际证据](evidence/latest-todo-integration.json) 及 SPEC 1.5 第 7.11 节；没有完成受管 Service/Gateway 或 Tailscale。

## 验收映射

2026-10-29 SSH/MCP 检查点：两个方向的 Pi/DSH 真实任务、执行中断开 SSH、发起端正常重启、事件补读和相同请求去重均已通过。新增 hmesh-mcp 和 schema 3 委派绑定，真实 Pi 1.4.0 原生 MCP 发现并提交 Windows DSH，子任务先无 attempt 等待独立批准，父子任务最终均 SUCCEEDED，nonce/hash 核验一致。最终 Windows 46 项、Orion/Linux 45 项 Rust 测试通过。配置见 [MCP](mcp.md)，过程偏差与完整证据见 [双向与 MCP 验收](ssh-bidirectional-mcp.md)。Tailscale、Codex、受管服务和完整 V0.1 仍未完成。

2026-10-29 大输出回归：已分离原生消息和内部事件限额，长正文脱敏后分段、事件按字节分页，`task watch` 补读到最后一页才退出。新的 Orion Pi/Flash 任务 `c2c13fd7-eae0-42e4-8945-99c6784129ba` 为 SUCCEEDED，实测最大原生消息 375,131 字节，累计 8,758,986 字节；新生成服务端 14 项测试通过，原应用 10+4 项测试与 HTTP/持久化复测通过。见 [独立回归报告](evidence/latest-pi-output-regression.json) 和 [ADR 0003](adr/0003-native-output-limits.md)。历史失败记录保持不变，最终 Rust 测试数量记录在该新报告中。

| 工作包 | 已交付内容 | 证据 |
|---|---|---|
| L01 | Workspace、类型/配置、SQLite migrations、幂等 init、独立 data-dir、owner 凭据、目录锁、CLI/前台 Daemon | core/store/security 测试，CLI 两进程测试（同目录与端口冲突明确失败），真实 smoke 两实例 |
| L02 | 回环 TLS 1.3、证书 pin + WebPKI、mTLS、签名单次配对、双端批准/撤销、owner/peer 分界、项目/Agent 默认拒绝 | transport TLS、CLI 双节点配对、daemon authorization 测试；真实默认拒绝/审批拒绝 |
| L03 | Pi RPC、DSH headless JSON、OpenCode run JSON、版本锁定、配置隔离、显式凭据/工具 | 三种本机实际版本、三个 owner 真实模型任务、原生 JSON 事件和文件断言；协议 fixture 单独标识 |
| L04 | 持久 task/attempt/events、原子去重/claim、审批/取消、发起端路由、事件补读、重启查询与 UNKNOWN | 三条真实跨 Node 任务：A→B/DSH、B→A/Pi、A→B/OpenCode；原 request 重发、hash/mtime/attempt 不变；受控故障进程测试 |
| L05 | 可重复脚本、示例、quickstart、协议/安全文档、ADR、显式模型 opt-in、只停止自己启动的进程 | scripts/e2e-local.ps1、examples/single-machine-task/、docs/quickstart-v0.md、docs/protocol.md、docs/security.md |

## 最终验证

- `cargo +stable fmt --all --check`、Clippy `--workspace --all-targets -- -D warnings`、workspace tests 和 CLI build 通过；当前共有 30 个非忽略测试。普通检查不调用模型。
- `pwsh -NoProfile -File scripts/e2e-local.ps1 -RunRealHarnesses` 完整执行退出 0。Pi 0.84.2、DSH 0.2.0-rc.2、OpenCode 1.18.20 使用授权 DeepSeek 环境凭据和 `deepseek-v4-pro`，本机验证与两个方向的实际委派通过。
- `docs/evidence/latest-p0-smoke.json` 为完整最终证据（schema 2），绑定 binary/Cargo.lock SHA-256，记录包与入口、模型标识、节点/task/attempt、事件轨迹、文件 hash、确定性断言、去重和重启查询；不含密钥或个人 Session 历史。
- `cargo +stable run --locked -p xtask -- audit` 通过官方 OSV API 检查锁文件的全部目标：232 个公开 crate，零漏洞命中。已升级命中的 TLS/异步及传递依赖，复用 ring SHA-256 移除旧 block-buffer 链。结果和锁文件 hash 在 `docs/evidence/dependency-audit.json`。
- 初次接入证据 `docs/evidence/2026-10-28-real-harnesses.json` 仅保留各自 managed-run 历史；完整双节点结论以上述最新报告为准。

授权负例经真实 TLS/API 验证：错误 pin、未配对访问、签名重放、owner API 不向 peer 开放、伪造 sender、未知项目/远端 cwd、跨 origin 读取/取消、审批拒绝、预启动取消、策略/信任撤销。拒绝任务没有执行 attempt 或工件。无效策略热更新保留最后有效策略；有效撤销立即生效。

受控 Rust 进程独立验证非交互输入失败、缺失凭据、执行前再授权、畸形/过大输出、超时、运行中取消和子进程清理；Windows 硬杀 Daemon 后保留原 attempt 并恢复 UNKNOWN，不自动重跑。Fixture 没有模型/账户访问，不能当作真实 Harness 执行证据。

## 已验证边界

- 三平台编译/检查与依赖审计 CI 已有远端成功记录，见页首 2026-11-01 稳定性检查点；下方原 V0 测试数量为历史记录。
- 真实 Harness 的正常 managed-run/原生 JSON 事件已通过；原生 abort、已有 Session 注入或恢复未验证。取消机制为受控进程树终止；Probe 对未验证项仍标 not_checked。
- Windows Job Object 的进程树回收已测试。Linux 正常结束与受控进程回收已有 SSH 实证；Unix 专用进程组为 best-effort，脱离进程组的子进程和 Unix 硬崩溃清理仍未验收。
- 工作目录和工具限制不构成 OS 文件系统沙箱。只登记独立测试项目；工件仅发布 owner 授权相对路径的 hash/大小/断言。
- V0 基线使用显式环境 allowlist。后续 SSH 试验新增 Pi/DeepSeek 的 owner 显式只读认证引用，密钥仅在执行端内存注入子进程；细节见 ADR 0002。DSH 仍仅支持已验证的 read/write/edit 工具组合。
- V0 CLI 显式使用 Node/Agent UUID；协议轮询采用持久 seq 游标。具体设计决定见 docs/adr/0001-v0-rust-boundaries.md。

本次测试只停止它自己启动的进程。私有实例/运行诊断保存在被忽略的 `.hmesh-v0/acceptance-9369313213414b0e851e5f6bbce6992d/`；两个 Daemon 已正常停止。实例属于初始化它的 Windows 账户，工具沙箱账户与桌面用户不得通过放宽 ACL 混用这些目录。

2026-10-31 Tailnet HTTP 基础连通：Mac 发布仅授权 Windows peer 的独立受管回环后端，Mac 侧观察 active/running/healthy。用户提供 Windows 回环 connection `/health` 的响应，status=ok、node=mac、唯一 marker 与后端一致，基础转发通过。无 SSH、无模型调用；Windows 输出由用户提供，非 Mac 独立采集。CRUD、重启持久化、撤销和修复循环未验收。见 [证据](evidence/latest-tailnet-http-smoke.json)。

2026-10-31 真实 Tailnet Todo CRUD：Mac Pi/Flash 后端任务 SUCCEEDED（16 MiB 显式额度，实测累计 1,153,857 字节），Windows DSH 编写固定契约检查器 SUCCEEDED；用户提供 Windows 经 `127.0.0.1:18094` mTLS 转发执行的八项检查汇总，全部 pass。Mac 独立本机 CRUD 也通过，受管服务 healthy。此前两次 1 MiB 输出限额失败保留，不改写为成功。见 [独立证据](evidence/latest-tailnet-todo.json)。这是 CLI 编排的真实 Harness 协作，不代表原生 MCP 自主委派；重启持久化、修复循环、撤销仍未验收。

2026-10-31 自动验收脚本：新增 Mac 总控和 Windows owner 启动的固定动作执行器，经现有受管 mTLS HTTP 协调。包括冻结检查器、CRUD、重启持久化、可选一次 Pi 修复、撤销和双端证据回传；无远程 shell、SSH 或网络配置修改。67 项 Rust、14 项 Node 检查通过，本机两个真实 Daemon 的无模型及修复替身流程通过。Windows/Mac 真机自动化和真实修复仍待执行，未用本机测试替代。见 [使用说明](automated-tailnet-acceptance.md) 和 [本机证据](evidence/latest-automated-acceptance-local.json)。
