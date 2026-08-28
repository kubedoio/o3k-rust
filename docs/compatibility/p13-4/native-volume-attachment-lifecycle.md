# P13.4 native VolumeAttachment lifecycle evidence

Implementation exercised by the current provider gate:
`53c1ed1247eed2337375792692f7012e2ee071a8`.

The Nova-compatible attachment projection uses the canonical
`o3k_domain::VolumeAttachment` persisted by `StorageRepository`.  The native
attachment workflow durably records the operation and replayable agent command
before preparing the volume or mutating the compute provider.  Startup recovery
replays those commands and observes before retrying an uncertain operation.
The compatibility response is a projection; it is not a second attachment
authority.

The bounded provider contract was exercised with OpenTofu 1.12.6 and the
unmodified `terraform-provider-openstack` 3.4.0 binary, SHA-256
`2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc`, using
Gophercloud v2.8.0.  `tests/p13_4_provider_volume_attachment_smoke.sh` created
a native LVM-backed volume and Nova server, attached the volume, verified an
exact post-apply plan exit code of 0, then detached and destroyed all resources.

The native profile intentionally does not persist provider-local device paths
in canonical state.  The compatibility adapter returns the stable bounded
`/dev/vdb` value expected by the pinned Nova provider while the local provider
uses its own prepared device path internally.  Raw connection information is
not persisted or emitted.

This gate proves provider lifecycle convergence and canonical attachment
workflow integration.  A real guest-device visibility claim requires the
separate libvirt/QEMU host gate and is not implied by this artifact.
