# Integration A — Native Resource Runtime Inventory

This inventory is the implementation checklist for the unified native resource
runtime. Every advertised collection must terminate in a bounded authority and
use the shared application page contract.

| Resource | Native collection | Discovery | List/show | Mutations/actions | Relationships | Operation | Bounded source |
|---|---|---|---|---|---|---|---|
| compute:server | compute/servers | manifest + live readiness | generic resource application | create/update/delete, Start/Stop/Reboot | generic relationship port | canonical operation journal | server page reader/store |
| compute:flavor | flavors (when advertised) | manifest + live readiness | generic resource application | declared lifecycle only | generic relationship port | canonical operation journal | bounded flavor authority required |
| image:image | images (when advertised) | manifest + live readiness | generic resource application | declared lifecycle | generic relationship port | canonical operation journal | image service page query |
| network:network | networks | manifest + live readiness | generic resource application | create/update/delete | generic relationship port | canonical operation journal | resource page query |
| network:address_realm | address-realms | manifest + live readiness | generic resource application | declared lifecycle | generic relationship port | canonical operation journal | address-realm page reader/store |
| network:subnet | subnets | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:port | ports | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:security_group | security-groups | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:security_group_rule | security-group-rules | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:router | routers | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:router_interface | router-interfaces | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | resource page query |
| network:floating_ip | floating-ips (when advertised) | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | bounded allocator/store query required |
| volume:volume | volumes | manifest + live readiness | generic resource application | create/delete | generic relationship port | canonical operation journal | volume page reader/store |
| volume:volume_attachment | attachments (when advertised) | manifest + live readiness | generic resource application | declared lifecycle only | generic relationship port | canonical operation journal | bounded attachment store query required |

Dedicated read handlers are intentionally not mounted by the production native
router; the generic handler is the single HTTP entry point for collection
pagination, so sorting, truncation, cursor creation, and `has_more` calculation
remain below HTTP.

