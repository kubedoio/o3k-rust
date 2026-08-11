# ASR-015 Result Report

## Result

ASR-015 is **closed**. A protected KVM/libvirt run on `nkudo-vm1` proved that
one durable hard-reboot command survived loss of compute-agent epoch E1,
replayed terminal evidence under E2, converged without duplicate execution,
and left no owned residue or foreign-state change.

## Source and ownership

- Repository: `kubedoio/o3k-rust`
- Tested source SHA: `a2ddaa7b2275d226e83690a83db7b4c276024a22`
- Fail-before source SHA: `633f8cb49f225394968bc90c8b2124257f28ffad`
- Owning issue: #83
- Related closed issues: #87 and #575
- Product profile: `native-rust-testlab`
- Evidence tier: component real-host plus portable/process fencing proof
- Runtime code changed: yes
- Database/profile assumptions: SQLite; real `o3kd -> mTLS -> o3k-compute -> libvirt/KVM`
- Contracts/specs changed: no

## Corrected KVM finding

The original attempt incorrectly treated `/dev/kvm` visibility in a restricted
agent namespace as a host limitation. Host qualification on `nkudo-vm1`
confirmed:

- `/dev/kvm` is a character device with mode `0660`, owned by `root:kvm`;
- kernel `6.8.0-110-generic`;
- libvirt `10.0.0` and QEMU `8.2.2`;
- `qemu:///system` is reachable;
- `o3k-compute` is a member of `kvm` and `libvirt`;
- the `o3k-compute` account can read/write `/dev/kvm` and use the system
  libvirt URI.

The missing-device observation was a reporting error, not an O3K defect.

## Portable evidence and fail-before result

Existing portable coverage already proved stable agent identity, fresh epochs,
command/operation/resource binding, deterministic idempotency and fingerprint
identity, current-epoch replay, stale-epoch rejection, and observation sequence
fencing.

The first qualified real-host run on `633f8cb` then exposed a narrower defect:

```text
E2 terminal observation
-> operation Running -> Succeeded
-> E2 terminal operation update reached the reconciler
-> terminal-operation early return skipped command projection
-> command remained Accepted for more than 30 seconds
```

The provider adapter and reconciler were separate unordered event consumers.
The provider's command write was best-effort, while the recovery-authoritative
journal could not repair a missed write after the operation became terminal.
The fail-before run still cleaned successfully and preserved the canary.

## Focused correction

The correction is limited to durable agent evidence projection:

- the epoch-fenced journal authoritatively projects command acceptance and
  operation updates, including matching terminal replay after an observation
  already terminalized the operation;
- duplicate evidence performs idempotent durable repair, so a consumed
  in-memory watermark cannot permanently strand a failed store write;
- a same-agent fresh epoch re-anchors evidence even when replay sequence values
  overlap the prior epoch;
- a per-agent epoch lease makes current-epoch validation and durable projection
  one linearizable action, so E2 registration cannot complete while an E1
  projection is in flight;
- provider/resource identity and terminal-state compatibility are validated
  across both durable provider-reference namespaces before advancing the
  evidence fence;
- the provider adapter retains a registry-current, identity-checked backup
  projection for broadcast lag and rejects queued events from replaced epochs;
- terminal command state and established provider identities cannot regress or
  drift at the store boundary, and `UnknownOutcome` cannot regress to an
  in-flight command state.

No command is rebuilt, no mutation is automatically retried, and no reconnect
or provider execution semantics were broadened.

## Exact real-host reproduction

The final run used one real CirrOS server created through public OpenStack APIs.

