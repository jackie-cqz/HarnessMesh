# 当前安全与能力边界（V0、SSH、Tailnet 与直连网络）

V0 只在本机回环网运行，但每次操作仍经过证书和当前配对信任。数据目录内私钥、证书、owner token、数据库、配置和每次运行数据各自独立。Windows 使用仅当前用户可访问的 ACL，Unix 使用 0700/0600；init 拒绝覆盖未完成身份或非空的无关目录。相同 data-dir 的第二个 Daemon 因目录锁失败。

显式 Tailnet 模式每次启动须通过本机 Tailscale CLI 验证运行/在线及确切监听 IP；只绑定该字面 IP，并给新证书加入对应 SAN。公开监听仅接受 Tailnet 地址来源，拒绝整个 `/local/*`，即使请求携带 owner 证书和 token；owner 经独立回环 socket 管理。候选发现只显示必要网络元数据，不自动信任、授权或调用模型。默认回环实例不能主动连接别的电脑，owner 客户端不向任何网络发送 token。不解析任意 DNS。详见 [直连说明](quickstart-tailnet.md)。

**直连网络（`network = "direct"`）。** 不用 Tailscale：电脑之间走它们本来就连得通的网络（局域网、ZeroTier、WireGuard 等）。以前配置明确禁止监听局域网和公网地址，直连模式有意放开了这一条，换成下面几道关：

- **只监听一个具体地址**：`listen_address` 必须是本机的一个字面地址，不允许 0.0.0.0、`::`、多播或广播地址。启动时绑不上（例如 DHCP 换了地址）就退出，并提示重跑 `hmesh setup`。owner 照旧走独立的回环监听，对外监听拒绝整个 `/local/*`。
- **来源网段**：对外监听只接受 `allowed_sources` 里的来源，不在网段里的连接在 TLS 握手之前就丢掉。不配置时默认是私有网段：10/8、172.16/12、192.168/16、100.64/10、fc00::/7，以及回环地址。监听公网地址时必须显式配置，否则配置校验不通过。网段只是第一道篱笆，不是认证。
- **配对窗口**：hello、challenge 和配对申请这几个匿名接口，只在 owner 打开面板的「配对新电脑」或运行 `hmesh pair show` 之后的 10 分钟内回应，其余时间回 `PERMISSION_DENIED`。这样平时别人连配对申请都发不进来，批准列表里只会有窗口那 10 分钟里收到的申请。窗口不隐藏证书：TLS 握手照常把证书出示给允许网段里的任何连接，所以指纹不是秘密；防冒充靠的是批准时核对校验码，以及「多条申请对得上同一个校验码就拒绝批准」。对已有申请的状态查询不受窗口限制：申请 ID 是随机的，只有发申请的一方知道。已配对的电脑互相探测在线和读取 agent 走带证书的接口，也不受窗口限制。配对窗口只记在内存里，daemon 重启后就关上。
- **按来源限流**：所有网络上的匿名接口都按「来源 IP 每分钟」计数，同时不能超过「所有来源合计的 10 倍」。超出本来源额度而被拒的请求不计入合计，所以一个地址不停地发，也占不掉别人的额度。按地址计数：一台机器用上很多地址（比如 IPv6 下同一网段的多个地址）仍能多占几份，最多到合计上限；来源网段越窄，能用的地址越少。
- **能连的地址**：对端地址可以是任何字面单播 IP，包括局域网和公网地址；只拒绝链路本地地址（169.254.0.0/16 是云主机元数据服务所在的网段，fe80::/10 需要区域标识）、未指定地址、多播、广播和保留地址，也不接受域名。对端到底是谁，由固定的证书指纹决定，和地址无关。
- **两端都要升级**：旧版本只接受 Tailnet 地址的对端，还会核对证书里是否写着拨号用的 IP，所以用直连的两台电脑都要装这一版或更新的版本。只用 Tailscale 的电脑之间不受影响。
- **macOS 本地网络权限**：macOS 要求进程有「本地网络」权限，才能向局域网地址发起 TCP 连接（[Apple TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)）。从终端运行的命令和 launchd daemon 自动放行，开机自启装的 launchd agent 不在放行之列。被拒绝时连接会报 `EHOSTUNREACH`，HarnessMesh 会把它翻译成去「系统设置 → 隐私与安全性 → 本地网络」打开权限的提示。Tailscale 等 VPN 地址不算本地网络，不受这条影响。

