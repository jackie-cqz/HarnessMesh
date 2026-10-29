# HarnessMesh — 跨设备 Coding Agent 协作网络

> **Engineering Specification / 可交给 Coding Agent 实施的工程规格**  
> 工作名称：`HarnessMesh`（暂定，不代表已核验名称可用性）  
> 文档版本：`SPEC 1.8` · 2026-10-29  
> 覆盖发布：**V0 → V0.1 → V0.2 → V0.3 → V1 → V2**  
> 实现语言：Rust 为主；Harness 专用桥接层可选 TypeScript / Python  
> 项目定位：**独立开源项目**；不依赖 AgentMeld，也不复制 AgentMeld 的团队工作台。  
> 规范词：**MUST（必须）**、**SHOULD（建议）**、**MAY（可选）**、**MUST NOT（禁止）**。  
> 注意：后续版本的命令、JSON、路径与服务端点仍为**拟定规范**；已实现 V0 的准确接口以 `docs/quickstart-v0.md` 和 `docs/protocol.md` 为准。本文不代表第三方 CLI 已支持尚未核查的能力。

---

## 0. 给开发 Agent 的执行指令（优先阅读）

本规格是工程实现基准，不是介绍文章。开发时：

1. **严格按照阶段验收**：先完成 V0 本机双节点联调，再进入 V0.1 跨设备联调；不要在 V0/V0.1 中实现 ECS Hub、WireGuard、P2P 打洞、Web 控制台或完整 A2A Server。
2. 每个功能按 **契约 → 单元测试 → 实现 → 集成测试 → 文档** 的顺序交付；不得用 Mock 测试成功冒充真实 Harness 成功。
3. 需要调用外部 CLI（Tailscale、Pi、DeepSeek Harness/DSH、OpenCode、Codex、OpenSSH）时，**先执行 `doctor`/版本探测并验证真实接口**。不要从本 SPEC 猜测第三方 CLI 的未验证参数。版本变化应被兼容层隔离。
4. V0 使用本机回环网络；V0.1 起跨设备网络层**优先复用 Tailscale**。本项目不实现 VPN 内核，不改造 Tailscale 客户端，不扫描未经用户授权的网络。
5. 节点在线、节点可信、Agent 已注册、Agent 可运行、开发服务可访问是**五个不同状态**，必须在数据模型与 UI/CLI 中分开。
6. 任意远端命令、文件读写、端口发布都要通过**本机明确授权**。Tailnet 可达 **不等于** 远端拥有执行权限。
7. 不得假设任意已有交互式 Harness Session 可以被远程注入输入；**Managed Run** 与**已存在交互 Session**必须分离。
8. 对副作用性任务不承诺 exactly-once execution；实现**幂等请求入站、持久化尝试、崩溃后状态 UNKNOWN 和人工核对**。
9. 每次提交必须保持 macOS、Windows、Linux 可编译（平台不支持的能力显式标注，不得静默成功）。
10. 每个 Milestone 提交 `tests/`、`docs/`、可运行示例以及验收证据。阶段未通过不得向下一阶段宣称完成。

### 0.1 必须先交付的最小成功画面（V0，本机）

先在**当前这一台电脑**上运行两个独立 Daemon 实例，通过回环网络完成真实任务闭环。两个实例必须具有不同的 `data_dir`、`node_id`、密钥/证书、数据库、监听端口、Harness session-dir 和测试项目目录。

```text
当前电脑（同一 OS）
  Node A / CLI                        Node B / Managed Harness
  127.0.0.1:38880  ── HTTPS/mTLS ──►  127.0.0.1:38881
  独立身份、数据库、项目 A               独立身份、数据库、项目 B

A: 查看已配对的 B 和可用 Agent
A: 提交任务 → B 本机批准 → 真实 Harness 在项目 B 中执行
A: watch → succeeded + 事件 + 工件摘要
受控 runner: 检查项目 B 中的预期文件、测试结果和 SHA-256
```

**首个检查点**：先用 Pi 完成上述真实闭环，验证重复请求不重复执行、未配对/未授权请求被拒绝、任务记录在 Daemon 重启后仍可查询。

**V0 P0 Pass**：本机现有 **Pi、DeepSeek Harness（DSH）、OpenCode** 三种 Adapter 分别完成真实任务 smoke，并以 Pi/DSH 完成 A→B、B→A 两个方向的任务委派；OpenCode 复用相同流程替换一个执行端验证，不要求三者所有两两组合。两个方向可由各节点 CLI 发起，Harness 主动调用 Mesh Tool 留给 V0.1 MCP。Codex 不作为 V0 前置条件。接口探测、fixture 与真实模型执行证据必须分开，未完成者不得标为通过。

V0 不要求 Tailscale、第二台电脑或跨 OS；使用与 V0.1 相同的任务协议、状态机、认证及授权流程，禁止创建仅供本地 Demo 的无认证捷径。**本机双实例不代表跨设备或跨 OS 验收通过。**

### 0.2 下一阶段的最小成功画面（V0.1，跨设备）

在 **Mac（Codex）** 和 **Windows（Pi）** 已经登录同一个 Tailnet 的前提下：

```text
Mac:     hmesh peers list
         → windows-node (trusted, online)
         → pi-backend (available)

Mac:     hmesh task send pi-backend --project demo \
           --prompt "在授权测试目录创建一个最小 HTTP API，并输出测试结果"

Windows: Mesh Daemon → Pi RPC → 真实工具/模型运行

Mac:     hmesh task watch <task-id>
         → accepted → running → succeeded
         → stdout summary / artifacts / audit record
```

这就是 **V0.1 第一条跨设备真实端到端验收**，必须在 V0 合格后完成。Mac/Windows Harness 可以按实际许可证、系统支持情况互换；测试必须覆盖两种不同 Harness、两种 OS。

---

## 1. 产品定义、用户与范围

### 1.1 一句话定义

> HarnessMesh is a local-first, cross-machine development mesh for coding agents: discover authorized peers, delegate managed work, expose temporary development services, and verify integrations across devices.

HarnessMesh 连接的是 **Node（设备）— Harness（运行时）— Agent（执行主体）— Task（任务）— Service（开发服务）**，不是单纯的聊天机器人网络。

### 1.2 主要用户场景

**场景 0：单机开发与联调（V0）**

- 当前电脑启动两个独立的 Mesh Node，先验证配对、授权、Agent 注册、任务提交、真实执行和结果回传。
- 每个节点使用独立的测试目录和持久化状态；P0 按 Pi → DSH → OpenCode 接入，先打通一个真实执行端，再验证异构执行端与双向委派。
- 先解决协议、Adapter 和任务引擎的问题，再进入多电脑网络联调；V0 不包含开发服务发布或 Gateway。

**场景 A：跨设备任务交接（V0.1）**

- Mac 上 Codex 负责前端；Windows 上 Pi 负责另一模块。
- 两边都在线且 Tailscale 相通。
- A 看得到 B 的公开能力，发送结构化任务；B 经本机授权执行并回传状态与工件。

**场景 B：前后端跨机器联调（V0.2）**

- Mac 前端 Harness 运行 Web 应用。
- Linux 后端 Harness 运行 FastAPI，监听 Linux `127.0.0.1:8000`。
- Windows 在同一校内网络，作为 SSH 跳板，不需要把整个校园网路由到 Tailnet。
- HarnessMesh 在授权后建立**具体端口、短时有效**的可访问通道；前端执行 HTTP/浏览器测试，将错误证据返回后端，后端修复并请求重测。

**场景 C：跨机器稳定执行（V1）**

- 网络瞬断、Daemon 重启、运行超时不丢失已确认接收的任务记录。
- 用户能区分任务成功、已取消、执行结果未知；可以检查尝试、回放事件和重试。

**场景 D：可选云端协调（V2）**

- 阿里云 ECS（2 vCPU / 2 GiB）部署轻量 Hub。
- 节点在线时仍直接走 Tailscale / SSH；Hub 只做目录、离线信箱、状态同步、信令和轻量管理。
- Hub 故障不妨碍已建立的 V1 直连关系继续工作。

### 1.3 不做什么（NON-GOALS）

- 不实现通用 VPN、WireGuard 内核、NAT 穿透、DERP；先复用 Tailscale。
- 不做跨机器通用远程桌面、RDP、屏幕控制。
- 不做模型训练、推理托管、分布式 GPU 调度与大容量文件网盘。
- 不做 AgentMeld 那样的多人团队聊天、Conductor、复杂 DAG 编排与全功能 Web IDE。
- 不要求对现有 Codex/Claude/Pi 的任意交互会话注入消息。
- 不承诺沙箱隔离：本机 Adapter 的 OS 进程权限仍由用户负责；MVP 优先**只允许指定项目目录**，并对高风险动作要求审批。
- 不默认向其他节点分享所有本地服务、SSH 凭据、环境变量或校园子网。
- 不在 V0-V1 宣称“完全符合 A2A 1.0”；先实现**内部契约，随后提供兼容层并通过一致性测试**。

### 1.4 成功指标（目标值，不是现成性能结论）

V0 的门槛是第 16.0 节的本机真实 E2E、实例状态分离及授权负例；以下跨设备指标从 V0.1 开始测量。本机结果不能计入跨 OS、跨网络或 Tailscale 可达性的验收证据。

| 指标 | V0.1 目标 | V1 目标 |
|---|---|---|
| 相同 Tailnet 的 2 节点初始化 | 手动配对，10 分钟内完成 | 标准化向导，5 分钟内完成 |
| 两个不同 Harness 的真实执行 | ≥1 条跨 OS E2E 通过 | 持续 CI 覆盖 2 Harness + 3 OS |
| 节点发现（在线后） | 手动刷新或 ≤15s 轮询 | 已信任节点 ≤10s 更新 |
| 任务审计 | 任务/尝试/状态/结果可查 | 事件可回放、重启可恢复记录 |
| 断线重连 | 不中途悄悄重启副作用任务 | 失联任务确定为 `UNKNOWN` 或可证明仍运行 |
| 服务联调 | V0.2 单端口 HTTP E2E | 多服务、租约回收、权限回归 |
| 测试示例 | 静态 fixture + 真机 smoke | 故障注入、负例、安全回归 |

性能目标须附测试环境和测量脚本，禁止直接当 SLA。

---

## 2. 版本路线与边界

| 版本 | 目标 | 必交功能 | 明确不含 |
|---|---|---|---|
| **V0 — Local Loopback** | 当前电脑上的两个独立 Node 完成真实任务闭环 | Rust Daemon/CLI 基础、多实例配置、回环 HTTPS/mTLS、手动配对/项目授权、Pi/DSH/OpenCode 三种 P0 Adapter、任务持久化/去重/事件与本机 E2E | Tailscale 依赖、跨设备/跨 OS 验收、Codex 与 MCP 必交门槛、服务暴露、SSH Gateway、云端 |
| **V0.1 — Peer & Task** | 两台已连通 Tailnet 设备上的 Harness 真正互调 | 复用 V0 核心；Tailscale 候选、跨机配对/信任与注册、Pi+Codex 两种 Adapter、跨 OS 任务 E2E、本机 MCP 任务工具 | SSH Gateway、服务暴露、云端 |
| **V0.2 — Dev Service & SSH Gateway** | Mac ↔ Windows ↔ Linux 后端联调 | 服务发布/访问/撤销；单端口 TCP/HTTP 代理；SSH Gateway；API 契约及测试证据闭环 | 自动修复无限循环、云端 |
| **V0.3 — Secure & Resilient Beta** | 安全可控与跨平台稳定 | 策略/审批、重启恢复、超时取消、版本协商、故障注入、可重复打包、跨平台安装/卸载 | 组织级多租户 |
| **V1 — Standalone Release** | 无云依赖的可用开源产品 | 稳定 CLI/MCP、文档/示例、两种 Harness 稳定适配、质量门槛、轻量状态观察 | Web 管理大平台 |
| **V2 — Optional Cloud Hub** | ECS 可选协调层 | 云目录、离线信箱、云端审计摘要、设备状态、可选 Web UI、密钥轮换/Hub 升级 | 代理全量执行、存全部项目源码、强制云中转 |

**版本门禁原则：任何阶段新增需求只有在不破坏上一阶段的离线/本地路径时才能合并。**

V0 → V0.1 的主要新增变量是设备、OS 与网络路径。不得另写一套任务执行链路；V0 回环测试作为后续阶段的基础回归长期保留。

---

## 3. 架构与部署拓扑

### 3.0 V0 本机双实例拓扑

```text
同一台电脑 / 同一 OS
┌─────────────────────┐                ┌─────────────────────────┐
│ Node A              │                │ Node B                  │
│ CLI / 任务发起方     │                │ 真实 Harness / 执行方    │
│ Daemon :38880       │◄──────────────►│ Daemon :38881           │
│ data-dir A / DB A   │ loopback mTLS  │ data-dir B / DB B       │
│ identity A / proj A │ + project ACL  │ identity B / proj B     │
└─────────────────────┘                └─────────────────────────┘
```

- 两个独立 Daemon **进程**通过真实 socket 连接；不能以同进程函数调用替代协议验收。
- `--data-dir` 选择完整实例配置，CLI 必须连接到对应 Daemon，并读取该实例自己的本机 API 凭据；不能因共用默认目录串用身份。
- 两个实例仅监听 `127.0.0.1`，不要求 Tailscale 运行。节点间 HTTPS/mTLS 与本机 CLI→Daemon 认证是两条不同的边界，均需验证。
- 配对、注册、授权、Task/Attempt/Event 和数据库实现直接复用于 V0.1；V0.1 在此基础上增加 Tailscale 地址与跨机验证。
- 项目目录分离用于发现路由错误与配置串用，不代表 OS 沙箱；原生 Harness 的审批和权限限制仍然适用。

### 3.1 V0.1 直连拓扑

```text
 Mac (Tailscale IP)                     Windows (Tailscale IP)
 ┌────────────────────┐                ┌────────────────────────┐
 │ Codex              │                │ Pi                     │
 │     ↕ adapter      │                │     ↕ adapter          │
 │ Rust hmesh-daemon  │◄──────────────►│ Rust hmesh-daemon      │
 │ SQLite / trust     │ Tailscale TCP  │ SQLite / trust         │
 └────────────────────┘ + app auth     └────────────────────────┘

 No ECS. No inbound public internet port. No direct Agent ↔ Agent runtime coupling.
```

### 3.2 V0.2 校园网络跳板拓扑

```text
Mac / Tailnet                             Windows / Tailnet                  Linux / LAN
┌───────────────────┐                 ┌────────────────────┐          ┌──────────────────┐
│ Codex + hmesh     │                 │ hmesh-gateway      │          │ Pi + hmesh-node  │
│ local proxy :18000│                 │ SSH client / proxy │          │ API 127.0.0.1:8000│
└─────────┬─────────┘                 └─────────┬──────────┘          └─────────┬────────┘
          │ Tailscale TCP                       │ SSH connection               │
          └────────────────────────────────────►│◄─────────────────────────────┘
        app-auth + route permission           only approved Linux host:ports
```

实现说明：

