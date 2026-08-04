# Goal Part 2/3 — Protected Runner and Real Execution

Goal: Execute and Prove the Real Gazpacho Cinder Service-Testbed Profile.
This is file 2 of 3: `02-protected-runner-and-execution.md` (sections E–M). See `01-goal-and-audit.md` (A–D) and `03-evidence-closure.md` (N–T).

## E. Phase 3 — Fix the protected runner architecture

Create `.github/workflows/real-cinder-testbed.yml`, manual dispatch only, runs only on `self-hosted`, `linux`, `x64`, `kvm`, `libvirt`, `o3k-testlab`. Requirements:

- no execution from untrusted fork pull requests;
- one serialized concurrency group;
- exact source-commit binding;
- pre-run foreign-state inventory;
- pre-run stale O3K-resource check;
- artifacts uploaded on success and failure;
- generated secrets masked immediately;
- no credentials committed or printed;
- bounded timeouts for every phase;
- cleanup always executes;
- post-run leak and foreign-state verification;
- machine-readable aggregate result.

The existing mock workflow must remain separate and clearly named as a component test.

## F. Phase 4 — Make runner resources disposable and ownership-safe

Do not use fixed shared identifiers (`o3k-vg`, `cinder` database user, `cinder` RabbitMQ user, fixed state directories, unqualified loop files). Derive disposable names from the protected workflow run ID:

```text
o3k-cinder-<run-id>
o3k-vg-<run-id>
o3k-cinder-db-<run-id>
o3k-cinder-rabbit-<run-id>
```

Generate ephemeral passwords for O3K bootstrap user, token signing key, Cinder service user, MariaDB, RabbitMQ. Never use repository-literal passwords as real protected-run credentials.

Before mutation record: existing volume groups, loop devices, logical volumes, iSCSI sessions, libvirt domains, bridges and TAPs, database users/databases, RabbitMQ users/vhosts, relevant configuration hashes.

Cleanup must: detach the guest volume; terminate/delete Cinder attachments; delete test volumes; remove run iSCSI sessions; remove test libvirt domains; remove run-owned LVs, VG and PV; detach the loop device; remove the test database and user; remove the RabbitMQ user/vhost; restore or remove run-owned Cinder configuration; stop only run-owned services/processes; remove run-owned state files; preserve all foreign state.

A warning about remaining resources is a failure, not a pass.

## G. Phase 5 — Start the actual O3K compute path

Reuse the repository's existing disposable protected TestLab bootstrap and certificate enrollment where practical. Do not create a second incompatible agent-bootstrap mechanism.

Build the compute binary with the real libvirt implementation enabled. Verify the exact executable name and path. The package is currently named `o3k-compute-bin`; do not assume the output is `target/debug/o3k-compute`.

Start and verify `o3kd` and `o3k-compute-bin` with libvirt support. Require evidence for: mTLS registration; agent identity; agent epoch; heartbeat; advertised compute capabilities; advertised block-device connector capabilities; selected host; libvirt `qemu:///system` connectivity; actual iSCSI initiator identity; actual management IP used in the connector.

Do not hardcode `host: compute-1`, `ip: 10.0.0.5`, `initiator: iqn.1993-08.org.debian:01:o3k`. The connector must come from `CollectConnector` through the real compute-agent path.

## H. Phase 6 — Start real Cinder and prove Identity compatibility

Provision only the required external-service dependencies: MariaDB, RabbitMQ, memcached when required, cinder-api, cinder-scheduler, cinder-volume, LVM backend, supported iSCSI target.

Configure Cinder to authenticate using `project_name = service`, `username = cinder`. Do not substitute the admin project for the Cinder service project.

Verify:

1. Cinder service-user password authentication succeeds through O3K.
2. The returned token contains the expected service project and roles.
3. `GET /v3/auth/tokens` succeeds for the Cinder middleware path.
4. `HEAD /v3/auth/tokens` succeeds.
5. Invalid and expired tokens fail closed.
6. The external block-storage endpoint is present in the catalog.
7. Unsupported services are absent.
8. Cinder API, scheduler, and volume services report healthy/up.
9. The LVM backend is enabled and usable.

Use the service type and endpoint form expected by the pinned Gazpacho OpenStack client and Cinder middleware. Do not assume the catalog display name, historical `volumev3` alias, and standardized service type are interchangeable.

## I. Phase 7 — Create a real O3K server through public APIs

Using standard OpenStack CLI and public O3K APIs only:

1. authenticate as the test user;
2. upload a pinned CirrOS image;
3. create a network;
4. create a subnet;
5. create a flavor;
6. create or import a disposable keypair;
7. create a server;
8. wait for ACTIVE;
9. verify the selected compute host;
10. verify a real libvirt domain exists;
11. verify console output contains a real guest boot marker.

Do not use a fabricated server record solely to exercise volume attachments. Record: O3K server UUID; libvirt domain UUID/name; project ID; selected host; fixed IP; image ID; flavor ID.

## J. Phase 8 — Execute the real volume attachment through Nova

Create a real Cinder volume using the standard OpenStack CLI. Wait for `volume status = available`. Attach it using the public Nova path, preferably `openstack server add volume <server> <volume>`. Do not call Cinder attachment endpoints directly from the runner to simulate O3K orchestration.

Expected internal sequence:

```text
Nova os-volume_attachments
→ AttachmentOrchestrator
→ Cinder attachment create
→ CollectConnector through o3k-compute
→ Cinder attachment update
→ connection_info returned
→ AttachDisk through o3k-compute
→ real iSCSI login
→ libvirt disk hotplug
→ Cinder attachment complete
→ final attached state
```

Verify each durable phase. The workflow must fail when any phase is missing or skipped.

## K. Phase 9 — Verify the attached device

Cinder: volume status reflects attachment; attachment record exists; attachment host and connector match the actual compute host; attachment reaches the expected completed state.

O3K control plane: durable attachment record exists; phase is `attached`; connection information is not stored in plaintext; server and project identities are correct.

Compute host: an O3K-owned iSCSI session exists; the expected block device exists; libvirt XML contains the attached disk; volume and attachment ownership metadata are present; no unrelated disk is used.

Guest: use a bounded non-secret method to prove the running guest sees the new block device (e.g. `lsblk` over an authorized disposable SSH key; a bounded serial-console command/result; another explicit mechanism accepted in the repository specification). Do not format or mount the device unless the test requires it. Do not upload private keys.

## L. Phase 10 — Detach and clean up through public APIs

Detach using the standard Nova/OpenStack CLI path. Verify:

```text
Nova attachment enters detach flow
→ compute device is detached
→ libvirt disk disappears
→ iSCSI session created by the run is removed
→ Cinder attachment is terminated/deleted
→ volume returns to available
```

Then delete: volume; server; keypair; flavor; subnet; network; image.

Verify no run-owned state remains at: O3K database; Cinder database; libvirt; LVM; iSCSI; loop devices; network; filesystem; process level.

## M. Phase 11 — Run focused failure and restart cases

Before closing #421, execute at least:

1. `o3kd` restart after Cinder attachment creation but before compute attach.
2. `o3k-compute` restart after host attachment but before Cinder completion.
3. Cinder API timeout after an attachment mutation.
4. duplicate Nova attach request.
5. repeated detach.
6. unsupported connector type.
7. malformed or incomplete connection information.
8. compute attach succeeds but Cinder completion fails.

For each case: preserve durable phase; observe before retry; avoid duplicate Cinder attachments; avoid duplicate libvirt disks; avoid leaked iSCSI sessions; converge to attached or cleanly detached/error state; preserve foreign state.

Do not expand this into the complete general #87 failure matrix.
