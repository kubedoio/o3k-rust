# CirrOS TestLab walkthrough

After a real libvirt installation and a successful release gate, configure
the CLI from `examples/clouds.yaml`, replace the placeholder credentials, and
run the public-API workflow:

```sh
openstack token issue
openstack image create cirros --file cirros.qcow2 --disk-format qcow2 --container-format bare
openstack network create testlab-network
openstack subnet create --network testlab-network --subnet-range 192.0.2.0/24 testlab-subnet
openstack flavor create test.small --ram 512 --disk 10 --vcpus 1
openstack server create --image cirros --flavor test.small --network testlab-network cirros-1
openstack console log show cirros-1
```

Use `openstack server show`, `stop`, `start`, `reboot`, and `delete` to finish
the lifecycle. Record operation IDs and the cleanup report; do not publish
credentials, tokens, or unredacted logs.
