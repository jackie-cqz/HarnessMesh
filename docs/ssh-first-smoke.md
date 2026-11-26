# 首条真实 SSH 跨机联调

2026-10-28 已通过一条 **Windows → SSH → Linux Daemon → Pi 1.4.0 → DeepSeek V4 Flash** 的真实任务。Windows 和 Linux 各运行一个独立的 Rust hmesh Daemon，任务通过 HTTPS/mTLS 和项目审批；SSH 只负责 TCP 转发。

## 本次结果

- Provider/model：`deepseek/deepseek-v4-flash`，HTTPS API 主机为 `api.deepseek.com`。Flash 使用 Linux 原有 models.json 的模型定义。
- Linux 先通过本机 owner 的真实模型验证，随后 Windows 发起跨机任务，等待 Linux owner 批准才建立执行 attempt。
- Pi 实际调用 write/read，在 Linux 专用项目生成并读回 `out/health.json`。JSON 含 status、harness、model 和本次 request_id。
- Windows 经 Mesh 读取完整递增事件和确定性核验结果，并通过独立 SSH 文件读取核对 JSON 和 SHA-256。
- 相同 request-id 重发返回原 task/attempt，没有新增运行事件，工件 hash 不变；事件游标补读通过。
- 本次两个 Daemon 和专用 SSH 隧道均已停止，Linux 原有认证文件 hash 未变，原有 7892 转发仍在。

任务：`f2346c90-aa07-4a73-8b6b-fedd9fbf41a7`；Attempt：`d2bb6027-7f7c-4900-a80f-6ce305a3e319`。

文件 SHA-256：`447c3fe200747c5add84237cbbdf8d7375d94e0d8f69c52197a746ba269ca786`。

完整脱敏证据：[latest-ssh-smoke.json](evidence/latest-ssh-smoke.json)。该文件绑定两端二进制和 Cargo.lock hash，记录两端 Node ID、任务/尝试、原生工具事件、独立文件断言和清理结果。

## 转发关系

```text
Windows Daemon 127.0.0.1:38880        Linux Daemon 127.0.0.1:38881
Windows 127.0.0.1:38881 --SSH -L----> Linux 127.0.0.1:38881
Windows 127.0.0.1:38880 <---SSH -R--- Linux 127.0.0.1:38880
```

端到端 HTTPS/mTLS 位于 SSH 内部，仍核对证书 pin、当前信任与项目 grant。两边继续只监听回环地址；各自公开的 Node Card 地址在另一端由专用转发接通。

脚本从现有 SSH alias 读取 HostName/User/IdentityFile，生成本次独立 SSH 配置，启用严格 host-key 检查和 ExitOnForwardFailure。不会改写用户 `.ssh/config`，也不会复制原 alias 中的 7892 转发，避免与现有隧道争用。只停止本次记录的进程。

## 实际适配

Linux 的 Pi 位于 NVM Node 24.20.0 下，非交互 SSH 的默认 Node 为旧版，因此 Agent 配置新增本机 owner 字段 `adapter.node_executable`，与实际 JavaScript entry 配套使用。远端无需修改 PATH 或原 Pi 安装。Pi 1.4.0 的隔离版本/RPC 探测、最终任务完成条件与真实工具任务均已验证。

`adapter.models_file` 读取 owner 显式选择的模型定义，只向每次隔离配置写入选中的 DeepSeek 模型元数据；丢弃 provider API key/headers，拒绝模型内嵌的认证字段，并限制到官方 HTTPS API origin。

`adapter.credential_source` 是 Linux 本机认证文件引用：仅支持 Pi/DeepSeek 的 api_key 项，在内存中读出并注入子进程 `DEEPSEEK_API_KEY`，纳入日志脱敏。不复制 auth.json、不修改原认证、不导入其他 provider/OAuth，也不向 Windows 返回密钥。没有显式引用时仍按原环境 allowlist 机制工作。

## 复现入口

`scripts/e2e-ssh.ps1` 必须显式传 `-RunRealHarness`。它需要一个新的本机试验目录和一个预先准备好的 Linux 专用构建目录：

```text
REMOTE_TRIAL/
  source/Cargo.lock              # 与本机相同的锁文件
  source.zip                    # 本次已验证的源码快照
  build-target/debug/hmesh       # 本机编译的 Linux 二进制
```

每次试验使用全新的实例目录，已有身份不覆盖。远端 helper 在该目录下创建 node-linux、project 和运行数据，并拒绝复用已有实例。

```powershell
pwsh -NoProfile -File .\scripts\e2e-ssh.ps1 `
  -SshHost linux-host `
  -RemoteRoot /absolute/fresh/remote-trial `
  -LocalRoot C:/absolute/fresh/windows-trial `
  -Model deepseek-v4-flash -RunRealHarness
```

本次 Linux 二进制使用独立目录中的 Rust 1.90.0 构建；官方 rustc/cargo/std 组件已按发布清单 SHA-256 校验。Cargo 依赖由本机按 Cargo.lock vendor 后校验上传，Linux 采用 `--offline --locked`、编译并发 2。没有改系统工具链、登录配置或现有项目。

该脚本目前针对本次 Linux 的已核查 NVM/Pi 路径；换机器时先更新 helper 的本机路径并重新探测版本。构建与试验数据属于各自初始化账户，保存在专用私有目录。

## 范围

这是首条单向 SSH 跨机真实任务证据。尚未验证 Linux→Windows 任务委派、SSH 故障期间的跨机恢复、Tailscale、Codex/MCP、开发服务发布或原生 Harness abort，不能据此宣称完整 V0.1 完成。已有 V0 Windows 三 Harness 验收报告作为原版本历史证据保留，不用新二进制 hash 覆盖历史报告。
