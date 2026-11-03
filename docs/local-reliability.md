# 本机可靠性与接入

0.0.1 增加 schema 7。以下功能可用临时本机节点和无模型 fixture 回归，不要求第二台电脑。已有运行实例需要 owner 显式备份并重启升级；新面板不会替换旧 Daemon。

## 完成时工件快照

新任务成功结束、确认进程清理后，运行时读取执行前配置里的 `artifact_paths`。每个文件最多 10 MiB、最多 16 个路径、整组最多 32 MiB。文件必须解析到该任务工作目录以内；缺失、越界或超限会记录为完成时不可用。工件缺失不把执行成功改写为验收成功。

快照字节、原 attempt、执行配置摘要与成功状态在同一 SQLite 事务提交。失败不会留下已成功任务的半组快照。后续核验读取快照，项目里的同名文件被修改或删掉不影响该结果。之后新增授权路径不能给旧任务补造快照；撤销现有路径/peer/project grant 仍会拒绝远端核验。接口不向 peer 返回文件正文。

升级前成功任务没有快照，继续使用原来的当前文件核验，并显示 `legacy-current-file`。新记录使用 `completion-snapshot`。快照保存观察到的完成时文件，不证明文件一定由该 Harness 生成。SQLite 与本机 owner 仍是信任边界。

## 工作目录隔离与修改审阅

默认 `workspace_mode = "shared"`。同一 Node 中，解析到相同根目录的共享任务互斥，即使来自不同 Agent 或 project ID。不同 Node 之间没有跨进程的项目锁，不应把它们配置为并发写同一目录。

需要隔离时，在 Agent 配置增加：

```toml
[project]
root = "/absolute/clean/repository"
allowed_tools = ["read", "write", "edit"]
artifact_paths = ["out/health.json"]
workspace_mode = "git-worktree"
git_executable = "/absolute/git"
```

根目录必须是干净、已有 commit 的 Git 仓库根。每个 attempt 在 Node 的 `runs` 下面建立 detached worktree，原仓库工作文件不变。Mesh 的 Git 操作关闭 hooks、交互提示与外部 diff，单次有 30 秒 / 4 MiB 输出边界；超限后保留工作目录供 owner 检查。隔离是代码工作目录隔离，不是 OS 文件系统沙箱。

任务完成后保存相对于起始 commit 的二进制 diff 及 SHA-256。面板按 UTF-8 字节游标每次读取最多 64 KiB diff，显示进度并继续加载；常规轮询只返回摘要。每页核对同一修改 hash，完整 diff 也可通过 CLI 查看。应用修改是离线 owner 操作：

```sh
hmesh --data-dir NODE daemon stop
hmesh --data-dir NODE workspace show TASK_UUID
hmesh --data-dir NODE workspace apply TASK_UUID --sha256 REVIEWED_PATCH_HASH
```

应用前要求成功任务、精确 hash、无未恢复执行记录，并核对原仓库 HEAD 和干净状态。它只把修改应用到原目录及 index，不创建 commit。原目录变化、hash 不同或重复应用均拒绝。工作树保留供排查，不自动删除任务历史或未审阅修改。

## 离线备份与恢复

```sh
hmesh --data-dir NODE daemon stop
hmesh --data-dir NODE backup create --out /private/new-backup
hmesh backup check --from /private/new-backup
```

备份必须在 Node 外的新目录。操作取得同一 Daemon 排他锁，运行中实例会被拒绝，不会被偷偷停止。SQLite Backup API 生成一致性副本，执行 integrity/foreign-key 检查，并生成逐文件 SHA-256 manifest。备份保存身份私钥、owner token、配置、有效策略与数据库，因此目录只允许当前 owner 访问，不能提交或作为普通诊断包发送。源配置字段保留，避免备份旧 schema 时写入旧二进制不支持的新字段。

备份包含已有受管登录注册的本机引用文件，但不复制外部 Harness 模型凭据、项目源码、运行目录或 OS 登录注册本身。数据库里的引用和已保存 diff/快照会保留；这些外部资源需要 owner 单独保存。外部策略会捕获为备份内的 `policy.toml`。单个数据库完整性检查上限 10 GiB，其余文件上限 1 MiB。

恢复前先确认原 Node 已停止，把原目录另存为故障副本，使**原路径不存在**，然后运行：

```sh
hmesh --data-dir ORIGINAL_NODE_PATH backup restore --from /private/new-backup
```

恢复只允许 manifest 中的原路径，不覆盖现有目录，也不把配对身份克隆到另一个路径。校验完成后通过临时私有目录发布；不会启动 Daemon。先检查外部项目、可执行文件和凭据引用，再显式启动。失败备份、修改过的文件、新版不兼容 schema 都会拒绝。