- Windows Gateway 从本机**主动发起**到 Linux 的 SSH 连接（目标 Host Key 必须验证）。
- Linux 运行 `hmesh-daemon`（监听 `127.0.0.1`）并不需要装 Tailscale；Gateway 可通过 SSH `-L` 本地转发连到该 Daemon。
- 当 Mac 要访问 Linux 开发服务时，Linux 注册 `127.0.0.1:8000`；Gateway 为该服务建立独立 `ssh -L` 链路或复用受控 SSH multiplexing，再将字节流转发到 Mac 已授权的本地临时端口。
- Gateway **不是全网 VPN 路由器**；不配置 `0.0.0.0/0`、校园子网路由或全局 IP 转发。
- 不允许浏览器或其他客户端绕过 API 直接把 Gateway 当通用 SOCKS / HTTP CONNECT 代理。
- 必须先取得学校/网络管理员允许该远程接入方式的授权。若学校不允许 SSH 端口转发，不能尝试绕过限制；应改用批准的 VPN/堡垒机。

### 3.3 V2 加 ECS（控制面）

```text
                ┌─────────────────────────────────┐
                │ ECS: hmesh-hub (optional)       │
                │ Registry / Mailbox / Heartbeat  │
                │ Event summary / Web / SQLite   │
                └──────────────┬──────────────────┘
                               │ HTTPS / WSS outbound
       ┌───────────────────────┴────────────────────────┐
       ▼                                                ▼
  Mac node ◄────────── Tailscale / SSH ──────────► Windows Gateway ─SSH─► Linux
           payload/port data prefers direct path;
           hub does not run the agents and is not a VPN coordinator.
```

### 3.4 核心组件职责

| 组件 | 职责 | 禁止耦合 |
|---|---|---|
| `hmesh-daemon` | 节点身份、RPC、注册、任务引擎、授权、服务代理 | 不绑定某个 Harness 模型协议 |
| `hmesh-cli` | 管理节点/信任/任务/服务/诊断 | 不绕过 Daemon 策略直接修改 DB |
| `hmesh-mcp` | stdio MCP Tool Server，通过本机 socket 调用 Daemon | 不开放任意远程 MCP Server |
| `hmesh-transport-tailscale` | 发现候选、获取可达地址、建立 TCP；不负责 VPN | 不读取/上传 Tailscale 登录密钥 |
| `hmesh-transport-ssh` | Windows → Linux 跳板、受限端口转发 | 不提供无范围代理 |
| `hmesh-adapter-pi` | Pi RPC 管理式执行 | 不干预用户已有交互 session |
| `hmesh-adapter-dsh` | DeepSeek Harness headless JSON/可选 SDK 管理式执行 | 不复用用户 Desktop/Web/交互 profile 或既有 session |
| `hmesh-adapter-opencode` | OpenCode 非交互 JSON 事件与受控子进程 | 不附着用户现有 server/session，不默认开放 server |
| `hmesh-adapter-codex` | Codex 非交互或 SDK 管理式执行 | 不假定任意交互 session 可附着 |
| `hmesh-hub` | V2 Registry/Mailbox/Presence | 不执行用户代码或持有本地 API 密钥 |

---

## 4. 核心实体及不变量

### 4.1 身份层级

```text
NodeIdentity (设备实例，持久 node_id + public key)
 ├── AdapterInstance (某一种 Harness 适配器 + 安装版本)
 │     ├── AgentInstance (运行模式 / project / capabilities)
 │     └── ...
 ├── Project (本地允许操作的目录，不等于共享文件系统)
 ├── PublishedService (短租约 + 具体本机端口)
 └── TaskExecutions (task_id + attempt_id + assigned agent)
```

不变量：

- `node_id` 与硬件 MAC/IP 无关；Tailscale IP 变化不改变 Node 身份。
- `agent_id` 由创建节点分配 UUID；同名不同机器 Agent 仍不同。
- Agent 在线 ≠ Harness 能被管理式调用；`capability_probe` 成功后才标 `available`。
- 宣称 `gpu` 能力不等于给任务授予 GPU 调用权限，权限由本地 Policy 决定。
- 项目身份使用 `(node_id, project_id)`，不以不安全的相对路径跨机器共享。
- 远端永远不能指定任意本地 `cwd`，只能引用目标节点**已登记的 project_id**。

### 4.2 Node 与 Agent Card（项目内部类型）

```json
{
  "schema_version": "1.0",
  "node_id": "node_uuid",
  "display_name": "windows-desktop",
  "node_public_key": "base64-ed25519-pubkey",
  "platform": "windows-x86_64",
  "protocol_versions": ["hmesh/1"],
  "status": "online",
  "agents": [
    {
      "agent_id": "agent_uuid",
      "name": "pi-backend",
      "harness": "pi",
      "execution_mode": "managed",
      "availability": "idle",
      "capabilities": ["python", "http-api", "test"],
      "project_ids": ["demo-backend"],
      "declared_capabilities": ["python"],
      "verified_capabilities": ["pi-rpc-available"]
    }
  ]
}
```

客户端把这些字段当作**远端提供的声明性信息**。不会因为显示 `verified_capabilities` 就跳过本机验证和权限判断。

### 4.3 服务卡片

```json
{
  "service_id": "svc_uuid",
  "owner_node_id": "node_uuid",
  "project_id": "demo-backend",
  "name": "backend-api",
  "protocol": "http",
  "internal_target": "127.0.0.1:8000",
  "allowed_peer_ids": ["peer_node_uuid"],
  "lease_seconds": 1800,
  "expires_at": "2026-10-28T16:00:00Z",
  "visibility": "approved-peers-only",
  "health_path": "/health",
  "state": "published"
}
```

**重要：`internal_target` 可由服务拥有者看到，不应默认广播给所有节点；对方只需要代理端点描述。**

---

## 5. 传输、发现、配对与授权

### 5.0 V0 回环传输

- 通过本机 owner 显式配置的 `127.0.0.1:<port>` 定位另一个实例；不进行端口扫描，不需要 Tailscale 候选发现。
- 节点间仍使用 HTTPS/TLS 1.3、证书固定/双向认证及第 5.2 节的配对确认。`localhost` 来源不自动获得信任或项目权限。
- 使用相同的 `hmesh/1` 请求与事件契约；发布版本 V0 不意味着新增 `/hmesh/v0` 协议分支。
- `doctor` 在 `network = "loopback"` 时将 Tailscale 标为 `not_required`，不能因它未安装而阻止本机联调；TLS、实例端口、项目和所用 Harness 的检查仍须通过。
- V0 只允许回环监听；向非回环地址暴露节点属于 V0.1 显式启用的能力，不得自动开启。

### 5.1 V0.1 传输

- `tailscale status --json` 用于获取**候选 Node 地址**。该 JSON 格式官方注明可能变化，因此必须隔离在解析 Adapter 中，兼容缺失字段、尾随变化、IPv6 与 MagicDNS 不可用。
- 仅从候选列表中寻找**用户指定/授权**的端点；严禁横向扫描整个 Tailnet 的任意端口。
- Daemon 默认仅绑定回环 `127.0.0.1`；用户显式执行 `hmesh node expose --tailscale` 后绑定**选中的 Tailscale IP** 的固定端口（建议 `38880/tcp`，可配置）。
- 防火墙/Tailnet grants 限制**哪一台已授权节点能连接哪一台机器的 Mesh 端口**。
- 获取 `tailscale status --json` 失败，必须报告原因，不自动回退公网 IP。
- 端到端 P2P/DERP 由 Tailscale 决定；项目不得声称可保证直连。

### 5.2 首次节点配对（默认拒绝）

**不允许以 IP 或同 Tailnet 身份直接信任远端。** 采用“设备证书固定 + 双向确认”的初版方案：

1. Daemon 启动时生成**每节点独立的本地身份密钥**（用于签名/认证）及应用层 TLS 证书，密钥文件 OS 权限限制在当前用户可读。V2 离线消息如需端到端加密，须另建专用 **X25519/HPKE 加密密钥对**；不能把 Ed25519 签名公钥直接当作加密公钥。
2. 两端各生成带公钥指纹的 `Node Card`；`hmesh pair show` 显示 node_id、证书指纹（SHA-256）和一次性邀请信息。
3. 发起方执行 `hmesh pair request --peer <host:port> --expect-fingerprint <sha256>`；V0 使用显式回环地址，V0.1 使用授权的 MagicDNS/Tailscale 地址。地址解析须支持可选端口和带方括号的 IPv6；目标展示请求与来源信息，等待 `hmesh pair approve <request-id>`。
4. 完成双向证书/公钥固定后写 `trusted_peers`；每次连接验证**对端公钥指纹**。推荐使用应用层 TLS 1.3/mTLS（rustls），证书不依赖公共 CA；未配对前只允许速率受限的握手端点。
5. Pairing PIN（如采用）只能是短期邀请辅助，不能替代密钥指纹校验。重放的配对请求必须失效。
6. `hmesh pair revoke <peer>` 立刻阻止新任务、新连接，并关闭已建立的服务代理会话。

允许一台设备上运行多个 Daemon 实例，但必须使用不同数据目录、node_id、监听端口。

### 5.3 授权模型

权限不是单一 `admin` 开关，必须至少支持：

| Action | 默认策略 | 说明 |
|---|---|---|
| `node.read` / `agent.read` | 已配对可读最小信息 | 不泄漏路径和凭据 |
| `task.submit` | 对指定 project + agent 需授权 | 可配置每次审批 / 明确的预批准 |
| `task.cancel` | 仅提交者或本机 owner | 必须确认取消能力 |
| `service.publish` | 本机 owner 批准 | 仅 `127.0.0.1` / 明确允许的目标 |
| `service.connect` | 指定 service + peer + lease | 不能等同全网访问 |
| `project.read/write` | 本机项目配置决定 | 默认不允许原始任意路径 |
| `peer.manage` | 仅本机 owner | 不接受远端管理 |
| `hub.sync` | V2 opt-in | 默认不上传正文和文件 |

配置阶段将策略写到本机文件并由 Daemon 原子热加载（非法策略拒绝并保留旧版）。所有授权拒绝都记录安全审计摘要。

### 5.4 V0.2 自动发现

- 自动发现仅在已授权 Tailnet 内进行，分两步：Tailscale 的节点列表**候选**，随后对**已配对或用户允许发现**的候选端点调用 `GET /hmesh/v1/hello`。
- `hello` 只提供最低限度版本和公钥指纹；Agent 目录必须在认证以后返回。
- 心跳可设 10–15s，连续若干超时标 `unreachable`；短暂网络失败不能撤销信任。
- 本地缓存 TTL 30–60s（配置项）；退避轮询，加随机抖动，不因多个 peer 造成扫描风暴。
- Windows SSH Gateway 的下游 Linux 节点是**逻辑可路由节点**，需显示 `route = via windows-gateway`，不得伪造为 Tailscale 直连。

---

## 6. 网络协议与 API 契约（内部 `hmesh/1`）

> 先实现一个规模受控、可测试的内部协议；A2A 1.0 作为扩展互操作目标，而非先强制实现整套规范。

### 6.1 选择

- 节点 RPC：**HTTPS/TLS + JSON**，V0 经 loopback，V0.1 起经 Tailnet；长任务状态使用 SSE（必要时 WebSocket）；Gateway TCP 代理不解码应用层数据。
- 本机 CLI/MCP ↔ Daemon：Unix domain socket；Windows named pipe（MVP 可先使用仅本机 loopback + 随机凭据，V0.3 迁移/完善 named pipe）。
- 大文件：不嵌 JSON；V0/V0.1 只支持小型文本与摘要，工件留在执行节点授权项目内并由受控 runner 核验；V0.2 引入**明确授权的分块 artifact**，V2 默认不上云。
- 定义 `Content-Type: application/json`，HTTP 状态码与内部错误码；事件采用 SSE 带 `event_id` 和单调 `seq`。

### 6.2 标准请求包

```json
{
  "version": "hmesh/1",
  "request_id": "req_uuid",
  "sender_node_id": "node_uuid",
  "timestamp": "2026-10-28T08:00:00Z",
  "operation": "task.submit",
  "payload": {},
  "trace_id": "trace_uuid"
}
```

- `request_id` 用于接口重试去重；限制消息体大小（例如普通 JSON 1 MiB）。
- node 身份来自 mTLS/密钥校验，**不是**信任请求包里的 `sender_node_id`；请求字段必须与实际身份一致。
- 时间戳仅用于诊断与重放检查；跨机器时钟可能漂移，主身份与去重基于密钥、随机 nonce 和持久化 ID。
- 所有输入通过 serde 校验，对未知 `version` 返回可解析的 `UNSUPPORTED_VERSION`，不执行不兼容任务。
- 用 `trace_id` 关联 Task、服务调用与测试结果，日志中不包含私钥/模型 API key。

### 6.3 初版 HTTP API

| Method | Path | 权限 | 版本 |
|---|---|---|---|
| `GET` | `/hmesh/v1/hello` | 未配对（极简） | V0 |
| `GET` | `/hmesh/v1/node` | `node.read` | V0 |
| `GET` | `/hmesh/v1/agents` | `agent.read` | V0 |
| `POST` | `/hmesh/v1/tasks` | `task.submit` | V0 |
| `GET` | `/hmesh/v1/tasks/{id}` | 任务所属 peer | V0 |
| `GET` | `/hmesh/v1/tasks/{id}/events?after=SEQ` | 任务所属 peer | V0 |
| `POST` | `/hmesh/v1/tasks/{id}/cancel` | `task.cancel` | V0 |
| `GET` | `/hmesh/v1/services` | `service.read` | V0.2 |
| `POST` | `/hmesh/v1/services` | `service.publish`（仅本机 API） | V0.2 |
| `POST` | `/hmesh/v1/services/{id}/connect` | `service.connect` | V0.2 |
| `POST` | `/hmesh/v1/services/{id}/revoke` | owner | V0.2 |
| `GET` | `/hmesh/v1/artifacts/{id}` | 明确 artifact grant | V0.2 |

**注意**：远端不能调用 `POST /services` 替自己发布其所不拥有的资源；该路由对远端应返回 `403`，或仅暴露在本机 API。

### 6.4 任务提交请求

```json
{
  "version": "hmesh/1",
  "request_id": "bcd0bd6c-94cd-43ad-9dba-8c046b89e233",
  "operation": "task.submit",
  "payload": {
    "task_id": "task_uuid",
    "target_agent_id": "agent_uuid",
    "project_id": "demo-backend",
    "goal": "实现 /api/health 并运行测试",
    "inputs": [{"type": "text", "name": "acceptance", "value": "HTTP 200, {status: ok}"}],
    "limits": {"timeout_seconds": 600, "max_output_bytes": 1048576},
    "requires_approval": true
  }
}
```

**从 V0 起，不允许对端传入任意 shell、执行绝对路径或任意文件挂载参数**。需要本机 Adapter 自己构造安全的启动方式，按 Project 配置决定工作目录、工具许可及审批。

### 6.5 任务状态机

```text
CREATED → PENDING_APPROVAL → QUEUED → RUNNING ──────→ SUCCEEDED
    │            │           │        ├───────────→ FAILED
    │            └→ REJECTED  │        ├───────────→ CANCELLING → CANCELLED
    │                        │        ├───────────→ WAITING_INPUT
    │                        │        └───────────→ UNKNOWN
    └─────────────────────────┴─────────────────────→ EXPIRED
```

- `WAITING_INPUT` 经显式继续操作返回 `RUNNING`；V0/V0.1 若 Adapter 不支持交互，必须终止为明确的 `FAILED_NEEDS_INPUT`，不能静默死等。
- `UNKNOWN` 表示进程状态或外部副作用不可证实，**不是失败，也不是可以自动重试的安全状态**。
- `CANCELLED` 只有进程确实确认停止或受控关闭后才可写；发出了取消请求不代表取消成功。
- `SUCCEEDED` 表示 Harness 有明确成功结果；是否满足业务验收由独立 `Verification` 表记录，不可只读模型一句“完成了”。
- 服务端保存合法转换表，拒绝从终态回到 `RUNNING`；重试产生新的 `attempt_id`，task_id 不变。

