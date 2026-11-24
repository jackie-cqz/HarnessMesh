# 当前安全与能力边界（V0、SSH 与 Tailnet 直连）

V0 只在本机回环网运行，但每次操作仍经过证书和当前配对信任。数据目录内私钥、证书、owner token、数据库、配置和每次运行数据各自独立。Windows 使用仅当前用户可访问的 ACL，Unix 使用 0700/0600；init 拒绝覆盖未完成身份或非空的无关目录。相同 data-dir 的第二个 Daemon 因目录锁失败。

显式 Tailnet 模式每次启动须通过本机 Tailscale CLI 验证运行/在线及确切监听 IP；只绑定该字面 IP，并给新证书加入对应 SAN。公开监听仅接受 Tailnet 地址来源，拒绝整个 `/local/*`，即使请求携带 owner 证书和 token；owner 经独立回环 socket 管理。候选发现只显示必要网络元数据，不自动信任、授权或调用模型。默认回环实例不能主动连接 Tailnet endpoint，owner 客户端不向 Tailnet 发送 token。当前不解析任意 DNS 或发布到公网。详见 [直连说明](quickstart-tailnet.md)。

## 认证和授权

TLS 只允许 1.3。客户端先核对明确的 SHA-256 证书 pin，再由 rustls/WebPKI 验证有效期、主机名、用途与握手签名，不存在接受所有证书的模式。匿名入口仅用于 hello、单次 challenge 和签名配对申请；challenge 有目标绑定、过期和使用一次的约束，两端 owner 分别批准。

**配对是信任边界。** 已配对且仍受信任的节点可以看到本机登记的 agent，并能给它们派活；未配对或已撤销的节点什么都看不到、什么都派不了。每个对端有一个访问模式（policy 的 `[peers]` 段，默认 `auto`）：`auto` 直接执行，`confirm` 让任务停在 PENDING_APPROVAL 等本机 owner 批准（`hmesh peers access`）。发送方也可以主动要求审批（开发流程的阶段任务就是这样）。旧的 `[[grant]]` 项目授权和 agent 的 `require_remote_task_approval` 仍可解析，但不再参与决定。agent 只在它登记的项目目录里工作。登记 Agent 只能由本机 owner 完成。只通过版本探测的 Agent 保持 unverified，真实 owner 验证成功后才能接收 peer 任务。任务在出队前和启动前重新检查配对信任。撤销也在每次 HTTP 请求中重查，因此连接复用不会保留旧授权。

owner API 必须同时具备自己的客户端证书、该实例的私有 token 和回环来源；信任的 peer 无法操作它。任务读、事件、核验结果和取消仅对原发起 Node 或执行端 owner 开放。审计记录授权决定、task/attempt/trace，以及审批、拒绝、取消、策略更新和信任撤销；不会把模型账户密钥当作审计字段。

## 受管开发服务与 SSH

服务发布、隧道配置、连接建立及联调启动只在本机 owner API 开放。服务 peer 白名单和可选 restart 白名单由 owner 明确设置；task grant 不会自动授予服务访问。远端服务卡隐藏内部端口、命令和环境。restart 仅使用原 owner 配置；模型五个 MCP 工具没有这些管理能力。

HTTP 桥接每次重查当前信任和服务租约，缓冲响应释放前再次重查。两侧正文各不超过 256 KiB，最多八个本地并发请求，不转发 Cookie/Authorization，不自动重定向或重放写请求。SSH 使用独立空配置、显式身份及 known_hosts、strict host key 和最多两个固定回环端口转发；不能指定任意目标地址或 SOCKS。原用户 SSH 配置及已有转发不变。

进程由 owner 绝对路径和项目 root 启动，不接管占用端口或历史 PID。初启加最多三次自动重启，租约至多一小时；Daemon 正常停止回收自己的进程树。重启不复活旧授权，Unix 非正常强杀后可能有孤儿进程，owner 必须独立核查，不能依靠历史 PID 杀进程。OS 常驻安装及崩溃接管未实现。

联调固定保存契约/测试计划及其 hash，最多两次修复；确定性程序验收，模型完成不代表测试通过。原任务审批继续生效，远端审批不能由模型代行。网络或任务结果未知时停止，不生成新 UUID 猜测执行。报告记录响应 hash/断言，不包含响应正文；配置/契约与预期断言也应避免敏感信息。服务 env 属于本机 owner 私有配置，当前没有服务凭据管理器。细节见 [受管服务说明](managed-services.md)。

## Harness 运行机制

Adapter 采用参数数组直接启动本机 owner 指定的实际 Node/原生入口，不拼 shell 命令。远端不能改变 executable、cwd、环境或工具配置。每次 attempt 使用新的配置/会话目录，清空环境后仅保留运行所需 OS 变量和 owner 显式列出的凭据环境变量；配置中的凭据变量缺失时在启动前失败。不接管已有 Session。

