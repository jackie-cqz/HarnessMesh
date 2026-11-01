# 原生运行包

`Native runtime` GitHub Actions 为 Mac arm64 与 Windows x64 生成原生 release 运行包；以实际 runner 平台/CPU 和包内 manifest 为准。运行 CLI/面板无需 Rust、npm 或前端构建工具，MCP 也已包含。

从检查通过的 workflow 下载对应 artifact，再解压其中的 `.tar.gz`（Mac）或 `.zip`（Windows）。这是未签名开发包，没有系统安装器或自动更新；SHA-256 用于检测文件变化，不是发行者签名。

Mac 双击 `start-mac.command`，或在终端执行：

```sh
./start-mac.command /absolute/existing/node
```

Windows 在包目录使用系统 Windows PowerShell：

```powershell
powershell -NoProfile -File .\start-windows.ps1 -DataDir 'C:\absolute\existing\node'
```

默认数据目录是包内 `data`。空的独立目录初始化为 loopback Node；已有目录保留身份、项目、策略和凭据。默认面板端口 38900；Windows 可加 `-Port 38901`，其他平台可直接执行 `./hmesh --data-dir PATH panel --start-daemon --port 38901`。

完整访问链接仅在本机终端显示，不能转发。顶部关闭按钮或 Ctrl+C 正常关闭面板和它启动的 Daemon；已有兼容 Daemon 被复用并保留。启动前检查端口及 owner API 能力，旧 Daemon 需要 owner 显式升级/重启。升级已有实例前备份数据库，schema 6 不能用旧二进制打开。

包不含 Harness、其 Node.js 运行时、模型密钥或 Tailscale。真实任务需要执行端 owner 独立配置这些依赖、Agent 与项目授权。不会复制个人配置、扩大策略或自动调用模型。

## 本地打包与回归

在源仓库根目录执行：

```sh
cargo +stable build --release --locked -p hmesh-cli -p hmesh-mcp
node scripts/package-runtime.mjs --out target/my-new-bundle
node scripts/package-smoke.mjs --archive-dir target/my-new-bundle
```

只复制固定 CLI/MCP 二进制，生成启动器、说明、NOTICE、manifest 和校验清单。输出目录必须新建，避免旧文件混入。Windows 使用系统 Compress-Archive；Mac 使用 tar。manifest 包含 OS/CPU、源 commit、dirty 标记和文件 hash；未提交源码构建会标 dirty，CI 包绑定 PR commit。

回归实际解压压缩包、核对清单/hash、执行独立 CLI、初始化临时 Node、启动面板与自建 Daemon、鉴权读取状态、正常关闭并确认 Daemon 不再可达。测试子进程 PATH 不含 Rust 或 Node，无模型调用、无个人 Harness 配置，不等于新的真实跨机验收。