### 6.6 幂等、顺序与重连

- `(sender_node_id, request_id)` 建唯一索引，重复请求返回**已有任务 ID 和状态**，不创建重复 Run。
- `(task_id, attempt_id)` 标识唯一执行尝试，Local Executor 使用互斥 Lease 阻止同时启动同一个 attempt。
- 任务事件追加 `seq INTEGER` 单调增长；订阅断开后以 `after=seq` 恢复，先从 DB 补事件再进入流式监听，避免补流期间丢事件。
- 网络 ACK 丢失时只重发相同 `request_id`，不得另建任务。
- Run 进程崩溃后可能存在已发生但未记录的副作用。从 V0 起保守标 `UNKNOWN`、阻止自动重跑；V1 引入人工核实与可配置恢复策略。
- 取消传播采用 deadline + kill-tree 尽力而为；取消结果必须由 Adapter 确认，若无法确认则标 `UNKNOWN`。

### 6.7 标准错误结构

```json
{
  "error": {
    "code": "PERMISSION_DENIED",
    "message": "Peer not allowed to submit to project demo-backend",
    "retryable": false,
    "trace_id": "trace_uuid"
  }
}
```

错误码必须至少含：`UNAUTHENTICATED`, `PERMISSION_DENIED`, `PEER_UNREACHABLE`, `AGENT_UNAVAILABLE`, `ADAPTER_NOT_SUPPORTED`, `PROJECT_NOT_FOUND`, `APPROVAL_REQUIRED`, `TASK_CONFLICT`, `TASK_UNKNOWN`, `SERVICE_EXPIRED`, `SERVICE_UNHEALTHY`, `UNSUPPORTED_VERSION`, `RATE_LIMITED`, `INTERNAL`。

---

## 7. Harness Adapter 体系（核心工程难点）

### 7.1 抽象接口

Rust `trait HarnessAdapter`（参考签名，具体可按 async_trait / boxed futures 实现）：

```rust
pub trait HarnessAdapter: Send + Sync {
    fn kind(&self) -> HarnessKind;
    async fn probe(&self, config: &AdapterConfig) -> Result<ProbeReport>;
    async fn start(&self, run: ManagedRunSpec) -> Result<RunHandle>;
    async fn events(&self, run: &RunHandle) -> Result<RunEventStream>;
    async fn cancel(&self, run: &RunHandle) -> Result<CancelOutcome>;
    async fn inspect(&self, run: &RunHandle) -> Result<RuntimeState>;
    async fn cleanup(&self, run: &RunHandle) -> Result<()>;
}
```

`ProbeReport` 必须能区分：未安装、版本不兼容、凭据未配置、可以启动、是否支持 streaming、是否支持 cancel、是否支持 session resume。**不要用一个布尔值掩盖能力差异。** V0 的 `HarnessKind`/配置标识包括 `pi`、`dsh`、`opencode`；V0.1 增加 `codex`。模型 provider/model 作为独立字段保存。

### 7.2 Pi Adapter（V0 P0 第一接入，V0.1 必交）

- 首选官方 Pi `--mode rpc` 模式，JSONL stdin/stdout；固定测试兼容版本，检测当前安装模式与二进制路径。
- 本机核查基线为 `@earendil-works/pi-coding-agent@0.84.2`；使用 `PI_CODING_AGENT_DIR` 和 `--session-dir` 将运行配置/会话分配到所属 Node/Managed Run，禁止误用其他 Node 的目录。
- 严格以 LF 分割 JSONL；不能以 Unicode 行分隔符作为协议分隔符。
- 每个 Managed Run 启动**独立 Pi 受控子进程**或一个具备明确独占 session 的实例，配置指定 cwd、模型配置和隔离的 session-dir。
- 不触碰其他用户 Pi 会话；不从 `~/.pi` 擅自收集历史聊天。
- 事件字段映射 `RunEvent::Started | TextDelta | ToolEvent | ApprovalRequested | Finished | Failed`。
- 密钥由本机原生 Harness 配置读取；**绝不写入任务消息、Hub 数据库或审计日志**。

### 7.3 Codex Adapter（V0.1 必交，不阻塞 V0）

- 第一阶段采用**Codex 官方非交互 `codex exec`** 或官方 SDK；选择可通过实测跨 macOS/Windows 覆盖的方式。
- 必须通过能力探测确认参数、事件格式；事件映射到同一 `RunEvent`。
- 使用指定的 repo 工作目录和 Codex 自身 sandbox / approval policy；绝不默认传危险的全权限跳过审批参数。
- 对支持的 CLI 版本保留 `FixtureParser` 回归测试；不支持的版本明示 `ADAPTER_NOT_SUPPORTED`。
- 不宣称能接管 Codex App 既有线程；先创建独立 Managed Run。对长会话的 Resume 能力留到 V1 可选扩展。

### 7.4 Adapter 接入优先级与门禁

| Harness | 时间 | 处理方式 |
|---|---|---|
| Pi | V0 P0，第一接入；V0.1 必交 | 先以 RPC 打通本机双 Node 的第一条真实任务 |
| DeepSeek Harness / DSH | V0 P0，第二接入 | 优先 headless JSON 受控子进程，与 Pi 验证双向委派；SDK 作为后续增强 |
| OpenCode | V0 P0，第三接入 | 优先 `run --format json`，复用同一任务契约与本机 E2E |
| Codex | V0.1 必交 | 非交互执行、状态/错误回传与跨设备/跨 OS 证据；不阻塞 V0 |
| Claude Code | V1 后 | 经官方 headless / SDK 验证许可与事件模型 |
| ZCode | V2 后的 Adapter 插件候选 | 先确认稳定、可公开维护的外部调用接口 |

V0 采用三个检查点：**Pi 单执行端闭环 → Pi/DSH 双向委派 → OpenCode 执行端回归**。三种 P0 Adapter 各自必须有真实模型/工具任务、工件和确定性测试证据，不能仅凭 help、无模型协议响应或 Mock 宣称 V0 完成。凭据/模型或能力尚未验证时记录具体阻塞，保留已完成检查点，不静默删除该 P0。V0.1 仍要求 Pi 与 Codex 两种 Harness、两种 OS 的真实互调；复用 V0 的 DSH/OpenCode 不替代原定跨设备门禁。

Harness 种类与模型供应商是两层：Pi 或 OpenCode 使用 DeepSeek 模型不等于接入 DeepSeek Harness；三者可以使用同一获授权模型供应商，但必须启动各自真实 runtime。

### 7.5 模型/工具审批边界

- 外部 Agent 只能提交**目标**，不能直接向执行节点下达 `sudo`、系统设置更改或任意 shell 访问授权。
- 可以让本机 owner 预先配置：哪些 project、Agent、Tool 授权给某个 peer；高危操作一律显式审批。
- 必须区分 `task.submit`（接受任务）和 `tool.approval`（执行过程中对具体工具调用审批），前者**不能取代后者**。
- 遵守各个 Harness 自身 sandbox / approval 功能；Harness 没有可验证审批能力时，必须提示用户采用独立 OS 用户、容器或仅限受信任测试项目，不能宣称安全隔离。

### 7.6 DeepSeek Harness Adapter（V0 P0）

- 本机命令为 `dsh`，安装包为 `@deepseek-ai/dsh@0.2.0-rc.2`。P0 首选已核查的 `dsh --profile headless --json`，任务正文经 stdin 输入；该 profile 一次运行一个任务并退出。
- 为每个 Node/Managed Run 设置独立 `DSH_HOME`，cwd 由本机已登记 Project 决定；使用受控 profile 和明确允许的工具/凭据配置，不继承用户 Desktop/Web/TUI 的补丁、插件或既有会话。
- headless JSON 事件映射到统一 RunEvent，保留 `session`、`status`、`text`、`tool_call`、`tool_result`、`final`、`error` 的 fixture。该版本的文本投影按提交步骤输出，不能宣称逐 token streaming。
- **`final` 存在不代表成功**：联合进程退出码、`turn_end` 原因、错误事件与测试证据判断；不得把空 final、退出码 1 或 aborted 映射为 `SUCCEEDED`。超时/取消必须确认受控进程停止，否则 `UNKNOWN`。
- SDK 增强可使用已核查入口 `dsh --profile sdk`，通过 stdio JSON-RPC；后续实现仍须验证 `initialize`、模型路由、session/prompt、事件及取消，不能由本次 `shutdown` 探测推断这些能力可用。
- 不通过 `--dump-config` 收集用户合成配置或凭据。profile/session resume 不作为 V0 前置，不得接管已有用户会话。

### 7.7 OpenCode Adapter（V0 P0）

- 本机安装包与隔离 CLI 版本均为 `opencode-ai@1.18.20`。P0 首选已核查的 `opencode run --format json --dir <registered-project>`，由 Adapter 构造参数并解析真实 JSON 事件；任务正文的传递方式需在契约测试中验证。
- 为各实例/Run 分配独立配置、数据、缓存、状态目录和 session；本机探测使用进程级 `XDG_CONFIG_HOME`、`XDG_DATA_HOME`、`XDG_CACHE_HOME`、`XDG_STATE_HOME` 与 `OPENCODE_CONFIG_DIR`。不能仅设置 CONFIG_DIR 就宣称凭据和持久化已隔离，实施时要验证有效路径。
- Managed Run 创建新 session；不默认使用 `--continue`、`--session`、`--attach`、`--share` 或用户已运行的 server。锁定 fixture 版本，联合事件和进程退出结果判断运行结局。
- 该版本 `--help` 将 `--auto` 标为危险的自动批准选项，**不得默认传入**。必须显式限制工具权限，验证非交互模式遇到审批时的实际行为；必要时返回 `FAILED_NEEDS_INPUT`，不能自动扩大权限。
- V0 不要求额外启动 OpenCode server/SDK。未来若采用 server，必须独立回环监听、认证和生命周期管理，不暴露到 Tailnet，不借此绕过 Mesh 策略。

### 7.8 本机安装与接口核查记录（2026-10-28）

环境为当前 Windows 电脑；下表保留**接入前的版本/接口探测快照**，不将该探测计作真实模型 E2E。后续 Rust 实施与真实联调结果见第 7.9 节。隔离复测以全新临时配置目录运行，不向模型发送任务，未主动读取个人凭据文件或输出 API key，也未修改用户已有 Harness 配置。

| Harness | 已安装包 / CLI 版本 | 已实际执行的探测 | 本次结论 |
|---|---|---|---|
| Pi | `@earendil-works/pi-coding-agent` / `0.84.2` | `--version`、`--help`；隔离 RPC 下 `get_state` 与空闲 `abort` 均返回 `success: true`，EOF 后退出 0，stderr 为空 | 安装及无模型 RPC 控制路径通过；模型/工具执行与运行中取消待测 |
| DeepSeek Harness / DSH | `@deepseek-ai/dsh` / `0.2.0-rc.2` | `--version`、`--help`、`--profile headless --help`（确认 `--json`）、`--profile sdk --help`；隔离 SDK 对 JSON-RPC `shutdown` 返回 result 后退出 0，stderr 为空 | headless 参数与 SDK 启停通过；SDK initialize、凭据、真实任务及取消待测 |
| OpenCode | `opencode-ai` / `1.18.20` | 隔离 XDG/config 目录后 `--version`、`run --help` 退出 0，确认 `--format json`、`--dir` 与 `--auto` 的实际语义 | 安装与 CLI 入口通过；真实 JSON 运行事件、凭据、任务与取消待测 |

本次受限环境最初直接执行 Pi help 时出现个人 settings 锁的 `EPERM`，直接执行 OpenCode 时出现默认 `.config/opencode` 初始化的 `EEXIST`/访问失败；改用临时隔离目录后上述探测通过。不得据此断言用户正常终端中的安装损坏，或尝试删除/重写个人配置来修复测试。

本次没有验证模型凭据、真实生成文件/工具执行、活动 Run 的取消、跨 Node 传输或跨 OS。Pi 的空闲 `abort` ACK 和 DSH 的 `shutdown` ACK 不能证明任务取消成功。`probe()` 必须分别记录 `installed`、`version_supported`、`protocol_probe`、`authentication`、`managed_run`、`cancel`，未验证项为 `not_checked`，不能提前标 `available`。

上游文档与本机安装可能存在差异：实施以此处安装版本、其自带文档、实际 `--help` 和保留的协议 fixture 为基线；不复制最新文档中的未核查参数或事件字段。官方参考见第 24 节。

### 7.9 V0 P0 实施与真实验收（2026-10-28）

已在当前 Windows x64 电脑完成 Rust V0 P0。两个独立 Daemon 采用不同身份、数据库、端口和运行目录，经真实回环 TLS/mTLS 执行。验收顺序为 Pi → DSH → OpenCode，各自先通过 owner 本机真实验证，再完成 A→B/DSH、B→A/Pi、A→B/OpenCode。

| Harness | 实际受控入口 | 本次真实结果 | 仍未宣称验证的能力 |
|---|---|---|---|
| Pi 0.84.2 | `node` + `@earendil-works/pi-coding-agent/dist/cli.js`；RPC stdin | DeepSeek 真实模型、write/read 工具、最终 stop/agent_settled/退出 0、事件与工件断言通过 | 原生运行中 abort、已有 Session 接管/恢复 |
| DSH 0.2.0-rc.2 | `node` + `@deepseek-ai/dsh/lib/bin.js`；headless JSON + 每次运行 patch | `deepseek-official` 真实模型、受控读写、final/退出 0、事件与工件断言通过 | SDK initialize/prompt、原生取消、已有 Session 恢复 |
| OpenCode 1.18.20 | `opencode-ai/bin/opencode.exe`；`run --pure --format json` | DeepSeek 真实模型、显式工具权限、最终 stop/退出 0、事件与工件断言通过 | 原生运行中 abort、已有交互 Session 恢复 |

本次真实模型为 `deepseek-v4-pro`，凭据仅使用用户授权的 `DEEPSEEK_API_KEY` 环境变量，未保存其值。三者创建/读回各自项目内 `out/health.json`；Daemon 的确定性 JSON 断言与受控 runner 的独立内容/SHA-256 检查均通过。每条委派的重复 request-id 未增加 attempt 或修改工件，重启后仍可从原发起端查询。

真实 smoke 单独显式 opt-in：`pwsh -NoProfile -File scripts/e2e-local.ps1 -RunRealHarnesses`。不加开关只执行不调用模型的 fmt/Clippy/test/build。普通测试使用明确标识的 Rust 受控进程和协议 fixture，覆盖运行中取消、超时、子进程清理、异常输出、凭据缺失和 Windows Daemon 硬退出后 UNKNOWN/不重跑；这些不冒充真实 Harness 原生 abort 已通过。

验收证据：`docs/evidence/latest-p0-smoke.json` 保存 OS/Rust、包版本、实际入口与命令模板、二进制/锁文件 hash、节点/task/attempt、事件轨迹、工件断言、去重及重启结果。`docs/evidence/dependency-audit.json` 记录整个锁文件的 OSV 审计（232 个公开依赖、零命中）；`docs/implementation-status.md` 将 L01-L05/E2E-00 映射到测试和文档。

边界：本次通过的是 **V0 本机 Windows**。三平台 CI 已配置但尚未取得本仓库远端执行结果；跨电脑、跨 OS、Tailnet、Codex/MCP 仍由 V0.1 验收。Windows Managed Run 取消使用经受控进程测试的 Job Object；Unix 进程组与 detached 子进程清理不计入本次通过范围。

