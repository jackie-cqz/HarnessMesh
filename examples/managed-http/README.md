# Managed HTTP / SSH acceptance

`contract-check.mjs` is the actual Windows DSH-authored client used against the leased Windows → Linux HTTP endpoint. It verifies health fields and creates/completes/queries a unique Todo. It uses Node built-ins and a 45-second request budget; assertions exit nonzero on failure.

```powershell
node .\examples\managed-http\contract-check.mjs http://127.0.0.1:18081
```

Run this only against an authorized test instance: it writes test Todos. The endpoint must come from an active `hmesh service connect` lease; the completed acceptance instance has been revoked and stopped.

The exported SHA-256 is `78178429a83e8b9f7b283ebc4512fadc4c0aa0dcf9cde4dbcaf5153b34d23c57`, identical to the final script that failed before the Pi backend repair and passed after it. DSH task `482afed1-e859-4c42-93f0-5eac465a6b8e` authored it, then task `55f15e7a-2d63-4cc6-b0c6-11907cbb6389` changed only the timeout from 10 to 45 seconds. The separate frontend proxy received the same budget-only change; all business assertions and API contract bytes remained fixed.

See [owner configuration and limits](../../docs/managed-services.md), and the [real acceptance report](../../docs/evidence/latest-managed-services.json). The opt-in controller script that ran it was removed in #6. The first network timeout and early CRUD deadline failures are preserved in the report.
