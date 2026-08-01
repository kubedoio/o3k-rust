# SPEC-0008 — Operation journal and reconciliation

Status: Implemented initial subset

`o3k-reconciler::OperationJournal` is the in-process recovery boundary for
compute mutations. It writes a resource intent and pending operation through
`DurableStore::insert_resource_and_operation` before calling a provider.

The resource's desired-state JSON is the bounded replay intent. A journal can
therefore be reconstructed after process restart using the operation ID. The
provider operation ID and provider resource reference are persisted as soon
as they are known.

Create completion is observation-gated. If a provider reports an accepted or
running operation while its resource is still creating, the journal persists
the provider reference and a `BUILD` observation but keeps the operation
non-terminal. Only an observed running resource may transition the create
operation to success and the durable resource to `active`.

Unknown outcomes are never replayed immediately. The next reconciliation pass
first observes the provider operation and, when necessary, the provider
resource. Retryable failures use a bounded deterministic attempt budget;
exhaustion becomes a visible failed operation. Journal events expose intent,
provider start, retry, unknown observation, success, and failure transitions
without provider payloads.

This alpha implementation is single-process and does not claim distributed
leases, wall-clock scheduling, or durable event analytics. Those are follow-up
concerns before multi-process deployment.