---

### 7.10 首条 SSH 跨机真实检查点（2026-10-28）

根据用户选择，先以 SSH TCP 转发连接本机 Windows 和 Orion/Linux，保留同一套回环 HTTPS/mTLS、双向配对、项目 grant 和审批。当前已完成 **Windows CLI → Windows Daemon → SSH → Orion Daemon → Pi 1.4.0 → deepseek-v4-flash** 的真实任务，原模型 API 为 `api.deepseek.com`，模型定义沿用 Orion 的自定义 Flash 条目。

Pi 实际 write/read `out/health.json`，独立 JSON 断言与 Windows 通过 SSH 发起的 SHA-256 核验一致；任务事件、游标补读、同 request-id 去重通过。任务 ID 为 `f2346c90-aa07-4a73-8b6b-fedd9fbf41a7`。脱敏证据位于 `docs/evidence/latest-ssh-smoke.json`；复现、端口关系与边界见 `docs/ssh-first-smoke.md` 和 ADR 0002。

该检查点新增 Pi 1.4.0 的版本支持、owner 指定 NVM `node_executable`、只复制非认证元数据的 `models_file`，以及 Pi/DeepSeek 专用的只读 `credential_source`。认证在 Orion 内存中注入子进程并脱敏，不复制认证文件、不传给 Windows；原文件未修改。两个测试 Daemon 与专用 SSH 隧道已停止，原 7892 转发保持工作。

本次只证明单向 SSH 跨机链路。Orion→Windows 委派、SSH 中断恢复、Tailscale、Codex/MCP 和完整 E2E-01 仍未验收，**不勾选完整 V0.1 门禁**。V0.1 的 Tailscale 主链路规划保持，SSH 用作先行跨机检查点；V0.2 的受管 Gateway/服务发布仍另行交付。

### 7.11 SSH 前后端应用检查点（2026-10-28）

按用户要求完成 `examples/ssh-todo`：Orion 上的 **Pi 1.4.0 / deepseek-v4-flash** 编写 Node.js 服务端，本机 **DSH 0.2.0-rc.2 / deepseek-v4-pro** 编写 HTML/CSS/JS 客户端及静态/API 代理。两端按同一份 `api-contract.json` 独立生成，业务代码无 npm 依赖。任务说明、受控 runner、生成文件和独立 HTTP 验收脚本均已保存。

运行链路为 **浏览器 → Windows 127.0.0.1:5173 → 127.0.0.1:18080 → SSH LocalForward → Orion 127.0.0.1:8000 → Orion JSON 数据文件**。代码生成阶段仍使用独立 Node 身份、HTTPS/mTLS、项目 grant 和 Orion owner 审批；应用运行阶段没有模型调用。服务只监听回环地址，应用隧道与模型生成用的 Mesh 隧道分开管理。

独立验收通过：Orion 服务端 10 项测试、本机客户端 4 项测试；跨机 HTTP 新增/完成/读取及 400/404/413；浏览器实际新增、勾选、刷新；核对 Orion 磁盘文件后重启后端、重载页面，数据和完成状态仍保留。三份接口契约 SHA-256 一致，导出的服务端文件与 Orion 文件 SHA-256 一致。脱敏记录见 `docs/evidence/latest-todo-integration.json`，使用与停止命令见 `examples/ssh-todo/README.md`。

**任务状态与产物验收分开记录**：DSH 任务 `29fa9a8e-f879-4ef6-a985-38c4a420f383` 为 SUCCEEDED。Pi 首次任务达到默认 stdout 额度；单独提交并获批的后续任务 `c5e3ecaf-87b6-4393-bec3-cb906b9a621b` 写出全部服务端文件后触发 JSONL 单条记录保护，状态保持 FAILED。runner 导出并测试这些实际文件，不将应用测试通过改写为 Pi 任务成功，也不自动重跑有副作用的任务。

本次增加显式 `task send --max-output-bytes`，允许代码生成任务申请至多 32 MiB 的累计输出；默认仍为 1 MiB，当时单条原生 JSONL 仍限制 64 KiB。该单条限制问题于次日按第 7.12 节修复，本节保留原始失败事实。该版本 Rust fmt、Clippy 和 33 项测试通过。此示例验证手动 SSH 应用联调，**不代表受管 Service/Gateway、Tailscale 或完整 V0.1/V0.2 交付**。代码生成 Daemon 和 Mesh 隧道在验收后已停止，应用服务与应用 SSH 转发保留供试用。

### 7.12 原生大消息修复与真实回归（2026-10-29）

原生消息与 Mesh 内部事件采用不同额度：Pi 原生 JSONL 单条最多 4 MiB，DSH/OpenCode 单条最多 1 MiB，均同时受任务累计 stdout 额度约束。累计默认 1 MiB、显式最多 32 MiB、超时和畸形消息检查保持有效。原生超限仍终止运行并返回 RATE_LIMITED，不丢弃大消息来伪造成功。

内部 payload 上限仍为 64 KiB。正文先执行跨消息脱敏，再按 UTF-8 边界拆为每段至多 8 KiB 的事件；长正文不再套用诊断摘要的截断。原生工具参数/结果与快照不进入事件库。事件每页另限制为 512 KiB，并修复 `task watch` 在终态出现后读取完全部分页才退出。output_summary 只记录字节数、完整消息计数与最大消息大小。

新的隔离 Orion 项目使用 Pi 1.4.0 / deepseek-v4-flash，经相同 SSH、mTLS、项目 grant 和 owner 审批运行。任务 `c2c13fd7-eae0-42e4-8945-99c6784129ba` 先读取 runner 构造的引号/反斜线测试文件，再重新生成完整 Todo 服务端，最终原生完成、退出 0、清理成立，状态 **SUCCEEDED**。实际解析 27,157 条原生消息，最大单条 **375,131 字节**，累计 stdout **8,758,986 字节**；确实越过旧 64 KiB 边界。新服务端的 14 项测试全部通过；原应用的服务端 10 项和客户端 4 项测试、跨机 HTTP 与原待办持久化也重跑通过。

修复不改写第 7.11 节历史 FAILED，也没有用新生成代码覆盖正在试用的原服务端。隔离生成文件按 SHA-256 核验并保存在私有运行记录。实现决策见 ADR 0003；执行入口为 `scripts/retest-pi-output.ps1`，脱敏结果见 `docs/evidence/latest-pi-output-regression.json`。最终两端回归数量见该报告；原应用继续通过专用 SSH 应用隧道运行。

### 7.13 SSH 双向与断线恢复（2026-10-29）

Windows→Orion Pi/Flash 与 Orion→Windows DSH/Pro 的真实任务均已通过。反向 DSH 在 RUNNING 时切断专用 Mesh 隧道，并正常停止 Orion 发起端 Daemon；Windows 执行继续，断线期间完成。发起端沿用原身份和数据库重启、恢复隧道后，观察到相同 task/attempt，补读 10 条后续事件且与执行端 seq 序列一致。重复相同 request_id/正文没有新增 attempt 或运行事件，工件 hash 不变。该结果覆盖 SSH 中断与发起端正常重启，执行端硬崩溃/脱离进程组的子进程仍另行验收。

正常正向任务为 `3f090707-fcd3-46da-8f8e-7bd0162387dd`，反向任务为 `35c153a8-07e8-4b8d-88cf-8716f5212e72`，均 SUCCEEDED 并通过独立 JSON/hash 断言。首条正向任务因同机 Daemon 停止被取消、DSH 首次产物路径不符等过程事实均保留，详见 `docs/ssh-bidirectional-mcp.md`。这些检查点继续使用 SSH，不勾选完整 Tailnet/Codex/Mac↔Windows 的 V0.1 发布门禁。

### 7.14 Managed Pi 原生 MCP 主动委派（2026-10-29）

新增 Rust `hmesh-mcp`，提供五个 stdio 工具，固定连接本机 owner RPC。Pi 1.4.0 的 owner Agent 配置可显式选择 `mesh_server` 和 `mesh_scope`；Runtime 只额外加载原生 `builtin:mcp` 和本次生成的注册扩展，不导入用户 MCP 配置。工具限制到精确 peer/Agent/project 和有限次数，提交仍需执行端独立批准，模型没有 approve/policy 接口。

真实 Orion Pi/Flash 父任务 `6b268118-6f6c-4b24-99cd-89248653b523` 亲自调用发现节点、发现 Agent、提交和查询工具，创建 Windows DSH 子任务 `15536b80-bf73-44fc-bb8c-e51a828a51fc`。runner 观察到子任务 PENDING_APPROVAL 且无 attempt，再从 Windows owner CLI 批准。父子任务都 SUCCEEDED，nonce 和回传 artifact SHA-256 与两端实际文件一致。取消工具的真实模型调用、DSH 作为 MCP caller 尚未计入通过范围。

SQLite schema 3 原子保存 parent/request/child 绑定和额度，跨进程重启仍生效；父任务结束、目标越界、其他父任务的 child_id、未知执行参数和超限输入均拒绝。断线不释放额度、不自动换 UUID 重跑。普通测试使用合成身份/任务和真实 stdio/TLS 进程，真实模型证据单独记录。配置与边界见 `docs/mcp.md`，结果见 `docs/evidence/latest-ssh-mcp-smoke.json`。

### 7.15 受管 HTTP 服务、SSH 隧道与有限修复（2026-10-29）

在既有 Windows ↔ Orion/Linux 的授权 SSH 路径上实现单独检查点，不将其计为完整 V0.1/V0.2 或 Mac 三机 Gateway 验收。实际 CLI/JSON 入口见 [受管服务说明](docs/managed-services.md)，SQLite schema 4 新增服务、隧道、连接租约与 IntegrationRun；旧任务和委派记录保留。

- Daemon 负责自己启动的进程树、租约回收及初启后三次有退避的重试；进程、SSH、HTTP 健康分别报告。owner 明确项目、可执行文件、端口和 peer，拒绝接管占用端口/历史 PID。
- 服务通道为具名服务的 mTLS HTTP 桥接，两侧正文各限 256 KiB，只在本机回环建立入口；不携带 Cookie/Authorization，不跟随重定向，不自动重放写请求。HTTP 总预算由 owner 设为 1..30 秒，Mesh 另留 5 秒；示例前端/客户端为 45 秒。此项不宣称通用 TCP、WebSocket 或 SSE。
- SSH 使用显式身份与 known_hosts、strict host key、专用空配置及最多两个精确回环转发；保留用户原 SSH 配置与 7892 转发。三机 route/Gateway 仍另行验收。
- IntegrationRun 冻结 API JSON 原文/hash/revision 与 GET/JSON Pointer 断言；失败后复用原任务引擎向确定的后端 Agent 交付最多两次修复，远端审批继续生效。任务成功后仅可重启 owner 已委派的固定服务，再运行原契约。测试报告保留响应 hash/长度与断言，不存响应正文。
- Daemon 重启不复活旧租约或重放中断联调；保留配置/证据并将未完成运行置为 needs_input。OS 常驻安装、Unix SIGKILL 后孤儿进程接管、通用 Artifact 获取和 MCP 服务管理工具未交付。

真实验收过程与已知失败须一起保留于 `docs/evidence/latest-managed-services.json`；不得把代理/超时失败重写为模型修复成功。最终检查点以独立客户端、后端单测、固定契约、隧道恢复与撤销访问的实际结果为准。

最终真实运行 `ed972039-357c-4f3d-a989-7df388f9bdc6` 已一轮修复后 verified，Orion Pi 修复任务 `00d63a84-8d6a-46a4-8771-a1c42d884062` 为 SUCCEEDED；同一版本 DSH 客户端在修复前失败、修复后通过。后端 10 项、客户端 4 项测试、独立前端 HTTP、受管重启后数据保留、SSH 子进程停止后的自动恢复与远端撤销关闭本地入口均通过。最终 Windows 52 / Linux 51 项 Rust 测试通过；格式与 Clippy 在 Windows 通过，Orion 便携工具链无对应组件，仅独立运行 Linux 构建/测试。最初代理超时与固定 5 秒/1 秒限制导致的失败保留在报告中；操作预算与调用方余量已明确化，没有自动重放旧写请求。

## 8. CLI 与 MCP Tool 规范

### 8.1 命令列表（拟定 CLI）

从 V0 起支持全局参数 `hmesh --data-dir PATH <command>`；`init`、`doctor`、`daemon`、配对、注册和任务命令都必须使用选中实例的配置、身份及本机 API 凭据。重复 `init` 不得覆盖已有密钥。V0 不要求 MCP；V0.1 再增加第 8.2 节的工具接入。

```bash
hmesh init                            # 本机身份 + 默认 deny 配置
hmesh doctor                          # V0 实例/OS/身份/Daemon；Adapter 结果见 agents list
hmesh daemon start                    # 常驻服务
hmesh daemon status
hmesh daemon stop
hmesh node show                       # 当前节点
hmesh node expose --tailscale         # V0.1：显式绑定选定 TS IP
hmesh peers candidates                # 仅列 Tailnet 候选，不自动信任
hmesh pair show                       # 显示本机指纹
hmesh pair request --peer NAME --expect-fingerprint SHA256
hmesh pair approve REQUEST_ID --expect-fingerprint SHA256
hmesh pair revoke PEER_ID
hmesh peers list
hmesh agents list --peer PEER_ID
hmesh agent register --config agent.toml
hmesh agent verify AGENT_ID --allow-model  # 显式真实模型验证
hmesh policy --file policy.toml
hmesh task send AGENT_ID --peer PEER_ID --project PROJECT_ID --prompt TEXT
hmesh task list
hmesh task show TASK_ID
hmesh task watch TASK_ID
hmesh task cancel TASK_ID
hmesh task approve TASK_ID                 # 执行端本机 owner
hmesh task reject TASK_ID                  # 执行端本机 owner
hmesh task verify TASK_ID --artifact out/health.json --expect-json '{"status":"ok"}'
hmesh task retry TASK_ID --confirm         # 后续阶段；V0 不自动重试
hmesh service publish --config service.toml   # V0.2
hmesh service list                           # V0.2
hmesh service connect SERVICE_ID --local-port 18000  # V0.2
hmesh service revoke SERVICE_ID              # V0.2
hmesh gateway add --config gateway.toml      # V0.2
hmesh hub login / sync / status              # V2
```

CLI 输出两种模式：人类可读表格和 `--json` 稳定机器格式。`--json` 的错误必须写到统一对象；返回码 `0=成功, 2=用户输入, 3=授权, 4=不可达, 5=执行失败, 6=未知状态`（可在接口冻结前调整）。

### 8.2 MCP 接入（V0.1 先只读 + 任务）

通过标准 **stdio MCP Server** 将工具暴露给 Harness，MCP Server 只连接本机 Daemon，不自行持有 Tailnet 凭据：

```text
mesh_list_peers()
mesh_list_agents(peer_id?)
mesh_get_task(task_id, after?, wait_seconds?)
mesh_submit_task(peer_id, agent_id, project_id, request_id, goal, timeout_seconds?, max_output_bytes?)
mesh_cancel_task(task_id)
```

V0.2 再加入：

```text
mesh_list_services(peer_id?)
mesh_request_service_connection(service_id, local_port?)
mesh_publish_local_service(project_id, name, target, ttl?)
mesh_get_artifact(task_id, artifact_id)
mesh_request_retest(task_id, test_contract_id)
```

