# P14.8 — OpenTofu Handoff and Final NO-OP

Implement only the bounded handoff needed for the existing P13 OpenTofu
profile. Refresh/import the migrated canonical resources and prove the final
plan is an exact NO-OP without broadening P13. Test stable IDs, ownership,
drift visibility, failed handoff, cross-project state, and no duplicate create.