## 队列与公平调度

已有全局 `max_concurrent_runs` / `max_queue_depth` 继续生效。队列容量包括待审批、排队和未结束执行；owner 模型验证任务同样受精确上限约束；重复原请求仍先去重，不会因满队列而变成新任务。

默认每个项目/Agent 最多一个并发、20 个未结束任务。可在 `node.toml` 中精确覆盖，并重启 Daemon 生效：

```toml
[runtime]
max_concurrent_runs = 4
max_queue_depth = 100

[runtime.project_limits.my-project]
max_concurrent_runs = 2
max_queue_depth = 30

[runtime.agent_limits."00000000-0000-0000-0000-000000000001"]
max_concurrent_runs = 2
max_queue_depth = 15
```

并发范围 1–16，容量 1–1000。共享目录互斥继续优先于更大的并发配置；并行写代码应使用独立 worktree。调度按项目最近一次实际派发轮转，并把次序保存在数据库中，重启不会丢失公平性记录。面板显示全局、项目、Agent 或共享目录等待原因。

## 项目/Agent 接入向导

在面板的 Agents 页面选择“接入项目与 Agent”，填写本机绝对路径、Harness、模型、工具和工件路径。凭据只接受本地文件或环境变量**引用**。检查步骤验证引用存在/格式，不把密钥返回浏览器，不调用模型。

审阅步骤展示固定 Agent ID、配置摘要以及可选可信 peer 的精确 `agent.read` / `task.submit` grant，固定要求审批。确认登记只运行 Harness 版本探测；Agent 仍然未验证。确认丢失后复用同一登记 ID，配置相同则返回原 Agent；不同配置拒绝。

向导不会应用策略、配对新 peer 或执行模型验证。需要远端访问时，把预览中的 grant 合并进当前策略，经 owner 检查后通过 `hmesh policy --file FILE` 应用；不要直接用预览覆盖其他项目的授权。模型验证仍须 CLI 的 `--allow-model`。

## 版本化运行包与升级

压缩包使用版本和 OS/CPU 命名，manifest 绑定源码 commit、文件 hash 和数据库 schema。解压后先使用新包的 CLI 检查并做升级前备份：

```sh
/new-package/hmesh --data-dir NODE upgrade check --bundle /new-package
/new-package/hmesh --data-dir NODE upgrade prepare --bundle /new-package --backup-dir /private/pre-upgrade
```

`prepare` 要求 Daemon 离线，先校验平台/CPU、文件 allowlist/hash/schema，然后备份。它不覆盖当前二进制或自动启动服务；owner 从新版本目录启动同一 Node。数据库升级后不能仅替换回旧二进制，必须停止新进程并恢复匹配的旧备份。运行包未签名，校验和不代表发行者签名。

## 登录自启管理

```sh
hmesh --data-dir NODE startup plan
hmesh --data-dir NODE startup install
hmesh --data-dir NODE startup status
hmesh --data-dir NODE startup uninstall
```

`plan` 只输出配置。`install` 显式注册当前用户下次登录时启动此 Node 的 Daemon，不立即启动，不打开面板；不注册系统服务或管理员任务。Mac 使用用户 LaunchAgent，Windows 使用绑定当前用户 SID、InteractiveToken 和最低权限的任务，任务文件以带 BOM 的 UTF-16LE 保存。不会保存账户密码或环境变量密钥。启动后会按原规则处理既有已批准队列，且继续检查授权；不自动重新执行 UNKNOWN。

自启引用安装时的绝对二进制路径，安装后应保留该版本目录。升级时先卸载旧登录注册，完成升级和检查后用新 CLI 重新安装。备份不恢复外部登录注册；恢复节点后单独核对 `startup status`，不要把历史标记当作运行状态。测试不在开发者本机安装自启项。

实现依据：[Apple launchd 配置](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html)、[Microsoft schtasks](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-create)。

## 自动回归

普通 workspace tests 覆盖快照写失败/SQLite FULL 的回滚、迁移中断的原子性、并发容量、项目轮转、同目录互斥、80 个并发任务的单 attempt 与连续事件、实际 Git worktree/diff/应用，以及备份身份/历史恢复、篡改拒绝和自启配置转义。

浏览器脚本使用两个隔离 TLS Node 和明确的 fixture，覆盖后续文件编辑仍核验原快照、接入预览、确认丢失后幂等登记、不修改策略、不调用模型与手机端向导。现有创建/审批/恢复/撤权/核验流程继续执行。这些不是新的物理两机或真实模型验收。