- 所有 Tool Schema 明确 required、限制字符串长度、拒绝未知字段。
- MCP 模型调用不会自动获得 approval token；用户批准应经过本机 CLI/GUI 或受信任的本机授权策略。
- 对远端对象列表/日志/工件中的文本执行**不可信输入处理**；MCP 工具结果不能作为扩大权限的指令。
- 最新 MCP 协议有不同 SDK 主版本。应在实现时锁定经过集成测试的 SDK 版本，并记录兼容矩阵。

当前实现锁定 MCP 2025-11-25 stdio/tools 子集，复用现有 Rust 依赖而未引入额外 SDK。Pi 1.4.0 原生客户端互操作已实测。服务器要求正在运行且由 owner 启用 Mesh scope 的 Managed Parent；精确参数、权限和部署入口以 `docs/mcp.md` 为准。acceptance/test-contract 工具参数留待 V0.2。

---

## 9. 开发服务发布与自动联调（V0.2 重点）

### 9.1 Service 生命周期

```text
DRAFT → PENDING_APPROVAL → PUBLISHED → ACTIVE → EXPIRED
                    └→ REJECTED        └→ REVOKED
```

- 仅允许发布明确 `localhost` 服务或配置白名单目标，默认不得暴露 `0.0.0.0` 上的所有端口。
- 每个 Service 必须绑定 `project_id`、`owner node`、`owner agent`、`protocol`、`port`、`peer ACL`、`TTL`、审计信息。
- 发布者在本机验证监听端口可达后才标 `PUBLISHED`；默认 TTL 30 分钟，续租必须获得策略允许。
- 发布和连接是独立授权，Service 上线不意味着所有配对节点都可访问。
- 接收方默认连接到 **本机 loopback 随机端口**，返回 endpoint 给当前 Agent；结束后回收 listener、连接与令牌。
- 每个 publish / connect 都有独立 `lease_id`，并限制最大并发、连接时长、字节数、目标协议和端口。

### 9.2 V0.2 实施策略

**优先使用 Daemon 管理的 TCP forwarding**，不要依赖不同 OS 上 `tailscale serve` 的行为一致性。Tailscale 只负责底层节点可达。

```text
Mac localhost:18000
  ↓ local hmesh TCP proxy (lease + peer access)
Mac Daemon ── Tailscale mTLS/authorized stream ──► Windows Daemon
  ↓ (可选：SSH Gateway)
Windows ssh -L 127.0.0.1:PORT:127.0.0.1:8000 linux-user@linux-host
  ↓ 校内 SSH
Linux 127.0.0.1:8000 FastAPI
```

如 Linux 无需 Gateway 且加入 Tailnet，直连 remote Daemon 即可。**绝对不能把 `ssh -L` 包装成不受限的通用端口转发 API。**

SSH Gateway 配置建议：

```toml
# gateway.toml —— 仅示例；部署时须明确授权
[gateway]
id = "lab-linux"
method = "ssh"
ssh_host = "192.168.10.20"
ssh_user = "developer"
ssh_host_key_policy = "strict"
allow_remote_host = "127.0.0.1"
allowed_remote_ports = [38880, 8000]
allow_projects = ["demo-backend"]
max_tunnels = 2
```

- Gateway 只能连接显式允许的 SSH 目的主机，且要求 known_hosts 正确匹配；禁止 `StrictHostKeyChecking=no`。
- `ssh -L` 仅在 Windows 本机回环监听，Mac 不直接连接该 SSH 本地端口；Mac 通过受控 Mesh 代理访问。
- Linux 的 Daemon 可经专用 SSH tunnel 让 Gateway 代理注册/任务请求，Linux 节点拥有**自己的节点密钥**；Gateway 不应伪造 Linux 身份。
- 经过 Windows Gateway 的数据机密性：Mesh 控制通信应在 Mac↔Linux 之间**端到端认证加密**，Gateway 仅转发密文；普通 HTTP 开发服务可见于端点/网关处理链，敏感业务需单独启用应用层 TLS，不能宣称中间主机完全不可见。

### 9.3 联调契约与证据

每次联调任务最少包含：

```json
{
  "integration_id": "int_uuid",
  "frontend_agent_id": "frontend_agent_uuid",
  "backend_agent_id": "backend_agent_uuid",
  "api_contract": {
    "type": "openapi",
    "artifact_id": "artifact_uuid",
    "sha256": "hex-digest",
    "revision": 3
  },
  "service_id": "svc_uuid",
  "test_plan": [
    {"test_id": "health", "method": "GET", "path": "/health", "expected_status": 200}
  ],
  "max_fix_cycles": 2
}
```

联调闭环：

1. 用户授权创建 `IntegrationRun`，指定前后端 agent/project。
2. 后端输出 OpenAPI（若变更，必须附新的 revision/hash）。
3. 本机 owner 批准后端启动服务并发布限定的 Service lease。
4. 前端请求连接并收到本机环回地址（Agent 无需知道校内真实 IP）。
5. 前端测试，生成可重放的请求摘要、断言结果、stdout/stderr 片段、相关文件 hash。
6. 失败时把**结构化测试报告**发送给后端，不把全部日志无差别塞进提示词。
7. 后端修改并回报证据，前端重新运行同一测试契约。
8. 满足断言后标记 `verified`；到达 `max_fix_cycles` 仍失败则退出并交由用户处理。

验收**不能只依赖模型自评成功**：至少 HTTP 合约测试由确定性程序执行。敏感响应和 bearer token 必须脱敏后入库。

---

## 10. SQLite 数据模型与持久化

### 10.1 节点数据库（建议 migrations）

```sql
CREATE TABLE nodes (
  node_id TEXT PRIMARY KEY,
  display_name TEXT NOT NULL,
  public_key_fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE trusted_peers (
  peer_node_id TEXT PRIMARY KEY,
  public_key_fingerprint TEXT NOT NULL,
  status TEXT NOT NULL,
  approved_at TEXT NOT NULL,
  revoked_at TEXT
);

CREATE TABLE agents (
  agent_id TEXT PRIMARY KEY,
  node_id TEXT NOT NULL,
  harness_kind TEXT NOT NULL,
  name TEXT NOT NULL,
  project_id TEXT NOT NULL,
  availability TEXT NOT NULL,
  config_json TEXT NOT NULL
);

CREATE TABLE tasks (
  task_id TEXT PRIMARY KEY,
  origin_node_id TEXT NOT NULL,
  target_node_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_attempt_id TEXT
);

CREATE TABLE task_attempts (
  attempt_id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(task_id),
  runtime_pid INTEGER,
  status TEXT NOT NULL,
  started_at TEXT,
  ended_at TEXT,
  recovery_reason TEXT
);

CREATE TABLE task_events (
  task_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  attempt_id TEXT,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (task_id, seq)
);

CREATE TABLE request_dedup (
  sender_node_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (sender_node_id, request_id)
);

CREATE TABLE audit_records (
  id TEXT PRIMARY KEY,
  actor_node_id TEXT,
  action TEXT NOT NULL,
  target TEXT NOT NULL,
  decision TEXT NOT NULL,
  trace_id TEXT NOT NULL,
  created_at TEXT NOT NULL
);
```

V0.2 迁移增加 `services`, `service_leases`, `artifacts`, `integration_runs`, `integration_test_results`, `gateway_routes`。V2 Hub 单独数据库，**不能直接共享/挂载某个客户端的 SQLite 文件**。

### 10.2 一致性要求

- V0 多实例的 SQLite、身份、信任表、日志和 Harness session-dir 必须各自位于选中 `data_dir` 内；测试项目根目录另行登记且彼此分离，不能让两个 Node 共用同一个数据库。
- SQLite 开 WAL、忙超时；事务内原子完成“入站去重 + 建任务 + 初始事件”。
- 每次任务状态更新与事件追加必须是同一事务。
- 工作目录/文件不可用时不能删除原历史；向用户显示缺失资源。
- 普通事件保留 30 天可配置；审计记录单独保留/导出。清理不能破坏未完任务的重连能力。
- 持久化数据结构应做版本化迁移，不在启动时丢弃未知旧版本表。
- V2 的消息去重采用 origin+request_id 或 origin+message_id，全链路不以 Hub 投递次数决定实际执行次数。

---

## 11. 错误、可观察性与运行安全

### 11.1 状态指标

本地 `hmesh doctor`/`hmesh status --json` 输出：

- 当前实例、有效数据目录、网络模式与监听地址；本机回环模式将 Tailscale 检查标记为 `not_required`；
- OS/arch、Daemon 运行版本、协议版本；
- Tailscale 是否运行、是否登录、Tailnet 可见性；
- 设备绑定端口、ACL 失败、证书有效性；
- Pi/DSH/OpenCode（V0 P0）与 Codex（V0.1）可执行文件版本、协议探测、认证与真实运行验证结果（不输出密钥）；
- SSH Gateway 的目标 Host Key 校验/转发功能是否可用；
- 当前受控 Run 数、等待批准数量、Service lease 数；
- 连接状态：`direct` / `relay` / `unknown`，但**不能**把单纯的 Tailscale Ping 结果当成应用 RPC 已经可达。

### 11.2 日志与追踪

- Rust `tracing` + JSON 日志；字段 `node_id`, `peer_id`, `task_id`, `attempt_id`, `trace_id`, `lease_id`。
- 默认日志级别 `info`；红线：**禁止写入** API key、SSH 私钥、环境变量全文、原始浏览器 Cookie、未经脱敏的请求 body。
- stdout/stderr 可包含敏感内容：需限制收集大小、提供存储开关、查看权限、过期清理。
- 支持 `hmesh debug bundle --redact` 生成脱敏诊断包，用于 issue 复现。

### 11.3 资源限制

默认值（用户可配置）：

```toml
[limits]
max_concurrent_runs = 1
max_queue_depth = 20
max_task_runtime_seconds = 1800
max_event_payload_bytes = 65536
max_json_body_bytes = 1048576
max_artifact_bytes = 10485760
max_service_tunnels = 2
max_service_lease_seconds = 3600
```

对大输出采用分页/分块/摘要；慢 peer 不能阻塞整个 Daemon；Daemon 需要对请求、事件订阅和代理连接进行背压控制。

---

## 12. 具体版本实施计划（可直接转为 GitHub Issues）

### 12.0 V0 — Local Loopback

**目标：先在当前电脑上，通过两个独立 Node，按 Pi → DeepSeek Harness（DSH）→ OpenCode 的 P0 顺序验证真实任务闭环。**

以下 L01-L05 是现有 W01、W03-W08 的本机基础切片，直接构成 V0.1 的实现基础，不另建临时 Runtime。W02 的 Tailscale 接入和 W09 的跨设备验收留到 V0.1。

#### L01 — Workspace 与多实例启动

- [x] 建立 Rust workspace、core/protocol 类型、配置校验和 SQLite migration。
- [x] 实现全局 `--data-dir`、幂等 `init`、`doctor` 和前台 `daemon start/status`。
- [x] 在两个终端启动独立 Node，显式配置回环端口 `38880` / `38881`；身份、数据库、日志、Harness session-dir 和本机 API 凭据均分离。
- **验收**：两个进程能同时运行，CLI 始终连接所选实例；端口冲突、误用相同数据目录时明确失败，不串用状态。执行当前 OS 的 fmt/clippy/test，并建立三平台编译 CI；真实 Harness 运行证据在 V0 只要求当前 OS。

#### L02 — 回环认证、配对与项目授权

- [x] 复用节点密钥、TLS 1.3/mTLS、指纹比对、配对审批与撤销；同机来源不能绕过认证。
- [x] 实现本机 owner 的最小项目登记和 `task.submit` 审批/明确预批准流程；任务提交授权与 Harness 工具审批分开。
- [x] 使用真实 loopback socket 测试错误指纹、未配对访问、未登记项目和授权拒绝。
- **验收**：未授权请求不创建执行尝试，不启动 Harness；两个 Node 的项目授权与信任状态互不串用。

#### L03 — Pi / DSH / OpenCode 的真实 Managed Run（P0）

- [x] 以第 7.8 节核查记录为基线重新探测 Pi/DSH/OpenCode 的版本、凭据可用性与事件/取消能力；核查时的 help/无模型 RPC 不抵扣真实任务。
- [x] 先接入 Pi RPC，再实现 DSH headless JSON 与 OpenCode run JSON Adapter；三者共用 HarnessAdapter/RunEvent。Codex 不作为本工作包前置。
- [x] 为 Node B 登记独立测试项目，在该项目中执行确定性的小任务，例如生成 `out/health.json` 并运行测试。
- [x] 分别验证三种真实 Harness 子进程的事件、退出结果与工件摘要；隔离配置/会话/数据，启用本机授权的凭据与工具限制。
- [x] 契约 fixture 覆盖 Pi 命令接受不等于任务完成、DSH final 但退出失败、OpenCode 错误事件/非交互审批、凭据缺失、进程异常和无法确认取消。
- **验收**：先交付 Pi 单端真实检查点，再补齐 DSH/OpenCode 的真实 fixture 任务；受控 runner 检查预期文件、内容、SHA-256 和测试断言。仅第一种通过时报告该检查点完成，不宣称全部 V0 P0 完成。

#### L04 — 本机双节点 Task 闭环

- [x] 实现 `peers list`、Agent 注册/列表、`task send/show/watch/cancel` 的最小 CLI 集合。
- [x] Node A 经 HTTPS 向 Node B 提交任务，B 审批后执行，A 读取任务状态和持久事件。
- [x] A 登记 Pi、B 登记 DSH，分别由两端 CLI 发起 A→B 与 B→A；再将一个执行端切换为 OpenCode 运行相同契约，不要求三者所有两两组合。
- [x] 原子完成入站去重、Task/Attempt 建立及事件记录；同 request_id 重发只返回已有任务。
- [x] 覆盖明确成功/失败、审批拒绝、取消确认、事件重连读取和重启后的任务查询；无法确认进程结果时标 `UNKNOWN`，不得自动重跑。
- **验收**：第 16.0 节 E2E-00 通过；项目 A 不出现误写工件；取消不支持或无法确认时如实返回能力/状态，不能伪报 `CANCELLED`。

#### L05 — 可重复的本机验收与文档

- [x] 提供 `examples/single-machine-task/`、`docs/quickstart-v0.md` 和当前 OS 可运行的 smoke 脚本；当前 Windows 环境优先 `scripts/e2e-local.ps1`。
- [x] 每次验收使用全新临时配置与项目，不读取开发者个人数据；真实 Harness smoke 单独显式 opt-in，仅使用用户为该测试授权的本机凭据。
- [x] 清理只处理该次测试启动的受控进程/临时资源；留下版本、命令、事件、文件 hash、断言及授权负例证据。
- **发布门禁**：L01-L05 和 E2E-00 通过，Pi/DSH/OpenCode 三种 P0 的真实任务与 Pi/DSH 双向委派证据齐全，无第二台电脑、Tailscale 或 Codex 前置依赖。未验证/被凭据阻塞的 P0 明确列出，不冒充完成；跨 OS 验收留到 V0.1。

### 12.1 V0.1 — Peer & Task

**目标：Mac ↔ Windows 在无 ECS、同 Tailnet 下，两个真实 Harness 有控制地互相下发并执行任务。**

复用 V0 已完成的 Workspace、协议、身份、授权、Task Engine、CLI 及 Pi/DSH/OpenCode Adapter。下面 W01-W09 保留原工作包编号，按现有实现补齐 Tailscale、跨设备、Codex、MCP 与跨 OS 验收，不重复另写本机基础。

#### W01 — Rust Workspace 与 CI 基础

