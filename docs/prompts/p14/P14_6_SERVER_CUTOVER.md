# P14.6 — Server Adoption and Explicit Cutover

Implement only cold server creation after dependency validation and the durable
cutover commit. Require source quiescence, fresh authorization, no unresolved
unknown outcomes, and persisted ownership before success. Cutover is one-way;
source deletion is not automatic. Test restart around the commit boundary,
replay, source/destination divergence, and cross-scope denial.
