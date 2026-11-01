# 本机管理面板

页面内置在 CLI 中，无前端构建、外部字体或云服务依赖。浏览器通过本机桥接进程访问 owner mTLS API，仅监听 127.0.0.1。

## 启动与关闭

```sh
cargo +stable build --locked -p hmesh-cli
./target/debug/hmesh --data-dir "$PWD/.hmesh-v0/tailnet-darwin/node" panel
```

默认连接已有 Daemon。加 `--start-daemon` 可初始化新的空目录，并在没有 Daemon 时启动一个。已有身份、策略和凭据不会被重建。端口默认 38900，支持 `--port 38901` 或自动分配的 `--port 0`；没有对公网、局域网或 Tailnet 监听的选项。Windows 使用 `.exe` 和现有 Node 路径。

完整访问链接带本次随机令牌，仅供本机 owner。浏览器清除地址栏 fragment，并在当前标签页 sessionStorage 保留访问令牌；新标签页需重新打开完整链接。不要转发或公开记录启动链接。`--json` 输出启动 URL、面板 PID 和本次拥有的 Daemon PID，集成工具应使用私有日志。

顶部关闭按钮或 Ctrl+C 正常关闭面板，只停止它启动的 Daemon，复用的已有 Daemon 保持运行。启动和停止均核对进程身份；端口占用在初始化/启动前报错。检测到缺少新 owner API 的旧 Daemon 时会保留它并提示显式升级/重启，不会替换运行中的实例。异常硬杀或断电不提供相同的正常关闭保证。

## 任务工作流

- **新建任务**：选择本机或已信任节点、对本机可见且验证通过的 Agent、所属项目与目标。默认 600 秒 / 16 MiB 输出，可按核心边界调整。输出字节不是 token 或费用预算。提交后始终等待执行端审批，批准后可能调用模型并产生费用。
- **审批与执行**：审阅完整请求、目标 Agent 和项目，再批准或拒绝。支持取消任务、查看 attempt 与执行状态、按 seq 补读事件、归档本机终态事件。执行成功与工件验收分别显示。
- **原请求恢复**：发送前生成并保留稳定 UUID。丢失确认后冻结原草稿；显式恢复使用本机已持久化的原 envelope，不接受新的目标或限额，不自动换 ID 重试。已有任务保持同一 task/attempt，未到达的原请求仍按原审批要求入队。放弃本地草稿不会取消服务端任务，无法保留草稿时不会发送。
- **工件核验**：成功任务可选择执行端 owner 已授权的相对路径，填写包含 1–16 个字段、最多 8192 字节的预期 JSON。显示字段断言、SHA-256、attempt 和配置摘要。核验失败不改写执行成功，也不调用模型。读取的是执行端当前文件，不是完成时的不可变快照。

发出任务列表来自持久化的最近观察缓存，标有“发出 · 缓存”；详情实时查询执行端。schema 6 保存最近一次成功读取的观察时间，升级前没有时间的旧缓存保持未知。失联会显示错误码、trace 和可用缓存，历史成功不代表远端当前在线。

一次读取一页事件，补读保留 seq 游标；单次打开最多显示 2000 条。更大回放使用 `hmesh task events/watch`。归档是同库逻辑归档，不删除历史或缩减文件大小。

## 节点、Agent 与服务

总览区分节点可达性、注册表可读性和可运行 Agent。本机 Agent 可退役/恢复；恢复后通过 CLI 显式重新验证，不自动调用模型。服务页面显示租约、受管进程、HTTP 健康、连接和隧道，支持重启受管服务和撤销访问。

Agent 登记/模型验证、策略修改、配对变更与服务发布保留在 CLI。面板没有任意命令、任意 API 代理或配置文件编辑入口。

## 访问与升级边界

owner token、私钥与模型凭据留在 Rust 进程，不发送到浏览器。每次启动生成独立浏览器令牌；API 要求精确 Host，写请求还要求同源 Origin。生命周期操作只接受空 JSON；任务/核验使用拒绝未知字段的类型化请求。页面无跨站 CORS 放行，禁止 frame 嵌入，响应不缓存，远端文本只通过 textContent 渲染。服务只返回摘要，不发送进程环境、参数或原始诊断。

远端工件核验只允许原任务发起节点，要求当前信任与该项目/Agent 的 task.submit grant 有效，并继续受执行端 artifact_paths 约束。返回路径列表、hash 和断言，不返回文件正文；不会扩大文件 allowlist。撤销授权后拒绝核验。两端均需更新以使用新接口。

升级 schema 6 前备份状态数据库；旧二进制拒绝打开新版数据库。面板不会偷偷迁移旧运行实例。它是本机 owner 的部分管理入口，不是多用户系统或 OS 用户隔离。

## 回归与运行包

普通 `cargo test --locked --workspace` 使用真实 HTTP/mTLS 和临时数据库，验证鉴权、原请求去重、工件边界、迁移、进程归属及旧 Daemon 保留。没有真实模型调用。

```sh
cargo +stable build --locked -p hmesh-cli -p hmesh-fixture
npm ci --ignore-scripts
npx playwright install chromium
npm run test:panel
```

Playwright 版本由 package-lock 固定，仅为开发回归依赖。可用 HMESH_PLAYWRIGHT_MODULE 指定已安装模块，用 HMESH_CHROME 指定本机 Chromium。两个独立临时 Node 与明确标识的 fixture 验证新建、审批、丢失确认、恢复不重复 attempt、核验通过/失败、Agent 生命周期、XSS、手机布局和关闭操作；这不代替新的真实跨机模型验收。CI 在 Linux 自动执行浏览器回归。

原生 Mac/Windows release 运行包及实际解压回归见 [打包与启动](runtime-packages.md)。运行包无需 Rust/npm，不带项目、模型凭据、Harness 或 Tailscale。初版面板的历史基线见 [原验收记录](evidence/latest-local-panel.json)。

2026-11-01 开发回归：79 项 Rust、14 项 Node 协议检查、20 项浏览器流程、8 项 Mac 实际运行包检查通过，fmt/Clippy 通过。三项既有本机 mTLS/修复替身/防重放流程复测通过。见 [本地脱敏记录](evidence/latest-panel-workflow.json)，跨平台 CI 与运行包结果以对应 PR/workflow 为准。
