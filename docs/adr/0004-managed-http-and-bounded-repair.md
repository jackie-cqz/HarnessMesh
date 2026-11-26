# ADR 0004: Daemon-owned HTTP leases and finite repair runs

Status: accepted for the two-node SSH checkpoint, 2026-10-29.

The Todo scripts established real Pi/DSH application behavior but required an external controller to own processes and forwards. A process existing did not establish SSH connectivity, HTTP health, or API correctness. Reusing these scripts as the public management API would also allow process ownership and authorization to drift apart.

The Daemon now stores independent service, tunnel and connection leases, owns only newly spawned process trees, and reports process/tunnel/HTTP states separately. Owners select exact project roots, executables, peers and loopback ports; remote clients receive a bounded mTLS HTTP bridge to a named service. Requests are buffered up to 256 KiB so access can be rechecked before data release. No cookies, bearer headers, arbitrary TCP targets or streaming protocols are forwarded. This uses existing transport dependencies and is an explicitly narrower checkpoint than the planned generic TCP Gateway.

Automatic recovery is limited to one initial process start plus three retries, with 2/4/8-second backoff. Expiry/revocation stops the resource; restarting the Daemon disables old leases instead of assuming ownership from a stored PID. Windows Job Objects and Unix process groups are reused from the task runtime. Unix SIGKILL orphan handling and OS startup installation need a separate design and acceptance.

Real durable writes exposed an overly short fixed HTTP deadline: Linux fsync samples reached 8.3 and 10.2 seconds. The owner now sets a total operation budget of 1..30 seconds (default 10), covering connection/headers/body without a hidden one-second connection cutoff. Mesh adds five seconds, and the demo clients use 45 seconds. Timeout leaves a mutation outcome potentially unknown; it never authorizes replay. A delayed-write regression checks a one-second failure, a three-second success and exactly one server-side write per call.

IntegrationRun freezes the complete owner contract and a deterministic GET/JSON plan. Failed assertions can submit at most two ordinary Mesh tasks to a fixed backend agent/project. Their UUIDs are durable before sending. Existing execution grants and approval remain authoritative. A successful task permits only a separately delegated restart of the fixed service configuration, followed by the same tests. Response bodies are replaced by hashes, lengths and assertion outcomes in reports. Network ambiguity, non-successful tasks, budget exhaustion and interrupted runs stop for inspection; they do not generate replacement task IDs.

This keeps policy changes, owner approval, arbitrary process configuration and contract editing out of model tools. The actual smoke uses a new pair of Nodes and independent ports/data so fault injection and cleanup cannot take over the existing Todo application.
