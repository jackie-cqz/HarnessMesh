# Security

**English** | [简体中文](#安全)

HarnessMesh lets computers you pair start coding agents on each other and see their work. That is powerful, so please report problems privately.

**Status: early and experimental (0.0.x).** It has been tested on real Macs, and only partly on Linux and Windows. Use it on computers and networks you trust, and only pair computers you own.

## Reporting a vulnerability

Report it privately through GitHub: on this repository's **Security** tab, choose **Report a vulnerability**. Please do not open a public issue or pull request for a vulnerability.

Include what you can of:

- the version or commit, and the operating system of each computer involved;
- the steps to reproduce it, and what you expected to happen;
- what someone could do with it.

You will get a reply in the same advisory. Only the latest commit on `main` gets security fixes for now.

## What counts

Examples of what we want to hear about:

- a computer that is not paired, or whose pairing was revoked, reaching the daemon, an agent or a task;
- a paired computer running an agent, a task or a command without the approval that [docs/security.md](docs/security.md) says it needs, or outside the projects it was granted;
- keys, the pairing passcode or other secrets reaching paired computers, logs, events or the audit record;
- the panel or the owner API being usable from another origin or another computer;
- a relay reading or changing what passes through it.

Usually out of scope:

- what an agent you approved does inside the project you gave it;
- attacks that need control of your own account or of a computer you paired;
- what [docs/security.md](docs/security.md) already lists as not covered.

## How it is meant to be safe

[docs/security.md](docs/security.md) describes pairing, the certificates each computer pins, approvals, which networks it listens on, the relay and what is not verified yet.

---

## 安全

[English](#security) | **简体中文**

HarnessMesh 让你配对的电脑互相启动编程 Agent、查看对方的工作，权限很大，发现问题请私下报告。

**现状：早期实验阶段（0.0.x）。** 主要在 Mac 真机上测过，Linux 和 Windows 只测了一部分。请只在你信任的电脑和网络上使用，只配对你自己的电脑。

### 报告漏洞

请通过 GitHub 私下报告：在本仓库的 **Security** 页点 **Report a vulnerability**。请不要为漏洞开公开的 issue 或 PR。

尽量写上：

- 版本或提交号，以及每台相关电脑的系统；
- 复现步骤，以及你预期的结果；
- 别人能利用它做什么。

回复会在同一个报告里给出。目前只有 `main` 上的最新提交会修安全问题。

### 哪些算

希望收到的例如：

- 没配对或已撤销配对的电脑，能连到 daemon、Agent 或任务；
- 已配对的电脑不经 [docs/security.md](docs/security.md) 里说的审批，或在没授权的项目里，运行 Agent、任务或命令；
- Key、配对口令或其他机密进入了配对电脑、日志、事件或审计记录；
- 面板或 owner API 能被别的网页或别的电脑调用；
- 中继能读到或改动经过它的内容。

一般不算：

- 你批准的 Agent 在你给它的项目里做的事；
- 需要先控制你自己的账号或已配对电脑的攻击；
- [docs/security.md](docs/security.md) 里已经写明不覆盖的情况。

### 安全设计

[docs/security.md](docs/security.md) 说明了配对、每台电脑固定的证书、审批、在哪些网络上监听、中继，以及哪些还没验证。
