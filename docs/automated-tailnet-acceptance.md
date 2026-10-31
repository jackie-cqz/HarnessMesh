# Automated Tailnet Todo acceptance

The Mac coordinator and Windows owner-started worker automate one bounded round over existing HarnessMesh mTLS HTTP services. They do not use SSH, modify Tailscale/TUN, add a general remote shell, copy owner credentials, change task policies or expose LAN listeners.

## Fixed scenario

1. Copy the previously model-authored backend into `PROJECT/.acceptance/RUN_UUID/server.mjs`. Its `data.json` is separate from existing application data. Publish this copy using a fresh service ID, separate port and exact Windows peer.
2. Windows runs the reviewed SHA-256-pinned client checker and independent owner CRUD assertions.
3. Windows creates a uniquely marked item. Mac restarts only this round's backend; revision must increase and PID change. Windows confirms the item survived, then deletes only that item.
4. Optional repair: Mac injects an owner-specified, exact single-match health-field defect into the per-run copy. Windows confirms the broken field AND that the frozen checker now fails. Mac submits at most one local Pi repair (600 seconds, 16 MiB stdout), restarts the same service, and Windows reruns the unchanged checker and independent CRUD assertions.
5. Mac revokes the backend, confirms the owned process stopped, and Windows verifies access denial. A timeout or malformed reply is not accepted as proof of revocation.
6. Windows revokes its backend connection and returns its durable results. Mac requests worker finish, observes worker service removal, revokes its worker connection, and writes a report.

The worker accepts only ordered named stages plus a run-bound finish request. Requests contain no executable, path, argv, prompt, credentials or arbitrary URL. Stage/action UUIDs are durably reserved before side effects. Duplicate requests return their original reservation. Exclusive process locks prevent concurrent runners. A crash leaves its lock in place for owner inspection; it is not removed automatically. If the owner explicitly reopens a stale worker run, in-flight stages become blocked and are never replayed. After a lost acknowledgement the coordinator only queries the existing action UUID. Unknown local mutations are blocked rather than retried. A failed run cleans up access and preserves files/evidence; start a new run for another attempt. This is not an indefinitely reusable resident remote command service.

## One-time prerequisites

Both Nodes must already run, pair and trust each other. The Mac backend Agent/project and Windows checker project must already be registered. The reviewed DSH checker must print the fixed eight-check JSON summary, accept BASE_URL and exit nonzero on failed assertions. Hash pinning does not sandbox generated code; it still uses owner OS privileges. Checker subprocesses receive a minimal environment without model keys. Known running checker processes are terminated on timeout/worker shutdown; detached children created by a modified or malicious checker are not covered by this script-level process tracking. Node.js 20+ is required on both machines.

Mac directly owns backend restart, repair and revoke, so no remote restart grant or automatic task approval is added. The worker is published as an external loopback service scoped solely to the exact Mac peer and a bounded lease. Each round consumes existing managed-record capacity (currently 16 records per resource kind); disabled records are retained, not silently deleted. Use dedicated acceptance instances for repeated campaigns.

## Mac setup

Create a private owner JSON with absolute paths, for example:

```json
{
  "binary": "/absolute/HarnessMesh/target/debug/hmesh",
  "node_dir": "/absolute/HarnessMesh/.hmesh-v0/tailnet-darwin/node",
  "project_root": "/absolute/registered/project",
  "backend_file": "/absolute/registered/project/app-todo/server.mjs",
  "backend_port": 18103,
  "backend_agent_id": "EXISTING_MAC_AGENT_UUID",
  "project_id": "tailnet-demo",
  "windows_node_id": "PAIRED_WINDOWS_UUID",
  "checker_sha256": "REVIEWED_WINDOWS_CHECKER_SHA256",
  "lease_seconds": 1800,
  "fault_needle": "service: 'tailnet-todo'",
  "fault_replacement": "service: 'tailnet-todo-fault'"
}
```

For a model-free round:

