# Compatibility matrix

This matrix separates implementation from verified evidence. “Contract” means
the repository tests the shape/semantics locally; “real TestLab” requires the
artifacts consumed by `packaging/release-gate.sh`.

| Capability | Contract/fake | Real libvirt alpha |
|---|---:|---:|
| Keystone token issuance and project scope | verified | pending |
| Glance image create/upload/delete | verified | pending |
| Neutron flat network/subnet/port lifecycle | verified | pending |
| Placement local inventory/allocation ledger | verified | public HTTP API pending |
| Nova flavor list/show | verified | failed: protected GET returned 405 |
| Nova keypair import/list/show/delete | verified | verified |
| Nova server create/show/list | verified | pending |
| Start/stop/reboot/delete | verified | pending |
| Config-drive generation | verified | pending |
| DHCP fixed-IP delivery | unit verified | pending |
| Console log retrieval | bounded/API verified | pending |
| Control-plane restart/reconciliation | fake TestLab verified | pending |
| Compute-agent restart/reconciliation | protocol verified | pending |
| Real KVM/libvirt guest boot | not applicable | pending |

The real column must not be changed to verified without a clean-host artifact,
real E2E output, recovery evidence, and benchmark metadata. Unsupported
features (HA, Cinder, floating IPs, security groups, live migration, and
CellHV-primary operation) remain explicitly out of scope.
