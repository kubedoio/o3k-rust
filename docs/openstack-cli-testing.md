# OpenStack CLI libvirt workflow

Run the end-to-end workflow only against an explicitly provisioned TestLab:

```sh
O3K_TESTLAB_PROFILE=libvirt \
OS_AUTH_URL=https://control.example/v3 \
OS_USERNAME=admin OS_PASSWORD='...' OS_PROJECT_NAME=bootstrap-project \
  bash tests/openstack-cli-libvirt.sh
```

The script creates an isolated `clouds.yaml`, uses only `openstack` commands,
exercises server list/show and lifecycle actions, keeps console output
bounded, and writes a machine-readable result containing only created resource
IDs. Missing CLI, credentials, or endpoint access produces `status: skipped`,
never a false pass. Do not upload `clouds.yaml`, response bodies, or error
files without redaction.
