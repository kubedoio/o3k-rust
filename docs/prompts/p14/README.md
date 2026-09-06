# P14 — OpenStack Adoption and Migration v1

This prompt set is the implementation contract for issue #855. Execute only
in order, from fresh protected `origin/main`, after the preceding slice is
merged and verified. P14.0 creates the architecture and contract boundary;
later prompts must not broaden it.

Profile: `p14-openstack-cold-migration-v1`. O3K owns canonical identity,
authorization, durable intent, destination resources, operations, audit, and
reconciliation. The declared OpenStack source is read authority until explicit
cutover; it is not an execution provider or a second O3K database.

Order: P14.1 source discovery; P14.2 manifest; P14.3 image/public key;
P14.4 network/policy/L3; P14.5 volume data; P14.6 server/cutover; P14.7
failure/rollback; P14.8 OpenTofu handoff; P14.9 real two-cloud evidence.