- [ ] 创建 Cargo workspace，锁定稳定工具链，`cargo fmt`, `clippy -D warnings`, `cargo test`。
- [ ] macOS、Windows、Linux 交叉平台 CI build；SQLite migration、config fixtures。
- [ ] 发布最小 `hmesh init`, `hmesh doctor`, `hmesh daemon start`。
- **验收**：干净开发机运行 `hmesh init` 生成身份文件，重复运行幂等；Windows/macOS/Linux CI 绿。

#### W02 — Tailscale Adapter

- [ ] `tailscale status --json` 获取 peers + IP + DNS name；可替换的 `TailscaleProvider` trait。
- [ ] fixture 覆盖离线、未登录、peer 缺字段、IPv6、CLI 不存在、JSON 格式变化。
- [ ] `hmesh peers candidates` 仅展示候选；支持显式配置地址。
- **验收**：在两个不同网络的设备中发现彼此的候选地址，无公网端口映射。

#### W03 — Node 身份与配对

- [ ] 生成私钥、保存信任表；实现双向 fingerprint 对照与审批。
- [ ] mTLS TLS1.3 连接，未认证访问最小 hello；拒绝错误指纹和撤销节点。
- [ ] 被拒绝/过期配对不能创建任何任务。
- **验收**：第三台未配对同 Tailnet 设备拿不到 Agent 列表，也不能调用 Task API。

#### W04 — Node/Agent Registry

- [ ] 本地 Agent 配置校验、`probe()`、Register / Unregister、在线状态查询。
- [ ] Remote Agent cards 缓存/过期策略；不展示未获授权的项目路径。
- **验收**：Windows Pi 可被 Mac 看到为 `available`，未配置凭据时显示清晰的 `unavailable_reason`。

#### W05 — Task Engine / SQLite

- [ ] 任务 CRUD、状态转移、attempt、审计；入站幂等事务。
- [ ] 任务事件 SSE 或轮询 + seq replay；支持断连恢复读取。
- [ ] 有限队列、超时、取消并记录 UNKNOWN 情形。
- **验收**：重复 POST 同 request_id 只创建一个执行尝试；任务状态重启后仍可查。

#### W06 — Pi Adapter 真执行

- [ ] Pi RPC 探测、子进程管理、JSONL 解析、事件收集、取消。
- [ ] 指定测试项目目录并运行确定性 fixture task。
- **验收**：真实 Pi 在 Windows 生成预期文件/结果，返回可验证 hash。

#### W07 — Codex Adapter 真执行

- [ ] Codex exec/SDK 探测、管理式执行、事件映射、超时取消。
- [ ] 测试 Mac 上 Codex 独立任务，另测试从 Windows Pi 发起对 Mac Codex 的远端任务。
- **验收**：两个方向跨机调用均成功；拒绝“直接接管用户现有交互 session”的错误假设。

#### W08 — CLI + MCP 最小集

- [ ] `peers`, `agents`, `task send/show/watch/cancel`。
- [x] 本地 stdio MCP Server 工具与 schema，支持一条从 Harness tool 发起的远端任务（Pi 1.4.0 → DSH，SSH 检查点）
- **验收**：Codex/Pi 中调用 `mesh_submit_task` 可触发经批准的远端真实 Run。

#### W09 — V0.1 E2E 与文档

- [ ] Mac↔Windows 不同 NAT；成功、拒绝、超时、网络断开、重复投递、证书错误。
- [ ] docs/quickstart-v01.md、docs/security.md、docs/protocol.md、examples/two-machine。
- **发布门禁**：W01-W09 所有 P0 完成；不能依赖 ECS；不能把 Mock 当 E2E 证据。

### 12.2 V0.2 — Service Sharing & SSH Gateway

#### W10 — Service Registry / Lease

- [x] `service publish/list/connect/revoke`，仅允许具体项目、具体 port（当前为有界 HTTP 检查点，实际参数见 docs/managed-services.md）。
- [ ] TCP proxy（Mac 本机 loopback → peer）和 lease 到期/断开清理。
- **验收**：A 通过 `127.0.0.1:18000` 调到 B 的 `127.0.0.1:8000`；未经批准节点失败，撤销后连接断开。

#### W11 — SSH Gateway

- [ ] Windows 上 `gateway add`，SSH host key strict、用户名与 SSH config、转发白名单。
- [x] Linux `hmesh-daemon` 通过 Windows `ssh -L` 隧道可达，逻辑节点有独立身份；当前由 owner 的 `tunnel add/list/restart/revoke` 管理。
- [ ] Gateway 的 node route 记录、健康检查、转发重建和回收。
- **验收**：Linux 完全不装 Tailscale，Mac 仍能通过 Windows Gateway 向 Linux Pi 派任务。

#### W12 — 跨机 API 连接

- [ ] Linux 服务发布后，Mac 能经过两跳建立临时本地 endpoint。
- [ ] 禁止端口扫描、通用 SOCKS、向任意内网 IP 开洞。
- [x] HTTP health/test contracts 与代理连接证据（Windows ↔ Orion 两机检查点；三机验收保持未完成）。
- **验收**：Mac 访问 Linux FastAPI，Windows Gateway 不开放公网端口，不配置校园网段路由。

#### W13 — IntegrationRun / Artifact

- [ ] OpenAPI 文件及 sha256 版本、测试计划、失败报告与验收结果。
- [x] 1–2 次受控修复循环，超限需人工接管；未知结果停止、不换 UUID 自动重跑，当前测试契约为固定 GET/JSON 断言。
- [ ] 限流分块 artifact 获取，防敏感信息泄漏。
- **验收**：固定缺字段测试可复现失败→后端修复→前端重测成功，记录全部工件和依据。

#### W14 — V0.2 文档与 Demo

- [ ] `examples/frontend-backend`：Linux FastAPI + Mac Vite/React 或简单 HTTP test harness。
- [ ] `docs/windows-ssh-gateway.md` 与学校网络合规说明。
- [ ] 双路由测试：direct-tailnet、via-ssh-gateway。
- **发布门禁**：W10-W14 通过，自动建立/回收的实际连接有可重放测试证据。

### 12.3 V0.3 — 安全与可恢复 Beta

#### W15 — Policy / Approvals

- [ ] 项目级、peer 级授权、显式审批/预批准、审批 UI 可先使用 CLI。
- [ ] 默认 deny；配置迁移与拒绝规则测试；危险调用不得越过本机审批。

#### W16 — 可靠性

- [ ] 网络故障重试/去重、Event Resume、进程重启状态重建。
- [ ] Windows 更新或 Linux 重启后的 UNKNOWN 处理，幂等恢复测试。
- [ ] 恢复不等于执行；未经核实不自动重复副作用任务。

#### W17 — 平台打包

- [ ] Linux systemd user service；Windows 用户服务或 Task Scheduler 安装策略；macOS LaunchAgent。
- [ ] `install/uninstall/upgrade` 保护数据与密钥；禁用提升权限默认安装。
- [ ] Windows SSH 客户端路径及 `ssh.exe` 差异覆盖。

#### W18 — 安全测试与 Benchmark

- [ ] 未配对、错误密钥、过期 token、跨项目权限、SSRF、请求泛洪、日志密钥泄漏测试。
- [ ] 断网/延迟/重复消息/任务超时/恶意大响应故障注入。
- [ ] 记录 CPU、RSS、任务启动延迟、事件丢失数、代理吞吐与条件。

### 12.4 V1 — 独立开源正式版

#### W19 — API 与协议冻结

- [ ] `hmesh/1`、JSON/MCP 工具 schema 发布兼容策略，协议负向测试。
- [ ] 日志、审计导出稳定；CLI 输出模式稳定；引入 protocol conformance fixtures。

#### W20 — 兼容矩阵

- [ ] Mac、Windows、Linux 安装与 CI；Tailscale 支持版本矩阵；Pi/Codex Adapter 真实运行测试。
- [ ] 各 Harness 的 capability flags 和不支持声明清晰；Pi/DSH/OpenCode 保留 V0 本机回归，Pi/Codex 具备 V0.1 跨设备证据；DSH/OpenCode 的跨 OS 支持范围按实际测量声明。

#### W21 — 社区与体验

- [ ] 快速入门、二机任务演示、Mac→Windows→Linux 联调演示、故障排查、配置范例。
- [ ] 发行二进制、SBOM、SHA256、代码签名（平台能力允许时），Release Notes、LICENSE。
- [ ] `hmesh doctor` 具有明确诊断修复建议；可选只读 `hmesh top` CLI 拓扑。

#### V1 发布标准

1. 不接 ECS，完成核心两机 Task 和三机跨网联调。
2. 两种不同 Harness 的 Managed Run 可运行，并有稳定 CI/smoke 证据。
3. 默认不暴露敏感数据或服务，授权撤销即时生效。
4. 断网恢复不会发生未经批准的第二次副作用执行。
5. README 能让新用户在两台已加入 Tailnet 的设备上复现 demo。

### 12.5 V2 — ECS Optional Hub

#### W22 — Hub 最小控制面

- [ ] 独立 `hmesh-hub` 二进制，Rust Web Server + SQLite。
- [ ] 节点 outbound HTTPS/WSS 注册、心跳与目录更新；Hub 校验设备签名/证书、用户 tenant 边界。
- [ ] 同一节点多连接会话防抢占；状态 TTL，失联清理。
- **验收**：新节点经 Hub 看到经授权的 Agent 目录，数据直连仍走 Tailnet。

#### W23 — Offline Mailbox

- [ ] 短消息、任务提交意图和 ACK 存储（有界队列/TTL/幂等键）。
- [ ] 节点重新上线后拉取、ACK、去重；不能执行未授权的离线任务。
- [ ] Hub 数据只保存最小字段、加密封装的 payload（采用绑定 Node 身份的专用加密密钥，Hub 默认不持有消息正文解密密钥）。
- **验收**：B 离线时 A 可保存待投递请求，B 恢复后取得请求并经本地审批，仅执行一次入站尝试。

#### W24 — Web Console（最小）

- [ ] 登录、自己的节点在线/离线、已授权 Agent 列表、任务状态摘要、离线邮件状态、撤销设备。
- [ ] Web Console 不存 Model API Key、SSH private key，不提供默认远程 Shell。
- [ ] CSRF/会话管理/操作审计，初版单 owner 模式，不能冒充组织级 RBAC。

#### W25 — Hub 接入和降级

- [ ] `mode = local | hybrid`；Hub 断开不影响已配对的直连任务/已建 Service。
- [ ] 恢复时 merge 规则：Task/Attempt 以 owner node 的本地状态为准，Hub 保存最终已确认状态摘要，不以 last-write-wins 覆盖受控执行状态。
- [ ] 从 V1 无损升级；Hub 可删除重建，节点身份与本地历史不丢失。

#### W26 — ECS 2C2G 部署

- [ ] Docker Compose + 持久卷 + Caddy HTTPS，限制容器内存与日志大小。
- [ ] `/healthz`, `/readyz`, `/metrics`（敏感指标限制访问）。
- [ ] 每日加密备份 SQLite、记录恢复流程和演练。
- [ ] 限流、连接上限、数据库大小、保留期、入站防火墙。
- [ ] README 提供 `ecs/compose.yaml`、`.env.example`、部署验证及回滚。
- **验收**：2 vCPU/2 GiB ECS 持续运行轻量目录/离线队列，不要求在 ECS 执行 Harness、不代理所有服务流量。

### 12.6 不进入 V2 主线的功能

- 完整自研 WireGuard/P2P/NAT 穿透；
- 全量 A2A 1.0 Server 与组织多租户；
- 跨设备共享/挂载整个工作目录、自动 Git merge 冲突解决；
- 跨机器沙箱资源编排、GPU 调度；
- 默认把远端所有 SSH 和数据库端口暴露给 Mesh；
- 支持所有闭源 Harness 的无接口注入；
- 基于模型的无上限自我修复循环。

---

## 13. V2 Hub 详细设计

### 13.1 API

```text
POST   /hub/v1/nodes/register           # 带证明的注册/刷新
POST   /hub/v1/nodes/heartbeat          # 心跳（状态 TTL）
GET    /hub/v1/nodes                    # 授权过滤后的设备目录
GET    /hub/v1/agents                   # 授权过滤后的 Agent 目录
POST   /hub/v1/mailbox                  # 提交端到端加密的待投递消息
GET    /hub/v1/mailbox?cursor=...       # 拉取收件箱
POST   /hub/v1/mailbox/{id}/ack         # 收件方 ACK
POST   /hub/v1/nodes/{id}/revoke        # Owner 撤销
GET    /healthz
GET    /readyz
```

所有响应使用游标分页及授权过滤，不允许用户通过改变 `node_id` 枚举其他 owner 的设备。Hub API 不可简单沿用 V1 的 peer mTLS 信任语义：必须引入账号 owner/tenant 归属与设备注册关系，并验证设备所属 owner。

### 13.2 云端同步语义

- `origin_node` 是任务的发起者；`execution_node` 是任务执行状态事实源。Hub 只持有用于分发/展示的副本与摘要。
- 离线消息是**待投递请求**，不是“已经批准要运行”；接收端上线后仍要执行本地 ACL/approval。
- 默认不上传 prompt 原文、工具日志、源代码和文件；如用户选择共享特定任务输入，使用**接收端已验证的 X25519/HPKE 加密公钥**封装内容密钥进行端到端加密，Hub 只见密文、元数据及限额。加密公钥必须与已认证 node identity 绑定并支持轮换；不能直接用 Ed25519 签名公钥加密。
- Hub 离线 24h 仍能经 Tailnet 完成已经配对的 Peer-to-Peer 任务。
- 禁止 Hub 直接修改 Daemon 本地可信设备表；远端吊销消息要验证并显式处理冲突。

### 13.3 ECS 部署建议（面向 2 vCPU/2 GiB）

- Linux + Docker Compose；Hub 进程自身限额例如 `512 MiB` 起（依据实测调整），SQLite WAL + 单实例写入。
- 常驻服务包括 Caddy、Hub、少量监控；不要同时放大型 Postgres/RAG/浏览器、模型推理、代理所有桌面流量。
- HTTPS 443 提供 API，SSH 管理端口仅管理 IP，Docker 服务不直接暴露 SQLite/内部端口。
- 默认不以 ECS 流量转发所有隧道；如未来增加流量 Relay，须另做带宽预算、租约、滥用防护与安全评估。
- ECS 路由和服务配额由真实压测决定；2C2G 是“小规模协调节点”起点，不是规模性保证。

---

## 14. 开源仓库结构

```text
harnessmesh/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── README.md
├── README.zh-CN.md
├── LICENSE
├── crates/
│   ├── hmesh-core/              # ID / State / Events / Contracts
│   ├── hmesh-protocol/          # Serde types / validation / version
│   ├── hmesh-store/             # SQLite migrations / repository
│   ├── hmesh-security/          # Keypair / trust / mTLS / policy
│   ├── hmesh-transport/         # Transport traits + tailnet RPC
│   ├── hmesh-transport-tailscale/
│   ├── hmesh-transport-ssh/     # V0.2 gateway
│   ├── hmesh-runtime/           # Task engine / leases / orchestration
│   ├── hmesh-adapter-pi/
│   ├── hmesh-adapter-dsh/        # V0 P0：headless JSON；SDK 后续增强
│   ├── hmesh-adapter-opencode/   # V0 P0：run JSON
│   ├── hmesh-adapter-codex/
│   ├── hmesh-daemon/
│   ├── hmesh-cli/
│   ├── hmesh-mcp/
│   └── hmesh-hub/               # V2，不能让 V1 依赖此 crate
├── configs/
│   ├── node.example.toml
│   ├── agent.example.toml
│   ├── service.example.toml
│   └── gateway.example.toml
├── examples/
│   ├── single-machine-task/      # V0：两个回环 Node、独立测试项目
│   ├── two-machine-task/
│   └── frontend-backend-gateway/
├── tests/
│   ├── contract/
│   ├── integration/
│   ├── fault-injection/
│   └── fixtures/
├── scripts/
│   ├── e2e-local.ps1             # V0：Windows 本机显式 opt-in smoke
│   ├── e2e-smoke.sh
│   └── release-check.sh
├── deploy/
│   └── ecs/                     # V2 compose/caddy/backup
├── docs/
│   ├── quickstart-v0.md
│   ├── architecture.md
│   ├── protocol.md
│   ├── threat-model.md
│   ├── security.md
│   ├── adapters.md
│   ├── windows-ssh-gateway.md
│   ├── compatibility.md
│   ├── testing.md
│   └── adr/                     # Architecture Decision Records
└── .github/workflows/
    ├── ci.yml
    ├── cross-platform.yml
    └── release.yml
```

