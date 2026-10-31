# Lifecycle and real-device recovery

The 2026-11-01 Windows/Mac run passed origin restart recovery, worker crash uncertainty and native Pi MCP delegation. These are separate from the earlier Todo repair acceptance and model-free local fixtures. [Real-device evidence](evidence/latest-real-recovery-native-mcp.json) records the original task/attempt IDs, native tool names, parent-child binding and independent Windows artifact hashes. [Backend crash evidence](evidence/latest-managed-backend-crash.json) records a separate isolated Mac backend crash, one supervised restart, preserved data and confirmed process cleanup. No forced Tailnet/TUN outage or power loss was injected.

## Pending approvals

Schema 5 adds lifecycle storage without replacing historical task records. Existing Node configuration defaults to:

```toml
[security]
pending_approval_ttl_seconds = 86400
```

The accepted range is 1–604800 seconds. Restart the Daemon after changing this Node setting. Age is measured from task creation; only `PENDING_APPROVAL` tasks expire. A one-second background sweep durably transitions overdue tasks to `EXPIRED`, appends their status event and records an audit entry. Approval also checks age inside the same transaction that would queue the task, so an overdue task cannot start while waiting for the sweep. Expiry creates no attempt. Reusing its original request ID returns the expired task; an intentional new run needs a new request ID. This setting does not interrupt already queued or running tasks.

## Event archival and Agent retirement

Owner commands use the existing authenticated local interface:

```sh
hmesh --data-dir NODE task archive TERMINAL_TASK_UUID
hmesh --data-dir NODE agent retire AGENT_UUID
hmesh --data-dir NODE agent retired
hmesh --data-dir NODE agent restore AGENT_UUID
```

`task archive` atomically moves the terminal task's hot events into a separate table in the same SQLite database. Reads merge both tables, preserving event bodies, sequence numbers, pagination and later verification events. Repeating archival moves only newly added events. This is logical archival: it does not compress history, reduce database file size, export an archive or implement retention deletion. The cumulative event budget still includes archived events. Running-task archival is rejected.

Retirement hides an Agent from active discovery and refuses new executions and model verification. Outstanding pending/queued/running/cancelling/waiting-input tasks must first be resolved. Task records, artifacts, config and credential references remain available to owner history operations. Restoration clears managed-run/authentication verification and requires explicit model verification before it can advertise verified availability. It does not reinstate any removed project grants. Existing request deduplication and historical artifact verification still work after retirement.

The API additions are owner-only: `GET /local/agents/retired`, `POST /local/agents/{id}/retire`, `POST /local/agents/{id}/restore`, and `POST /local/tasks/{id}/archive`. Peer access cannot perform these operations.

## Unix process cleanup

Runtime and managed-service supervision periodically record descendants of the owned process using kernel parent identities and PID/start-time pairs. An observed child remains tracked after leaving the original process group. Cleanup signals only matching observed identities; it never searches by harness/process name. Linux uses pidfds and fails cleanup if pinned signaling is unavailable. macOS rechecks kernel birth time immediately before signaling. Its check and signal are best effort and cannot remove every PID reuse race.

Cancellation reports `UNKNOWN` when stopping cannot be confirmed. A managed service with unconfirmed cleanup reports `failed_needs_input`/`unknown` and disables automatic restart. Tests explicitly create a child in a new process group, wait until it is observed, cancel, and confirm its heartbeat stops. Windows retains Job Object tracking.

This is not OS containment. A fast double fork that escapes before observation, descendants launched while no tracked parent is visible, daemon hard crashes and OS power loss remain limitations. The current tests do not establish cleanup of arbitrary detached children. Stronger guarantees require OS containment beyond periodic descendant tracking.

## Repeat the bounded two-device run

Build CLI and MCP binaries first. Both Nodes must already be running, paired, and authorized for the exact Windows Agent/project. Pi 1.4.0, a usable existing Pi credential reference and native MCP support are required. A fresh run performs one real Windows recovery task and one real Pi parent/Windows child delegation, each bounded to 600 seconds and 16 MiB stdout. Output bytes are not a token or spending budget.

Prepare a private owner JSON with absolute paths and existing Windows IDs:

```json
{
  "binary": "/absolute/HarnessMesh/target/debug/hmesh",
  "node_dir": "/absolute/HarnessMesh/.hmesh-v0/tailnet-darwin/node",
  "project_root": "/absolute/registered/mac-project",
  "pi_config": "/absolute/private/pi.toml",
  "mcp_binary": "/absolute/HarnessMesh/target/debug/hmesh-mcp",
  "windows_node_id": "PAIRED_WINDOWS_UUID",
  "windows_agent_id": "REGISTERED_WINDOWS_AGENT_UUID",
  "windows_project_id": "tailnet-reverse"
}
```

```sh
node scripts/reliability/kit.mjs --config /absolute/owner.json --out /absolute/new-run
# Transfer only new-run/windows-kit to Windows, preserving manifest.json bytes.
node scripts/reliability/mac.mjs --config /absolute/new-run/mac-owner.json
```

From the received Windows kit directory:

```powershell
pwsh -File .\start-windows.ps1 -Repo 'C:\path\HarnessMesh' -ProjectRoot 'C:\registered\tailnet-reverse'
```

The Windows owner-started helper approves only the two exact manifest task IDs, origin, Agent, project, prompt hashes and bounded limits. It does not change policy. Its loopback guardian is shared only with the exact Mac peer. One intentional exit code triggers one restart of that same helper; a durable mutation reservation becomes blocked after restart and duplicate requests cannot replay it. This helper behavior does not claim exactly-once execution for arbitrary harness side effects.

The Mac driver normally stops its existing origin Daemon, makes a private SQLite backup and restarts the same identity. Run this only when unrelated local work can be interrupted. It recovers the original remote task and requires its original attempt. A scoped temporary Pi Agent then uses actual MCP discovery, submit and get tools with a one-child quota. Windows independently checks the child's JSON; Mac checks parent receipt and tool events. The helper revokes its service on finish/expiry; the controller revokes its connections and records cleanup. Retire the temporary MCP Agent after collecting evidence.

Never reopen a failed/uncertain manifest to repeat side effects; create a new run. Per-run files and evidence remain for investigation. Retained managed records consume bounded capacity, so repeated campaigns should use dedicated instances. The worker kit contains prompts and public IDs, but no credentials, Mac owner config or Pi config. Keep owner JSON, private backups and full local journals outside Git.

Model-free regression uses `node scripts/reliability/local-smoke.mjs` after building the CLI and fixture. CI runs it on macOS, Linux and Windows. A local smoke result does not replace real-device evidence.