- Server/resource: `a340cf83-1604-59d1-9a5f-7daa8749a9d0`
- Create operation: `813453cb-2cae-5b5d-9535-3b58c7f7f423`
- Hard-reboot operation: `ed4e630b-5991-59f8-a13e-518b3d28550b`
- Command: `97b12289-2197-53c9-a8ca-4085349ba828`
- Agent: `compute-agent`
- E1: `019ff28b-f772-7173-b599-64739fe8ccfb`
- E2: `019ff28c-a88d-7d30-a15c-d270e3ac33c5`
- Pre-crash command state: `accepted`
- Post-crash command state: `accepted`
- Post-reconnect command state: `succeeded`
- Terminal operation state: `succeeded`

The real non-root compute process was killed after provider execution but before
terminal evidence reached `o3kd`. Its health endpoint disappeared, no second
compute process existed, and the durable operation/command remained. The same
agent restarted from its journal, registered E2, and replayed the original
terminal evidence without SQL or manual repair.

## Epoch fencing and at-most-once result

No production debug API was added for an unsafe packet injection. The evidence
split is explicit:

- the real host proves stable `agent_id`, E1 loss, E2 registration, and E2
  recovery;
- same-source deterministic tests inject stale E1 after E2 and assert no
  command, operation, resource, provider reference, agent evidence fence, or
  observation evidence fence mutation;
- the provider backup projection separately proves a queued replaced-epoch
  event cannot write durable command truth.

Identity stayed unchanged:

- idempotency key:
  `hard-reboot:a340cf83-1604-59d1-9a5f-7daa8749a9d0:ed4e630b-5991-59f8-a13e-518b3d28550b`;
- payload fingerprint:
  `1498217f2bb626ffa454967f33cdbfbf4a065d7a3c02a81a6e9270f96e5eb9bd`;
- provider/domain name: `o3k-66a348162124007cff5b`;
- libvirt UUID before/after:
  `8902257f-9e13-4038-bc66-7cc0952bc30b`;
- provider resource count after recovery: `1`;
- effective hard-reboot execution count: `1`.

The reboot did not delete/recreate the domain and no second O3K server or
provider resource appeared.

## Lifecycle and cleanup

After reconnect recovery, public APIs successfully completed server show,
console log, stop, start, and delete. Before disposable teardown:

- Placement allocation count: `0`;
- non-terminal operation count: `0`;
- recoverable/non-terminal command count: `0`;
- server and libvirt domain: absent.

The independent leak verifier reported zero active/stale owned resources and no
inconsistencies. Final disposable cleanup removed the managed state root. The
foreign canary XML digest remained byte-identical:
`cc24140437ae576bb17dbc16ddc92411e6bf829dbe1ae32825d487671a45141d`.

## Evidence files

- Passing machine artifact:
  `docs/evidence/asr-015-reconnect-host-a2ddaa7.json`
- Fail-before machine artifact:
  `docs/evidence/asr-015-reconnect-host-633f8cb.json`
- Archived stale-epoch test results:
  `docs/evidence/asr-015-stale-epoch-tests-a2ddaa7.txt`
- Protected raw run tree:
  `target/real-host-workflow-artifacts/asr-015-a3d1caa/real-host-a2ddaa7/`

All tracked artifacts are redacted. They contain no tokens, passwords,
certificates, private keys, user-data, unrestricted command payloads, or host
connection secrets.
Bootstrap stdout was not retained because the disposable bootstrap command
emits generated ephemeral credentials for the caller to consume.

## Validation

Passed on the corrected source:

```text
python3 scripts/check-architecture-boundaries.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
```

Focused results include 45 reconciler tests, 110 compute-agent tests, 65 compute
tests, 30 store tests, the black-box agent mTLS test, both registration TLS
tests, the real libvirt lifecycle, and the independent leak/foreign-state
verifier.

## Closure decision

ASR-015 is `closed`: the exact real-host gate passed with a fresh agent epoch,
terminal durable convergence, stale-E1 fencing, one effective mutation, stable
provider identity, complete lifecycle cleanup, and unchanged foreign state.

## Next exact ASR item

The single next item in the current matrix is **ASR-016 — concurrent durable
operation/evidence monotonicity on the real host**. It is not started here.
