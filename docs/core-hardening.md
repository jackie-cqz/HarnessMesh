# Task engine P0/P1 hardening

## Recovery and submission uncertainty

Interrupted RUNNING, CANCELLING and WAITING_INPUT tasks recover as UNKNOWN with the original attempt. They are not automatically reexecuted. Pending and queued tasks remain durable and require authorization at execution.

`hmesh task recover <request UUID>` loads the origin's original durable outbound reservation and resends that exact envelope to its original peer. It does not accept a replacement prompt, peer or task ID. Receiver deduplication returns the existing task; conflicting request reuse fails. Current trust and receiver policy continue to apply. An unknown local reservation fails explicitly. A transport timeout remains uncertain until recovery succeeds; do not generate a new request UUID to retry side effects.

## Execution and acceptance

Task execution status and artifact verification remain separate. Successful execution does not imply verified output. New verification records include task ID, attempt ID, expected JSON and configuration digest together with artifact hash and assertions. Historical records without these additive fields remain readable. Verification operates on current allowlisted files; it is not an immutable artifact snapshot or proof of which process produced the file.

## Cancellation and authorization

Existing cancellation requires confirmed process cleanup before CANCELLED, otherwise UNKNOWN. Terminal transitions reject competing terminal writes. Runtime now rechecks authorization every inspection tick (100 ms while executing) and before declaring success. Revocation stops the tracked process tree and records failure; unconfirmed cleanup remains UNKNOWN. Already-performed side effects cannot be undone. Queued authorization and mTLS ownership checks remain enforced.

## Events

Negative cursors are rejected. Ordered, byte-bounded pages and final-output draining remain unchanged. Normalized run events have a 16 MiB per-task storage budget, measured as persisted UTF-8 JSON plus conservative envelope allowance. Budget exhaustion stops execution rather than deleting history or wrapping sequence IDs. Internal terminal transitions remain writable. This is not a global database retention policy; historical tasks still need a future explicit archival policy.

## Agent lifecycle

Owner-only commands:

```sh
hmesh agent summary <agent UUID>
hmesh agent update <agent UUID> --config /absolute/agent.toml
hmesh agent disable <agent UUID>
```

Summary returns a digest rather than raw configuration or credential contents. Update preserves ID, project ID and canonical project root; a project change requires a separate registration. Update invalidates managed-run/authentication/streaming verification. Pending or executing tasks block configuration changes and disable, so the configuration cannot change underneath a task. Resolve these tasks explicitly first. Disable preserves task history and removes real-run availability. Reenable through update and explicit model verification. No destructive deletion or automatic policy expansion is provided. Credential file content changes are not captured by the configuration digest.

## Validation boundaries

Model-free tests simulate a lost admission response, reopen the origin reservation, and replay to the receiver without creating another attempt. They cover active revocation, blocked agent updates, preserved history and cursor boundaries, alongside existing cancellation, timeout, cleanup, restart, deduplication and pagination tests. Real cross-device outage recovery, approval expiration, immutable artifact storage, global event archival and detached Unix child cleanup remain separate future gates. These changes do not claim complete V0.1/V0.2 acceptance.

2026-10-31 local validation: 65 Rust tests and four Node tests passed; fmt, workspace Clippy with warnings denied and diff whitespace checks passed. No real model calls.
