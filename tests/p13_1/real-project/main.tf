terraform {
  required_version = "= 1.12.6"

  required_providers {
    openstack = {
      source  = "terraform-provider-openstack/openstack"
      version = "= 3.4.0"
    }
  }
}

provider "openstack" {
  auth_url    = var.auth_url
  user_name   = var.user_name
  password    = var.password
  tenant_id   = var.project_id
  region      = var.region
  insecure    = var.insecure
  max_retries = 0
}

data "openstack_images_image_v2" "probe" {
  name = var.image_name
}

data "openstack_compute_flavor_v2" "probe" {
  name = var.flavor_name
}

output "image_id" {
  value = data.openstack_images_image_v2.probe.id
}

output "flavor_id" {
  value = data.openstack_compute_flavor_v2.probe.id
}