```sh
node scripts/acceptance/mac.mjs prepare --config /absolute/mac-owner.json --out /absolute/new-run
node scripts/acceptance/kit.mjs --run-dir /absolute/new-run --out /absolute/new-worker-kit
node scripts/acceptance/mac.mjs run --run-dir /absolute/new-run --wait-worker-seconds 1200
```

To include exactly one real repair, pass `--allow-model` to BOTH prepare and run. Prepare only freezes the repair-enabled plan; run performs the billed call. The CLI/Daemon must support `agent verify --max-output-bytes` and `--timeout`. A 16 MiB stdout bound is not a model token or monetary budget.

Prepare freezes input identity/checker hashes, creates isolated files and does not call models. Transfer only the generated worker kit directory (or zip it); it contains no owner config, keys or tokens. Preserve `plan.json` bytes, because both ends compare its SHA-256. Keep both Nodes and coordinator running for the round. Readiness/network problems produce blocked results; the scripts do not alter system networking.

## Windows startup

From the received worker kit directory, use the registered Windows project root:

```powershell
pwsh -File .\start-worker.ps1 -Repo 'C:\path\HarnessMesh' -ProjectRoot 'C:\registered\tailnet-reverse'
```

Override `-NodeDir`, `-ProjectId`, `-Port` only for the intended existing instance/project. The helper verifies the pinned checker and writes a private owner config under `.hmesh-v0/acceptance-worker/RUN_UUID`. Keep the foreground process running; remaining steps/results require no manual copying. The same worker scripts can be distributed as the credential-free kit without updating the Windows repository first. Ctrl+C stops only this worker's resources. It also shuts down after coordinator finish or lease expiry. The helper itself does not change execution policy or request new project/model permissions.

## Reports and failure behavior

Mac writes `report.json` and `coordinator-state.json`; Windows writes `worker-state.json`. Reports contain manifest/checker/backend hashes, action IDs, service revisions, process changes, stage outcomes and known repair task/attempt IDs. They do not include owner tokens, private keys, raw model output or unrelated Todo contents. Failure may preserve uniquely marked data for investigation; no blanket data deletion is performed. Unresolved cleanup is reported as blocked, not passed. A service lease ending is not automatic renewal or permanent hosting.

No-model tests use fixtures that are explicitly not real Harness evidence. Same-machine TLS success does not establish Windows/Mac or Tailnet acceptance. Fixture repair tests exercise the coordinator with an explicit test double and model_submissions=0. The real cross-device automated round and real Pi repair must be collected separately. Previously user-supplied manual CRUD results remain separate historical evidence.

## Developer verification

```sh
cargo +stable build --locked -p hmesh-cli -p hmesh-fixture
node --test scripts/tailnet-config.test.mjs scripts/acceptance/protocol.test.mjs
node scripts/acceptance/local-smoke.mjs
node scripts/acceptance/local-smoke.mjs --fixture-repair
```

CI runs these checks on Windows, macOS and Linux; local configuration is not proof that remote CI passed. The scripts do not automate forced Tailnet outages, OS startup, power-loss durability or native MCP autonomous delegation.


## Real two-device result (2026-11-01)

Run `b17d75f3-aa32-4556-a1af-c3617a17e2e4` passed all 22 stages with a real Windows (`win32`) worker and Mac coordinator. Results returned through mTLS, with no manually pasted test summary. Baseline and post-repair pinned checker/independent CRUD passed; the seeded item survived a process/revision change. The original checker detected the injected defect before one real Pi/DeepSeek V4 Flash repair, task `793ad108-9c0f-4d69-9cf7-7d22ffb992de`, succeeded. The checker hash and original application remained unchanged. Revocation denied subsequent access, stopped the owned backend and removed the worker service/connections. The repair produced 266,846 stdout bytes within its 16 MiB /600 s budget.

See [real-device evidence](evidence/latest-automated-tailnet-acceptance.json). This does not extend the claim to native MCP autonomous delegation, forced Tailnet outages or power-loss durability. Earlier local fixture evidence remains separately labelled.