**只装 MCP（只出不进）。** `hmesh setup --mcp-only` 建的节点在本机装了并登录了 Tailscale 时是 `network = "tailscale"`，否则是 `network = "direct"`，两种都是 `expose_to_tailnet = false`：只在回环地址上开一个监听，不对外监听，也不在启动时要求 Tailscale 在线；Tailscale 节点配置里的 `tailnet_address` 只写进证书 SAN，留给以后换成完整安装时用。Daemon 对任何不是本机 owner 的请求一律回 `PERMISSION_DENIED`：已配对节点的受认证请求、匿名的 hello、challenge、配对申请和申请状态查询都一样，所以它只能主动向外配对和调用，别的电脑什么都调用不了它，也看不到它的 agent。完整安装的节点看到对端名片的地址是回环地址时，就把它当成只出不进的电脑：不探测、不调用，面板上显示「只调用」。

**名片更新。** 已配对且受信任的节点可以 `POST /hmesh/v1/peers/card` 更新自己在对方那里的名片：要求名片的 node_id 等于连接认证出的节点、指纹等于这条连接的证书指纹，并通过和配对时相同的 `validate_card` 校验；不会改动信任状态，也不会接受别的节点的名片；每次更新记一条 `peer.card` 审计。每个 Daemon 启动时把自己的名片发给每个受信任的对端，没送达的每 5 分钟重试。只出不进的节点拒绝这个接口，收不到对方的新名片，这是已知限制。

## 认证和授权

TLS 只允许 1.3。客户端先核对明确的 SHA-256 证书 pin，再由 rustls/WebPKI 验证证书编码、有效期、用途与握手签名，不存在接受所有证书的模式。证书不再需要写着拨号用的那个地址：指纹已经锁定了唯一一张证书，TLS 握手又证明对方持有它的私钥，再核对地址不增加保护。这样局域网地址变了、换了网卡或网络，都不用重新配对。所有 HarnessMesh 证书都写有 localhost，WebPKI 统一按这个名字检查。匿名入口仅用于 hello、单次 challenge、签名配对申请和申请状态查询；challenge 有目标绑定、过期和使用一次的约束。

配对只需对方 owner 批准一次：发起方核对对方的 pin（配对码里带着完整指纹）发出申请，此时双方都不信任对方。对方 owner 核对发起方的校验码（发起方证书指纹的最后 6 位）后批准，对方开始信任发起方。发起方的 Daemon 每 3 秒查一次申请状态；查到「已批准」后，再用自己的证书连一次对方的受认证接口，对方接受了这张证书才在本机信任对方，所以发起方不会在对方批准前、也不会只凭一个状态词就开放自己的 agent。状态查询是匿名的（发起方这时还不被信任），只回 pending/approved/declined/revoked/expired 一个词，只有持有随机申请 ID 的一方能查，并和其他握手请求共用每分钟 60 次的限额。过了有效期，发起方仍先问对方再下结论（对方可能在发起方关机或休眠时批准了），之后每分钟问一次，问到确定答案为止，连续一天联系不上才记为过期。对方拒绝或申请过期，发起方的申请随之结束；发起方取消自己的申请，或撤销对方的配对，都会结束和对方之间所有等待中的申请，之后对方再批准也不会让发起方重新信任它。

批准时校验码不对，面板拒绝批准。6 位校验码只有 24 位：在 Tailnet 里的人可以专门生成证书，离线试上千万次碰出同样的 6 位，冒充一条申请。所以面板在多条等待中的申请对得上同一个校验码时拒绝批准，并显示对方的地址；需要更强的保证时，用 CLI 的 `pair approve --expect-fingerprint` 核对完整指纹。