不要为了“微服务化”将单机 Daemon 拆成多个独立常驻服务；MVP 尽量单二进制 + SQLite + 可单独启用的 MCP stdio 进程。

---

## 15. 配置示例（拟定，最终以 JSON Schema/serde 校验实现）

### 15.1 `node.toml`

`--data-dir` 是当前实例的数据目录选择入口，优先于配置中的 `node.data_dir`；未指定时使用默认目录。相对的 `security.policy_file` 与 Harness session-dir 相对于有效数据目录解析，`init` 为各实例生成各自配置与本机 API 凭据。显式配置的项目根目录独立校验，不得因切换实例而继承另一个实例的项目授权。

`init` 默认生成 `network = "loopback"` 配置。下面完整示例用于 V0.1 的 Tailscale 模式；V0 使用后面的双实例片段，不能因复制该示例而引入 Tailscale 前置依赖。

```toml
[node]
name = "macbook-dev"
data_dir = "~/.harnessmesh"
listen_address = "127.0.0.1"
listen_port = 38880
network = "tailscale"
expose_to_tailnet = false  # 默认关；需显式开启

[discovery]
mode = "approved-peers"
refresh_seconds = 15

[security]
require_mutual_auth = true
policy_file = "policy.toml"
default_action = "deny"

[runtime]
max_concurrent_runs = 1
max_queue_depth = 20

[hub]
enabled = false  # V2 opt-in
```

**注意**：TOML 的 `~` 在 Rust 中不会自动扩展；配置加载层必须显式展开用户目录或要求绝对路径。不能把未展开的字符串当作路径。

#### V0 双实例配置

两个实例分别在终端使用 `hmesh --data-dir ./.hmesh-v0/node-a ...` 与 `hmesh --data-dir ./.hmesh-v0/node-b ...`；`init` 后分别修改各自 `node.toml`，再执行 `daemon start`。以下为 Node A 的配置片段，Node B 使用独立配置，将名称改为 `local-b`、端口改为 `38881`：

```toml
[node]
name = "local-a"
listen_address = "127.0.0.1"
listen_port = 38880
network = "loopback"
expose_to_tailnet = false

[security]
require_mutual_auth = true
policy_file = "policy.toml"
default_action = "deny"
```

分别登记项目 A/B，使用不同的项目根目录；先以 Pi 完成一个执行端，随后 A 登记 Pi、B 登记 DSH，再切换一个执行端为 OpenCode 验证同一任务契约。配对时显式使用对方的 `127.0.0.1:<port>` 与预期证书指纹，不能跳过审批。完整命令需由 V0 实现后的 quickstart 与 smoke 验证，本节不代表 CLI 已存在。

### 15.2 `agent.toml`

V0 P0 的 `agent.harness` 分别为 `pi`、`dsh`、`opencode`，对应 `adapter.executable` 为本机核查后的实际启动入口。三个 Adapter 各自构造已验证的参数、配置/数据隔离及权限限制；不能把下例 Pi 的参数或环境直接套到其他 Harness，也不能接受远端指定可执行文件。Windows 须记录 `.cmd` shim 与真实 Node 入口/原生 exe 的解析结果，验证含空格路径、stdin/stdout 和取消，不以 shell 字符串拼接目标或任务正文。

```toml
[agent]
name = "backend-pi"
harness = "pi"
mode = "managed"
project_id = "demo-backend"
display_capabilities = ["python", "fastapi", "tests"]

[project]
root = "/home/developer/projects/demo-backend"
allowed_tools = ["read", "write", "test"]
require_remote_task_approval = true

[adapter]
executable = "pi"
# 具体 CLI 参数由 adapter 版本探测决定，不允许远端自行指定
```

### 15.3 `policy.toml`

```toml
[default]
action = "deny"

[[grant]]
peer = "paired-mac-node-id"
project = "demo-backend"
actions = ["agent.read", "task.submit"]
require_approval = true

[[grant]]
peer = "paired-mac-node-id"
project = "demo-backend"
actions = ["service.connect"]
service = "backend-api"
max_lease_seconds = 1800
require_approval = true
```

对 `service.publish` 的授予应该在服务拥有者本机操作，不能因为远端有 `service.connect` 就能要求任意开放服务。

---

## 16. 端到端验收方案

### 16.0 E2E-00 本机双节点任务（V0 P0）

**前置**：当前电脑已安装 Pi/DSH/OpenCode（版本与接口基线见第 7.8 节），真实运行仍需分别完成凭据与模型探测；测试使用临时实例目录 A/B、分离的项目目录 A/B 和明确授权的测试凭据。先用 Pi 完成首个检查点，再扩展其余两种 P0；无需 Tailscale 或 Codex。真实 Harness smoke 必须显式 opt-in，不能把该前置用于普通自动化测试。

```text
1. 分别 init Node A / B，启动两个 Daemon 进程，仅监听 127.0.0.1:38880 / :38881
2. 检查 node_id、指纹、数据库、本机 API 凭据和 session-dir 各自独立
3. 按实际指纹完成 A/B 配对；B 登记项目 B 和真实 Harness Agent
4. A peers/agents list → B 的 Agent 为 available，未授权项目路径不泄漏
5. A task send → B 本机审批 → Harness 真实启动并只以项目 B 为配置的工作目录
6. B 在项目 B 产生 out/health.json 和测试结果；A watch → succeeded + seq 事件
7. 受控 runner 核验内容、测试断言、SHA-256 和项目 A 未被误写
8. 原请求以相同 sender_node_id/request_id 重发 → 同 task_id，不新增执行尝试
9. 未配对客户端、错误指纹、未授权项目、审批拒绝 → 拒绝且不启动 Harness
10. 断开/恢复事件订阅后可补读；重启 Daemon 后任务与审计记录仍可查询
11. 受控长任务取消：确认停止才写 CANCELLED；无法确认时报告 UNKNOWN
12. 杀死测试 Daemon/Adapter 后恢复：未证实的运行结果为 UNKNOWN，无自动重跑
13. A/Pi 与 B/DSH 分别作为执行端，运行 A→B、B→A；可由各自 CLI 发起
14. 将一个执行端换成 OpenCode，重复真实任务、事件、工件和去重核验
```

**首个检查点**：Pi 单执行端完成步骤 1-12。**V0 P0 Pass**：Pi/DSH/OpenCode 三种真实 Harness 分别通过任务闭环，并完成步骤 13-14；上述授权负例与持久化/去重检查通过。Runner 按 Harness 保存当前 OS、包名/版本、实际入口、provider/model 标识、节点与任务 ID、事件及断言证据，凭据不得入库；fixture/Mock 测试单独标注。若 Harness 不支持确认取消，须有明确能力报告及受控测试进程验证取消路径，不能把 unsupported 当作真实取消成功。

**进入 V0.1 前**：上述证据只证明同机协议与执行链路；Tailnet 发现、跨机认证、不同 OS 进程管理与 Codex 接入仍需 E2E-01 及 W06/W07 实测，不能由 E2E-00 抵扣。

### 16.1 E2E-01 两机任务（V0.1 P0）

**前置**：V0 E2E-00 已通过；Mac/Windows 同一 Tailnet，互可达，正确配对；Mac Codex、Windows Pi 实测可运行。

```text
1. Windows hmesh daemon start
2. Mac hmesh daemon start
3. 双向证书指纹比对、发起/批准配对
4. Windows register pi-backend (project=test-fixture)
5. Mac peers/agents list → pi-backend available
6. Mac task send → Windows 授权 → Pi 实际运行
7. Windows 输出 ./out/health.json
8. Mac watch task → succeeded + sha256 + event history
9. 同 request_id 重发：不能再启动一次 Pi
10. 未信任第三台设备调用：403/握手失败
```

**Pass**：项目 artifact 真实存在、hash 与受控 runner 计算一致、任务事件完整，且取消/审批负例通过。

### 16.2 E2E-02 Linux SSH Gateway（V0.2 P0）

**前置**：Mac↔Windows Tailscale、Windows→Linux SSH 已获批准且 known_hosts 已固定；Linux daemon 仅回环监听。

```text
1. Windows 登记 Linux SSH Gateway（明确的主机/端口白名单）
2. Windows 经 SSH tunnel 将 Linux daemon 登记为逻辑 node
3. Mac agents list → linux/pi-backend (via windows)
4. Mac task send → Windows 代理 → Linux Pi 真执行
5. Linux 启动 127.0.0.1:8000 FastAPI 并登记 Service
6. Mac service connect → 本地 127.0.0.1:18000
7. curl http://127.0.0.1:18000/health → HTTP 200
8. 撤销/租约过期：curl 连接拒绝
9. 断开 Windows：Mac 看见 via gateway unreachable，无错误直连宣称
```

**Pass**：Linux 不安装 Tailscale、不开放公网端口、不新增校园子网路由；任务与服务会话均有可审计的授权链。

### 16.3 E2E-03 前后端联调（V0.2 P0）

- 固定 fixture 故意令后端响应缺 `request_id`。
- 前端测试报告具体断言失败，不宣称完成。
- 后端修改后产生新的 artifact hash / API revision；前端按同一契约复测通过。
- IntegrationRun 的 `verification_status=passed` 必须由确定性断言生成，不能仅由模型回答生成。
- 断开服务后重连不能继续使用已撤销的 lease。

### 16.4 E2E-04 网络故障与安全（V0.3 P0）

- P2P→DERP 路由变化，正在执行的本机任务不因单次网络波动自动重启。
- 强制杀死远端 Daemon/Adapter 进程，重启后的 Task/Attempt 状态保守恢复为可证状态或 UNKNOWN。
- 请求重放/签名错误/冒用 node_id/权限升高/越权访问 project 受到拒绝。
- 拦截 service.connect，尝试目标 `169.254.169.254:80`、本机非许可端口、未列入的校园主机，必须拒绝。
- 通过故障注入确认能回收所有代理监听和子进程，资源无明显泄漏。

### 16.5 E2E-05 ECS Hub（V2 P0）

- 两节点已建立 V1 连接、随后启用 ECS Hub。
- A 离线提交待投递请求到 Hub；B 上线拉取并本地审核；重复 ACK/重复投递只登记一个任务。
- Hub 关机后，已配对、Tailnet 可达的 A/B 仍能在线执行任务。
- Hub 不存放 model API key、SSH 私钥、用户源码；默认不包含 prompt 正文。
- 2C2G ECS 压测记录至少包含 RSS、CPU、连接数、入队/出队延迟、SQLite 大小；不得只报告“能启动”。

---

## 17. 测试矩阵与工程质量门禁

| 类别 | 内容 | 触发 |
|---|---|---|
| Unit | State transitions、serde validation、policy、key parsing、lease TTL | 每 PR |
| Contract | Pi RPC、DSH headless JSON、OpenCode run JSON、Codex event、Tailscale JSON fixtures、HTTP API errors | 每 PR |
| Integration | 两 Daemon loopback/模拟 Tailnet、真实 SQLite migrations | 每 PR |
| Local E2E | 两独立 Daemon + 回环 mTLS；Pi/DSH/OpenCode 真实任务、Pi/DSH 双向委派、实例分离与授权负例 | V0 Release；后续 Runtime/Adapter 变更时显式 opt-in |
| Security | 错证书、权限拒绝、重放、token 过期、端口白名单 | 每 PR |
| Cross Platform | Windows x64、macOS arm64、Linux x64 build/tests | 每 PR |
| Smoke | 真实 Mac↔Windows Pi/Codex（凭据条件具备时） | Release |
| Gateway | Windows→Linux SSH；host key、隧道关闭、连接重建 | V0.2+ Release |
| Network Fault | disconnect/reconnect、消息重复/乱序、超时、重启 | V0.3+ Release |
| Load | 任务队列 100/1000 模拟事件；10–50 peers metadata（目标范围） | V1/V2 Release |

### 17.1 必须满足的具体工程约束

- `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --workspace`。
- `sqlx` 或 `rusqlite` 可选其一；禁止两套并存而无明确原因。
- 普通自动化测试不得连接开发者真实账户、真实校园 SSH 或读取个人配置；必须使用隔离 fixture。
- V0 本机及后续 Release smoke 的真实 Harness/API 调用必须与普通自动化测试分离，显式 opt-in，仅使用用户授权给该测试的本机凭据；不能把 token 预设在公开 CI。
- 第三方依赖需要锁定版本并运行 `cargo audit` 等依赖漏洞检查。
- 不得通过关闭 TLS 验证、跳过 SSH host key、放宽所有权限来“修复”测试。

### 17.2 性能基准

每阶段报告测量结果时应列明机器 CPU/内存/OS、网络模式、消息大小和样本数；V0 标记为 loopback，V0.1 起另记录 Tailscale 直连或中继状态：

- 发现时间：从加入 Tailnet 到已授权 peers 列表状态变化；
- Task admission latency：入站请求到 `QUEUED` 的时间；
- Managed Run start latency：`QUEUED → RUNNING`（不含模型首 token）；
- Event replay gap：重连事件缺口数量；
- Proxy latency / throughput：单端口 HTTP echo、局域网 vs Gateway；
- CPU/RSS：空闲、任务执行、服务代理、重连风暴。

若 SSH Gateway 会成为瓶颈，需在 Release Notes 说明；不要编造“比 Tailscale 更快”的结论。

---

## 18. 安全威胁模型（不可删减的最小版）

### 18.1 保护的资产

- 本机项目文件、SSH 密钥、模型供应商 API key、浏览器 cookies；
- Agent 执行控制权、工作目录、工具批准；
- 跨节点任务输入和输出、测试日志、源代码片段；
- 发布的本地开发服务以及校内服务器访问权限。

### 18.2 威胁与对应防护

| 威胁 | 防护要求 |
|---|---|
| 同 Tailnet 恶意节点发起任务 | 应用层节点配对 + mTLS + 默认拒绝 |
| IP 改变或设备名称冲突 | 稳定 node_id + 公钥指纹，不将 IP 作为身份 |
| 远端请求任意 shell/project | 预定义 project_id + 本机策略 + Harness 原生审批 |
| Prompt Injection（远端日志/Agent 文字） | 远端内容视为不可信数据，工具调用必须由本地策略判断 |
| SSH MITM | 严格 host key 校验，拒绝自动忽略校验 |
| 校园内网被当作公开代理 | 仅单机白名单 + 单端口租约；不提供 SOCKS/子网路由 |
| 端口代理 SSRF | 发布与连接双端白名单；拒绝链接本机元数据/未授权内网地址 |
| 任务重复产生副作用 | 幂等入站、attempt lease、UNKNOWN 人工处理 |
| 断线后失控进程 | 本机进程组跟踪、超时、谨慎清理，不能假设能杀死已脱离的子进程 |
| Hub 数据泄漏 | V2 最小元数据、默认 E2E 加密内容、分级保留 |
| 凭据进入日志或工件 | 敏感字段脱敏、输出限额、查看授权、诊断包擦除 |
| 供应链恶意更新 | lockfile、漏洞审计、Release 校验和、可重现构建 |

