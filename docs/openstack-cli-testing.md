# OpenStack CLI libvirt workflow

Run the end-to-end workflow only against an explicitly provisioned TestLab:

```sh
O3K_TESTLAB_PROFILE=libvirt \
O3K_TESTLAB_IMAGE_PATH=/path/to/cirros.img \
OS_AUTH_URL=https://control.example/v3 \
OS_USERNAME=admin OS_PASSWORD='...' OS_PROJECT_NAME=admin \
  bash tests/openstack-cli-libvirt.sh
```

The script requires a local guest image, uploads it with the public image API,
waits for server lifecycle transitions, polls bounded console output, and
writes a machine-readable result containing only created resource IDs. Missing
CLI, credentials, image input, or endpoint access produces `status: skipped`,
never a false pass. Do not upload `clouds.yaml`, response bodies, or error
files without redaction.