**配对是信任边界。** 已配对且仍受信任的节点可以看到本机登记的 agent，并能和它们开会话或派活；未配对或已撤销的节点什么都看不到、什么都派不了。每个对端有一个访问模式（policy 的 `[peers]` 段，默认 `auto`）：`auto` 直接开始，`confirm` 让会话停在 `pending_approval`、任务停在 PENDING_APPROVAL，等本机 owner 批准（`hmesh peers access`）。会话只允许打开它的那台电脑（和本机 owner）读取、追问、取消和关闭；权限请求由打开者回答，agent 打开的会话在 `auto` 下自动允许一次、在 `confirm` 下由本机 owner 回答。撤销配对会关闭对方在本机打开的会话。发送方也可以主动要求审批（开发流程的阶段任务就是这样）。旧的 `[[grant]]` 项目授权和 agent 的 `require_remote_task_approval` 仍可解析，但不再参与决定。agent 只在它登记的项目目录里工作。登记 Agent 只能由本机 owner 完成。只通过版本探测的 Agent 保持 unverified，真实 owner 验证成功后才能接收 peer 任务。任务在出队前和启动前重新检查配对信任。撤销也在每次 HTTP 请求中重查，因此连接复用不会保留旧授权。

owner API 必须同时具备自己的客户端证书、该实例的私有 token 和回环来源；信任的 peer 无法操作它。任务读、事件、核验结果和取消仅对原发起 Node 或执行端 owner 开放。审计记录授权决定、task/attempt/trace，以及审批、拒绝、取消、策略更新和信任撤销；不会把模型账户密钥当作审计字段。

## 受管开发服务与 SSH

服务发布、隧道配置、连接建立及联调启动只在本机 owner API 开放。服务 peer 白名单和可选 restart 白名单由 owner 明确设置；task grant 不会自动授予服务访问。远端服务卡隐藏内部端口、命令和环境。restart 仅使用原 owner 配置；MCP 会话工具没有这些管理能力。

HTTP 桥接每次重查当前信任和服务租约，缓冲响应释放前再次重查。两侧正文各不超过 256 KiB，最多八个本地并发请求，不转发 Cookie/Authorization，不自动重定向或重放写请求。SSH 使用独立空配置、显式身份及 known_hosts、strict host key 和最多两个固定回环端口转发；不能指定任意目标地址或 SOCKS。原用户 SSH 配置及已有转发不变。

进程由 owner 绝对路径和项目 root 启动，不接管占用端口或历史 PID。初启加最多三次自动重启，租约至多一小时；Daemon 正常停止回收自己的进程树。重启不复活旧授权，Unix 非正常强杀后可能有孤儿进程，owner 必须独立核查，不能依靠历史 PID 杀进程。OS 常驻安装及崩溃接管未实现。

联调固定保存契约/测试计划及其 hash，最多两次修复；确定性程序验收，模型完成不代表测试通过。原任务审批继续生效，远端审批不能由模型代行。网络或任务结果未知时停止，不生成新 UUID 猜测执行。报告记录响应 hash/断言，不包含响应正文；配置/契约与预期断言也应避免敏感信息。服务 env 属于本机 owner 私有配置，当前没有服务凭据管理器。细节见 [受管服务说明](managed-services.md)。

## Agent 运行机制（ACP）