SSH 试验新增 owner 显式的 `node_executable`、`models_file` 和 `credential_source` 引用。认证文件引用只支持 Pi/DeepSeek 的 api_key：在执行端内存中读取后注入受控子进程，加入日志脱敏，不复制原文件、不修改认证内容、不传给发起端。模型引用只复制所选模型的非认证元数据，限制到官方 HTTPS API origin，丢弃 provider key/header 并拒绝模型内嵌认证字段。其他 provider/OAuth 认证导入仍不支持。未配置引用时不读取个人认证文件。

Pi 默认关闭扩展、skills、上下文自动加载和交互审批；DSH 使用验证过的只读写编辑工具集和拒绝提权的 headless 策略；OpenCode 使用 pure 模式、隔离配置、显式 deny 默认权限和允许工具。owner 显式启用 Mesh 工具时，仅 Pi 1.4.0 额外加载 `builtin:mcp` 与当前 attempt 生成的固定注册扩展。原生权限策略仍是 Harness 自身的一部分。**固定工作目录和工具 allowlist 不构成 OS 文件系统沙箱**；本机进程拥有当前 Windows 用户的底层权限，只应登记用于测试的独立项目。凭据只在受控子进程环境中使用，不在注册配置保存值。

## MCP 服务器的权限

`hmesh mcp`（`hmesh-mcp`）是本机程序，使用本机 owner 的 RPC 客户端身份，因此接上它的 agent 能做本机 owner 能做的派活操作：列出本机和已配对节点的 agent、派活、等待和读取结果、取消。模型只看到五个固定工具，拿不到私有 token、任意路径读取、配对或 policy 修改、approve 接口。能读到节点目录的本机进程本来就能拿到 owner 身份，MCP 服务器没有扩大这个范围。执行端仍按自己对发起方的访问模式决定是否需要确认；远端返回的文本按不可信数据处理。

mesh 托管的 Pi 父任务（服务器带 `--parent-task` 启动）不使用上面的 owner 全权模式，而是限制在 owner 的 scope 文件内：只能看到和派给 scope 列出的目标，不能借 owner 身份读取或取消其他任务，也不能转派到 scope 之外。父任务必须仍为 RUNNING，每次调用检查当前 scope；get/wait/cancel 还验证持久化 parent→child 归属及当前目标绑定。schema 3 原子预留提交额度，失联仍占额度，同一 UUID 只能重发相同 payload。工具结果保留不可信数据语义；未知参数、跨父任务读取和未授权目标均拒绝。模型不能靠工具自行批准子任务。MCP 子进程的 DEEPSEEK_API_KEY 被置空，模型供应商凭据不通过工具协议传送。scope 不构成 OS 沙箱；信任本机 MCP 程序及其进程权限仍是 owner 的责任。详见 `docs/mcp.md`。

已知凭据值在 text delta（包括跨 delta 的值）、工具事件文本和 stderr 中脱敏。原生工具参数和输出不作为公共事件转发。不要主动把密钥放入任务正文、工件或输出；任务正文和运行历史是需要私有保存的数据。未承诺对任意编码/变换后的秘密进行识别。

## 取消、崩溃和恢复

Windows 在发送 prompt 前将进程放入 kill-on-close Job Object，终止时确认 Job 中活跃进程为零；只追踪当前 attempt 的进程。Unix 使用专用进程组，无法保证已脱离进程组的子进程被包含。无法证明退出时返回 UNKNOWN。它们均不是通用进程沙箱。

受控 Rust 测试进程已覆盖运行中取消、超时、子进程清理、异常输出和 Windows Daemon 硬退出恢复。真实 Pi/DSH/OpenCode 的正常任务已验证；本次没有声称验证过它们各自的原生 abort 协议或交互 Session 恢复。Managed Run 采用进程树终止；Probe 中未测试的取消/恢复项保持明确标记。

SQLite 去重避免同一请求创建第二次尝试，不承诺跨外部副作用的 exactly-once。Daemon 崩溃后的运行结果归 UNKNOWN，需要用户检查副作用；不得自动重试。

## 验证范围

本机测试覆盖错误 pin、匿名/未配对访问、owner 凭据分离、发送方伪造、未知项目/远端 cwd、跨 origin 读取、未批准拒绝、去重、签名重放、策略热更新和信任撤销。测试进程和协议 fixture 与真实模型证据分开。

`cargo run --locked -p xtask -- audit` 通过官方 OSV querybatch 查询整个锁文件（包含其他目标平台），处理分页且遇到不完整结果直接失败。报告含时间和锁文件 hash。安全数据库随时间变化，需要在发布前重新执行；最近一次报告位于 `docs/evidence/dependency-audit.json`。

真实验收已覆盖 V0 本机 Windows、SSH 双向 Pi/DSH、断线与发起端重启后的补读/去重，以及 Pi 1.4.0 原生 MCP 委派 DSH 的独立审批和父子产物核验。Unix 硬崩溃时对 detached 子进程的清理仍不在已验证能力内。Windows 和 Orion/Linux 本地回归已执行；CI 已配置三平台，尚无远端 CI 执行证据。完整 V0.1 的 Tailscale、Codex/Mac↔Windows 链路仍需验收。见 `docs/ssh-bidirectional-mcp.md`。