### 18.3 三项必须明确的现实限制

1. **Tailscale 已经加密网络，但不能替代本应用的远程执行权限体系**。
2. **Windows Gateway 管理员可能观察其主机上的进程/流量**；即便 Mesh Control 经端到端 TLS，明文 HTTP 开发流量也可能由应用转发实现泄露，敏感场景需应用层 TLS。
3. **Harness 具备命令执行能力就有主机风险**；项目不能仅靠模型提示词宣称文件隔离或安全沙箱。

---

## 19. A2A / MCP 兼容策略

- **MCP**：为本机 Harness 提供 `mesh_*` Tools，MCP 是**Harness 到本机 Mesh Runtime** 的工具接入协议，不应误当跨机器节点通信层。
- **A2A**：用于未来对外互操作的 Agent Card、Task、Artifact、状态对齐参考。V0/V0.1 内部协议暂不声明 A2A conformance；V1 增加**独立的 A2A 兼容映射组件**可作为实验性功能，但只有完整通过指定 A2A 1.0 规范的互操作/安全测试后才可称兼容。
- **Tailscale**：只用于 OS 网络可达、节点候选以及私有连接。MagicDNS 主机名不等于 Agent 身份，Tailnet 可达不代表服务可用。
- **SSH**：只用于经授权网络路径及有限目标的隧道；不承载 Agent 任务状态和权限语义。

不要设计两个互相竞争的 Task State Machine：内部执行状态有一个事实源，A2A 协议的状态通过显式 adapter 映射。

---

## 20. 最终 Demo 与开源展示要求

### Demo 0：`One machine, two nodes`（V0）

在当前电脑的两个终端中，分别启动 Node A/B，展示不同的数据目录、节点身份与回环端口。先展示 Pi 单执行端任务，再以 A/Pi 与 B/DSH 展示两个方向的委派，由两端 CLI 发起即可；最后替换一个执行端为 OpenCode 运行相同任务。展示三者真实进程、生成文件、独立测试断言、任务事件、SHA-256 和重复请求未新增 Run；不要求演示所有 Harness 两两组合。

演示须注明“同机双实例”，不能宣称已经完成跨设备、跨 OS、Tailscale 或服务代理验收。后续 Demo A 必须保留独立真机证据。

### Demo A：`Two machines, two harnesses`（V0.1）

用户在 Mac 执行：

```bash
hmesh agents list
hmesh task send pi-backend --project demo-backend \
  --prompt "编写 /health 接口并运行测试"
hmesh task watch <id>
```

Windows 上 Pi 真正运行、回传结构化状态和测试工件。视频必须展示对端设备与 Harness 真实进程，而不是两个本地 Mock。

### Demo B：`Frontend and backend, across the campus gateway`（V0.2）

Mac Codex + Windows OpenSSH Gateway + Linux Pi/FastAPI。Mac 自动获得本地 API Endpoint，API schema 有误 → 验证失败 → 发送报告 → 后端修复 → 重新验证。整个过程只需用户完成必要授权。

### Demo C：`Offline hub with optional ECS`（V2）

同一套 CLI 不变。开启 ECS 后增加目录/离线请求功能；关闭 ECS 后在线任务仍继续走 P2P。展示 cloud assisted but local-first，而不是全流量中转。

---

## 21. 各阶段交付物 Checklist

### V0 交付

- [x] Rust Workspace、core/protocol 类型、SQLite migrations 与三平台编译 CI
- [x] `hmesh --data-dir`、幂等 init、doctor、Daemon 与任务 CLI 最小集
- [x] 两个独立实例，身份/证书/数据库/凭据/session-dir/项目目录分离
- [x] 回环 HTTPS/mTLS、指纹配对、项目授权和审批负例
- [x] Pi RPC、DSH headless JSON、OpenCode run JSON 三种 P0 Adapter 与版本锁定的契约 fixture
- [x] 三种真实 Harness 的 Managed Run、事件、工件与确定性核验；Pi/DSH 双向委派
- [x] 去重、重启后记录可查、未知结果不自动重跑、取消状态如实报告
- [x] E2E-00、single-machine 示例、quickstart-v0、显式 opt-in 本机 smoke
- [x] 本机已验证/未验证的 Harness 能力清单；不以接口探测代替真实任务，Codex 与跨设备验收留待 V0.1

### V0.1 交付

- [ ] V0 回环 E2E 回归通过；复用同一协议、授权及执行链路
- [ ] `hmesh-daemon`, `hmesh-cli`, `hmesh-mcp` 可用
- [ ] Tailscale candidates + 手动配对 + 双向信任
- [ ] 本地 Agent Registry + Pi/Codex 真执行
- [ ] Task state/event/SQLite + 去重/取消/错误
- [ ] macOS/Windows/Linux CI 与 Mac↔Windows E2E
- [ ] `docs/protocol.md`、安全文档、快速开始

### V0.2 交付

- [ ] Service lease、TCP proxy、访问授权、撤销/回收
- [ ] Windows→Linux SSH Gateway；严格 host key
- [ ] Linux 不装 Tailscale 的逻辑 Node/Agent 可见与执行
- [ ] OpenAPI / Test Artifact / IntegrationRun
- [ ] Mac→Windows→Linux 联调真实 E2E

### V0.3 交付

- [ ] 策略/审批/取消语义加固
- [ ] 断网/崩溃/重启稳定性与故障注入
- [ ] 跨平台后台服务与安装升级
- [ ] 安全回归、兼容版本矩阵与资源基准

### V1 交付

- [ ] `hmesh/1` 契约与稳定 CLI/MCP
- [ ] 真实 Harness/OS Smoke、发布二进制
- [ ] 两个 Demo 完整复现、社区文档与 Release
- [ ] 无 ECS 依赖的本地工作路径

### V2 交付

- [ ] 可选 Hub（二进制、Docker Compose）
- [ ] 目录、在线状态、离线信箱、幂等 ACK
- [ ] ECS 2C2G 实测与备份恢复
- [ ] Web 状态 UI（最小权限）
- [ ] Hub 宕机时 P2P 继续可用与降级测试

---

## 22. 建议开发拆分与并行工作包

第一轮目标是 V0 的 L01-L05；下表 A-E/G 的本机基础可用于 V0，B 的 Tailscale 能力、E 的 MCP 与跨设备 QA 从 V0.1 补齐。仅在 V0 合格后进入跨设备验收；本表只描述工作边界与依赖，不要求实际启动多个 Agent。

为降低共享文件冲突，前期按 Rust crate 边界并行开发，不应直接安排多个 Agent 同时修改同一组核心文件。

| Workstream | Owner 范围 | 依赖 | 最早开始 |
|---|---|---|---|
| A — Protocol & Model | core/protocol、状态机、OpenAPI fixtures | 无 | Day 1 |
| B — Identity/Network | security、tailscale transport、配对 | A 基础 Node 类型 | Day 2 |
| C — Persistence/Runtime | store、task engine、SSE | A 状态机 | Day 2 |
| D — Harness Integration | V0 pi/dsh/opencode、contract fixtures；V0.1 增加 codex | A Adapter interface | Day 2 |
| E — CLI/MCP | 本机客户端命令和 Tools | A+C 主要契约 | Day 3 |
| F — SSH/Service | Gateway、TCP forward、租约 | V0.1 合格 | V0.2 |
| G — QA/E2E | 真机测试、网络故障、报告 | 随各模块进展 | 全程 |
| H — Cloud Hub | hub、offline mailbox、Web | V1 契约稳定 | V2 |

以上的 `Day` 只代表依赖/相对施工顺序，**不是工期承诺**。

### 22.1 开发 Agent 的任务模板

```markdown
## Task: Wxx — <name>
Phase: V0.x
Goal: <single testable outcome>
Scope: <exact crate/files to own>
Inputs: <interfaces and fixtures available>
Implementation:
  1. Write contract tests first.
  2. Implement only the specified behavior.
  3. Add negative/security tests.
  4. Update docs and examples.
Non-goals: <out of scope>
Validation:
  - cargo fmt --check
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo test --workspace
  - targeted integration/e2e
Acceptance evidence:
  - test names + output summary
  - changed files
  - any deviations from SPEC + ADR
  - remaining risks
```

### 22.2 第一轮开发 Agent 指令（可直接复制）

> 在独立的 HarnessMesh 项目中先实现 V0（Local Loopback），以本 SPEC 为产品范围来源，按 L01-L05 交付。先建立 Workspace、core/protocol、SQLite、多实例 `--data-dir` 和最小 CLI，再建立真实回环 HTTPS/mTLS、双向配对、项目授权与 Task Engine。V0 P0 按 Pi RPC → DeepSeek Harness headless JSON → OpenCode run JSON 接入，以第 7.8 节本机核查作为版本/接口基线，重新验证真实模型与工具执行；Codex 留到 V0.1。先用 Pi 打通首条任务，再以 Pi/DSH 验证两个方向的委派，并将一个执行端换为 OpenCode 复测；两个独立 Daemon 和测试项目执行 E2E-00，验证状态、工件、重复请求、授权拒绝与重启记录，不要求三者所有两两组合。先建立测试；第三方参数必须由版本探测和真实接口验证，help/无模型响应不等于真实任务成功。不要把 Tailscale、另一台电脑、MCP 或 Codex 作为 V0 前置，不引入 ECS、SSH Gateway、服务暴露、Web 前端或无认证的本地执行入口。交付可编译代码、single-machine 示例、quickstart-v0、显式 opt-in 本机 smoke、测试和 ADR；执行 fmt/clippy/test 并报告已验证能力、真实证据与阻塞点。

**V0 通过后，复用其核心实现补齐 W01-W09：增加 W02 Tailscale 接入、跨设备配对、Codex 与 MCP，直到出现跨 Mac/Windows、跨 Pi/Codex 的真实执行证据，再宣称 V0.1 完成。**

---

## 23. 待决问题（默认采用建议方案，除非真机验证推翻）

| 问题 | 当前默认决策 | 决策依据 |
|---|---|---|
| 新项目名称 | HarnessMesh（临时） | 先不把品牌当技术依赖 |
| 语言 | Rust，必要时 Adapter 用 TS | Daemon 常驻、跨平台、资源可控 |
| 数据库 | SQLite 本地/Hub 单实例 | 低资源与低运维成本 |
| 节点传输 | V0 loopback HTTPS/mTLS；V0.1 起 Tailscale 私有 TCP + 应用层认证 | 先验证本机闭环，复用相同协议与执行链路 |
| 设备发现 | Tailnet candidates + 手动可信配对 | 无 Hub 的可控启动 |
| RPC | HTTPS JSON + SSE | 简单、可重连、可观察 |
| Harness | V0 P0：Pi + DSH + OpenCode；V0.1 必交 Pi + Codex | 先使用本机已有 runtime，复用任务契约，再扩展跨设备与 OS |
| Linux 校园接入 | Windows 限定目标 SSH Gateway | 不让整个校园子网暴露 |
| 服务数据通道 | 受控 TCP proxy + lease | 跨 OS 一致，不要求 Serve 特性 |
| 云服务 | V2 完全可选 | 保证 local-first |
| A2A | 兼容性拓展而非 V0.1 核心 | 控制范围，避免过度承诺 |
| MCP | 本机 tools 接入 | 主动调用异构 Harness 的共同接口 |
| 许可证 | 建议 Apache-2.0 或 MIT，正式发行前确定 | 不要继承其他项目代码许可证假设 |

---

## 24. 官方文档与技术依据（开发前复核当前版本）

- Tailscale CLI / `status --json`：https://tailscale.com/docs/reference/tailscale-cli
- Tailscale MagicDNS：https://tailscale.com/docs/features/magicdns
- Tailscale Identity / Grants：https://tailscale.com/docs/concepts/tailscale-identity
- Tailscale Serve：https://tailscale.com/docs/reference/tailscale-cli/serve
- A2A 1.0 规范：https://a2a-protocol.org/v1.0.0/specification
- A2A 源仓库：https://github.com/a2aproject/A2A
- MCP 协议主页：https://modelcontextprotocol.io/specification
- MCP v2 TypeScript SDK：https://ts.sdk.modelcontextprotocol.io/v2/
- Pi RPC 文档：https://pi.ubitools.com/rpc/
- Pi 当前上游与 RPC 文档：https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md （本机 `0.84.2` 自带文档与当前 main 可能不同，须锁定 fixture）
- DeepSeek Harness CLI profiles：https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/README.md
- DeepSeek Harness headless JSON：https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/headless/README.md （本次已读取安装包同路径自带文档）
- DeepSeek Harness SDK stdio：https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/sdk/server/README.md （本次已读取安装包同路径自带文档）
- OpenCode CLI / run JSON：https://opencode.ai/docs/cli/
- OpenCode 配置与配置目录：https://opencode.ai/docs/config/
- Codex Harness / CLI / SDK 选型：https://developers.openai.com/blog/codex-as-a-platform
- Microsoft OpenSSH Server 文档：https://learn.microsoft.com/windows-server/administration/openssh/openssh-server-configuration
- Headscale（可选未来替代协调层）：https://github.com/juanfont/headscale
- NetBird（参照实现）：https://github.com/netbirdio/netbird
- EasyTier（Rust Mesh VPN 先例）：https://github.com/EasyTier/EasyTier
- AgentMeld（仅可选未来集成，不是依赖）：https://github.com/jackie-cqz/AgentMeld

> 规范边界声明：A2A/MCP/Tailscale/Pi/DSH/OpenCode/Codex 都可能随版本变化。此 SPEC 指导**自研项目**的设计，不保证所有示例第三方参数在所有安装版本上直接可运行。必须由 Adapter 的版本探测、契约测试和真机 E2E 确认。

---

## 25. 最终 Definition of Done

HarnessMesh 达到 V0，意味着**当前一台电脑上**的两个独立 Node 已通过认证与授权分别完成 Pi/DSH/OpenCode 的真实任务和 Pi/DSH 双向委派，任务/事件/工件证据可核验，重复请求不重复执行，重启后记录可查，未知结果不会自动重跑。Codex 不阻塞此阶段；接口探测不能替代真实模型/工具运行。这是进入 V0.1 跨设备联调的基础，不是跨设备验收结论。

HarnessMesh 达到 V1，不是因为“Agent 列表能出现”，而是因为能给一个普通开发者带来以下体验：

> 两台电脑加入已有 Tailscale，安装一个轻量 Daemon 和各自可用的 Harness。用户授予项目级权限后，可以在一台电脑向另一台电脑的 Agent 交付实际编码任务，得到过程状态和可检验证据；还能按需、短时间发布开发 API，进行前后端跨机器联调。无需共享整个文件系统，无需公开校内服务，不依赖 ECS。

HarnessMesh 达到 V2，是在**不破坏上述体验**的基础上增加：

> 可选 ECS 协调中心，让已授权设备目录与离线任务传递更方便，断线恢复更可追踪；但本机仍拥有执行、审批、项目文件和数据通道的控制权。

**若实现结果只能跨机器传聊天消息、不能运行真实 Harness 或实际访问对方开发服务，则没有完成本 SPEC。**
