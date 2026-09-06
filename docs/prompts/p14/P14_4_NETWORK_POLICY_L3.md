# P14.4 — Network, Policy, L3, and Public Address Translation

Implement only translation of network, subnet, port, security groups/rules,
router, router interface, and floating/public IP into canonical O3K resources.
Use dependency order and canonical authorization. Never reuse provider IDs as
authority. Test policy isolation, route/address conflicts, partial rollback,
foreign-state protection, and unknown external outcomes.
