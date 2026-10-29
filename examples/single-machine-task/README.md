# 本机任务示例

完整复现见 `docs/quickstart-v0.md` 和 `scripts/e2e-local.ps1 -RunRealHarnesses`。脚本会生成本机适用的配置；这里保留可供手动复制修改的最小样例。

1. 创建三个专用测试项目目录，复制对应 TOML，将 `C:/absolute/...` 替换为本机绝对路径。TOML 不展开 `~` 或环境变量。
2. A 登记 Pi，B 登记 DSH/OpenCode；使用 `agent verify ID --allow-model` 分别执行真实 owner 验证，等待成功。
3. 复制策略，填写真实 peer/agent UUID 后在执行端应用。B 授权 A 调用 DSH/OpenCode；A 以相同格式授权 B 调用 Pi。每个项目最小授权，不使用全局通配。
4. 从发起端 `task send AGENT --peer NODE --project PROJECT --prompt TEXT`；执行端 `task approve TASK`；发起端 `task watch TASK`。
5. 通过 `task verify` 对 `out/health.json` 做独立断言。实例密钥、运行数据和真实输出仅放 `.hmesh-v0/`，不得提交。

本次已验证 Pi/DSH/OpenCode 的共同 provider 为 DeepSeek。Pi/OpenCode provider 名为 `deepseek`，DSH 为 `deepseek-official`；密钥由 Daemon 启动环境中的 `DEEPSEEK_API_KEY` 提供。样例使用的模型可按账户可用性修改，改后需重新验证。允许工具为 read/write/edit，不含 shell、网络和子 Agent。
