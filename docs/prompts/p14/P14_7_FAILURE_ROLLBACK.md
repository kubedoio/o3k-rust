# P14.7 — Failure, Unknown Outcomes, and Compensation

Implement the failure matrix across every prior phase. On timeout observe before
retrying; use bounded backoff and durable fencing. Compensate only resources
proven migration-owned, in reverse dependency order, and report blocked cleanup
explicitly. Test process kill, worker loss, duplicate requests, partial source
failure, partial destination failure, and post-cutover forward repair.