HarnessMesh 只通过 [Agent Client Protocol](https://agentclientprotocol.com) 驱动 agent：以参数数组直接启动 owner 登记的 ACP 命令（`opencode acp`、`claude-agent-acp`、`codex-acp`、`pi-acp`、`dsh --profile acp` 或其他），工作目录为登记的项目目录，不拼 shell 命令。远端不能改变命令、cwd、环境或参数。默认 agent 用它自己的登录和模型设置；HarnessMesh 不读取、不复制、不导入任何 agent 的凭据文件（旧的 `credential_source` / `models_file` 已移除并会被拒绝），只有 owner 在「模型服务」里给的值会以环境变量交给 agent（见下）。owner 在 `env_allowlist` 里列出的环境变量缺失时，启动前失败。

客户端在 `initialize` 里声明不提供文件系统和终端能力，agent 用自己的工具读写项目。agent 的每个 `session/request_permission` 都由会话的回答方决定（见上节）；一次性任务只允许 owner 在 `allowed_tools` 里列出的工具类别，其余一律拒绝并以 `FAILED_NEEDS_INPUT` 结束。**固定工作目录和权限回答不构成 OS 文件系统沙箱**；agent 进程拥有当前用户的底层权限，只应登记可信项目。

每个打开的会话对应一个长驻 agent 进程，空闲 10 分钟后停止。下次发消息时，HarnessMesh 根据新进程的 ACP 握手续上原会话。声明 `loadSession` 的 agent 使用 `session/load`，DSH 使用 `session/resume` 恢复上下文，面板从本地记录显示历史。恢复请求失败时，本轮停止并保留原会话 ID，便于重试。两种恢复能力都未声明的 agent 会开新会话，并提示之前的上下文已丢失。登记 agent 只做一次 `initialize` 握手，不创建会话、不调用模型。

### 会话设置和模型服务

- **会话设置**（模型、模式、思考强度）走 ACP 的 `session/set_config_option`，打开会话的电脑可以在两轮之间修改，值由 agent 校验。用户的选择记在会话上，进程恢复后，HarnessMesh 会把已经回到默认值的选项重新设为用户所选的值。
- **模型服务**（每个 agent：用它自己的登录 / API key / 自定义地址、默认模型、额外环境变量）只能由本机 owner 通过 `/local/agents/{id}/service` 设置，存在 `<数据目录>/agent-secrets/<agent id>.json`（目录 0700、文件 0600，原子写入），和节点私钥同一级保护；没有对应的 peer 接口，配对电脑只看到 `service_configured` 和默认模型名，看不到 key。接口只写不读：返回值只说「已设置」和长度足够时的末 4 位。
- 启动 agent 进程时按类型换成环境变量：OpenCode 用 `OPENCODE_CONFIG_CONTENT` 注入一个 `hmesh` 服务商（地址是本机转发，见下一条；转发的令牌放在单独的 `HMESH_OPENCODE_API_KEY` 里，配置文本里只有 `{env:…}` 引用；模型列表里的模型都注册进去，并用 `enabled_providers` 让会话只在这些模型之间切换，不会切到用户自己 OpenCode 账号下的模型；标题等小请求也走同一个地址），Claude Code 用 `ANTHROPIC_API_KEY` 或 `ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN`，Codex 用 `OPENAI_API_KEY` / `OPENAI_BASE_URL`，Pi 与 DSH 使用按 Agent 生成的服务商配置和专属环境变量。通用 ACP 可指定它读取 Key 的环境变量名。冲突的继承变量会被去掉。**不改用户自己的 agent 配置文件**。
- **OpenCode 自定义地址走本机转发**：有的 OpenAI 兼容网关（如 `api-gateway.glm.ai`）流式回复的最后一块带 `delta.role: ""`，OpenCode 内置的 AI SDK 只认 `"assistant"` 或不带，收完回复后整轮报 `Type validation failed`。所以 daemon 在 `127.0.0.1` 的随机端口上开一个转发（第一个用到它的 agent 启动时才开），OpenCode 连 `http://127.0.0.1:<端口>/relay`，拿到的是一个随机令牌（`hmr-` 开头，同一地址、协议和 Key 复用同一个，daemon 重启后作废），**真 key 只在 daemon 里**，OpenCode 进程的环境变量里没有。转发只接受已知令牌，路径只能接在配好的地址后面（换不了主机），不跟随重定向，去掉 `Authorization`、`Cookie`、`Accept-Encoding` 等本跳的头再带上真 key；回复原样流回，只把 `choices[].delta` / `choices[].message` 里值为空字符串的 `role` 删掉（流式逐行处理，单行超过 4 MiB 或压缩过的回复原样放过）。不记录请求和回复内容。OpenCode 的 `NO_PROXY` 和 `no_proxy` 设成同一份：合并两个名字原有的排除项（agent 的额外变量和 daemon 环境里的都算），再加上 `127.0.0.1`，免得它继承的代理把转发请求和令牌带到别处（有的客户端两个都设时只认其中一个，所以两个必须一致；合并后给单个 agent 设的 `NO_PROXY` 也会带上 daemon 环境里的排除项，没法再用它让某个主机强制走代理）；转发到真实地址时用 daemon 自己的代理设置（`HTTPS_PROXY` / `NO_PROXY` 等，回环地址不走代理），给单个 agent 配的额外代理变量对经转发的模型请求不起作用。令牌只在本机 127.0.0.1 上有用、daemon 重启即作废，所以不当作密钥脱敏。Claude Code、Codex 和 OpenCode 的其他模式仍直接连。
- 注入的 key 和额外变量（8 个字符以上）加入脱敏名单，不会出现在会话事件、最终回复和数据库里。`PATH`、`HOME` 等系统变量和 `HMESH_` 开头的变量不能被覆盖。改了模型服务后，新会话和已有会话的下一轮会用新的进程。
- **测试连接**（`/local/agents/{id}/service/test`，同样只有本机 owner 能调，没有 peer 接口）会把 key 发给填写的地址：只在 owner 点按钮或运行 `hmesh agent service … --test` 时发生。先请求模型列表（OpenAI 协议使用 `GET {地址}/models`，Anthropic Messages 使用 `GET {根地址}/v1/models`），不花 token；列不出来才用默认模型发一次最小的请求（Chat 接口 `max_tokens=1`；Anthropic Messages 走 `/v1/messages`；OpenAI Responses 走 `POST {地址}/responses`，`max_output_tokens=16`）；key 被拒（401/403）或连不上就直接报告，不再发第二次。不跟随重定向（避免把 key 带到别的地址），回环地址不走代理，整次测试 13 秒内结束。返回值、报错和审计记录（`agent.service.test`，只记模式和成败）里都不含 key，服务商回包里如果引用了 key 也会被替换。
- **检测 agent 自己的配置**（`/local/agents/{id}/service/check`，只有本机 owner 能调）只问 agent 自己：先用 ACP 的 `auth/status`（claude-agent-acp 会回答，也接收它推送的 `_auth/status_update`），再跑它自带的只读状态命令（Codex `codex login status`、OpenCode `opencode auth list`、Pi `pi --list-models`）。**不读任何 agent 的凭据文件，也不运行会打印凭据的命令**（如 `pi auth`）；不建会话、不调模型；每个命令最多等 10 秒。检测结果（登录方式、账号邮箱）存在 `<数据目录>/agent-login/<agent id>.json`（0600），只在本机弹窗显示；配对电脑只从 agent 卡片的 `own_login` 看到「已配好 / 没配好 / 看不出来」。`codex login status` 给 API key 时会带 key 的前后几位，HarnessMesh 只保留「API key」这个结论。审计记录 `agent.service.check` 只记三态结果。
- **key 只填环境变量名**：owner 可以让 key 在启动 agent 时从 daemon 的某个环境变量读取（`api_key_env`），设置文件里只存变量名。变量没设就不启动 agent，并在会话里说明缺哪个变量。读到的值和存下的 key 一样加入脱敏名单。系统变量和 `HMESH_` 开头的变量不能用。
- **Claude Code 接第三方地址**时，选了默认模型就同时设 `ANTHROPIC_MODEL`、`ANTHROPIC_DEFAULT_OPUS/SONNET/HAIKU_MODEL` 和 `CLAUDE_CODE_SUBAGENT_MODEL`（否则它会发出 Claude 自己的模型名），并设 `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`，避免它拿别家的 key 去请求 Anthropic 的后台服务。

## MCP 服务器的权限

`hmesh mcp`（`hmesh-mcp`）是本机程序，使用本机 owner 的 RPC 客户端身份。模型只看到六个固定会话工具，拿不到私有 token、任意路径读取、配对或 policy 修改、批准接口。能读到节点目录的本机进程本来就能拿到 owner 身份，MCP 服务器没有扩大这个范围。

HarnessMesh 注入给会话的服务器带 `--session <id>` 启动：会话不存在或已关闭时拒绝启动，不会退化成不受限的 owner 模式；它只能操作自己这个会话打开的子会话，打开的子会话带上调用链。调用链最多 3 层、不能回到链上已有的 agent、同一会话最多 4 个运行中的子会话；**别的电脑打开的会话（以及它的后代）只能叫本机的 agent**，不能借本机身份连到第三台电脑。执行端仍按自己对来源电脑的访问模式决定是否需要确认；远端返回的文本按不可信数据处理。详见 `docs/mcp.md`。

**这些限制只管 MCP 工具，不是进程隔离。** agent 进程以当前用户身份运行，能读到节点目录（包括 owner 证书和 token）。一旦你批准它执行 shell 命令（`execute` 类权限），它就可以直接运行 `hmesh session open --peer …` 等命令，以本机 owner 身份做任何 owner 能做的事，包括连到其他配对电脑。所以：对别的电脑或别的 agent 打开的会话，批准 shell 权限前要想清楚；只想让 agent 互相对话时，拒绝 shell 权限即可。

已知凭据值在 text delta（包括跨 delta 的值）、工具事件文本和 stderr 中脱敏。原生工具参数和输出不作为公共事件转发。不要主动把密钥放入任务正文、工件或输出；任务正文和运行历史是需要私有保存的数据。未承诺对任意编码/变换后的秘密进行识别。

## 取消、崩溃和恢复

Windows 在发送 prompt 前将进程放入 kill-on-close Job Object，终止时确认 Job 中活跃进程为零；只追踪当前 attempt 的进程。Unix 使用专用进程组，无法保证已脱离进程组的子进程被包含。无法证明退出时返回 UNKNOWN。它们均不是通用进程沙箱。

受控 Rust 测试进程已覆盖运行中取消、超时、子进程清理、异常输出和 Windows Daemon 硬退出恢复。会话的取消走 ACP `session/cancel`，挂起的权限请求按协议回答 `cancelled`；daemon 重启后进行中的轮次标为中断，不自动重发。真实 agent 的 ACP 取消与 `session/load` 还没有逐一真机验证。Managed Run 采用进程树终止；Probe 中未测试的取消/恢复项保持明确标记。

SQLite 去重避免同一请求创建第二次尝试，不承诺跨外部副作用的 exactly-once。Daemon 崩溃后的运行结果归 UNKNOWN，需要用户检查副作用；不得自动重试。

## 验证范围

本机测试覆盖错误 pin、匿名/未配对访问、owner 凭据分离、发送方伪造、未知项目/远端 cwd、跨 origin 读取、未批准拒绝、去重、签名重放、策略热更新和信任撤销。测试进程和协议 fixture 与真实模型证据分开。

`cargo run --locked -p xtask -- audit` 通过官方 OSV querybatch 查询整个锁文件（包含其他目标平台），处理分页且遇到不完整结果直接失败。报告含时间和锁文件 hash。安全数据库随时间变化，需要在发布前重新执行；最近一次报告位于 `docs/evidence/dependency-audit.json`。

真实验收已覆盖 V0 本机 Windows、SSH 双向 Pi/DSH、断线与发起端重启后的补读/去重，以及 Pi 1.4.0 原生 MCP 委派 DSH 的独立审批和父子产物核验。Unix 硬崩溃时对 detached 子进程的清理仍不在已验证能力内。Windows 和 Orion/Linux 本地回归已执行；CI 已配置三平台，尚无远端 CI 执行证据。完整 V0.1 的 Tailscale、Codex/Mac↔Windows 链路仍需验收。见 `docs/ssh-bidirectional-mcp.md`。
