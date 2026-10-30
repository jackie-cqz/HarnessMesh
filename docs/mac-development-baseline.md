# Mac development and acceptance status

## Developer commands

Use `sh scripts/start-tailnet.sh` on Mac. It finds Node on PATH or the managed Pi runtime, reports missing Node clearly, and delegates to the same startup script. Rust stable >=1.90 remains required. No installation or model call happens implicitly.

`hmesh doctor --peer <peer UUID>` reports local identity, Tailnet readiness, local owner API errors, peer trust/connectivity and remote registry errors. A readable empty registry is not proof of runnable Agents or task permission. RPC errors retain their structured cause; do not infer certificate or authorization failure from a TCP timeout.

Task submission accepts an exact peer display name and Agent name as well as UUIDs:

```sh
hmesh task send windows-dsh-pro --peer windows-tailnet --project tailnet-reverse --prompt '...'
hmesh task list --pending
hmesh task verification <task UUID>
```

Names must resolve uniquely among visible entries. UUIDs remain the stable reference for automation. Execution status and deterministic artifact verification are separate commands and outcomes; SUCCEEDED alone does not prove an artifact passes.

## Evidence boundaries

| Gate | Status | Evidence |
|---|---|---|
| Windows to Mac model execution and artifact checks | Passed | latest-tailnet-mac.json |
| Mac to Windows model execution and artifact checks | Passed | latest-tailnet-reverse.json; latest-tailnet-reverse-mac.json |
| Reverse duplicate submission and cursor suffix | Passed | latest-tailnet-reverse-reliability.json |
| Real origin restart recovery | Blocked during earlier network failure; not revalidated | latest-tailnet-reverse-reliability.json |
| Model-free terminal replay and restart | Automated regression | crates/hmesh-cli/tests/two_nodes.rs |
| Cancellation, timeout and child cleanup | Model-free fixture tests | test-support/hmesh-fixture/tests/executor.rs |
| Tailnet managed HTTP two-device integration | Pending | Local backend health and earlier mTLS registry query do not prove forwarding |
| Reverse execution-time network interruption | Pending | No evidence collected |
| Complete V0.1/V0.2 | Incomplete | Do not infer from individual checkpoints |

Model-free regression checks do not replace real model or cross-device acceptance. Test fixtures never count as real Harness runs. Private credentials and local network configuration stay outside version control.

## Local validation

2026-10-30 on Apple Silicon / Rust stable 1.99.1: fmt, workspace Clippy with warnings denied, 60 Rust tests and 4 Node configuration tests passed. Shell syntax/help and diff whitespace checks passed. These changes do not call models or change system network settings.
