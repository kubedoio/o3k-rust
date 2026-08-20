use super::*;

impl super::LinuxP11FabricBackend {
    pub(crate) fn ensure_geneve(
        &mut self,
        plan: &NamespacedRoutedFabricPlan,
    ) -> Result<(), LinuxP11Error> {
        if self.plans.values().any(|other| {
            other.realm_id != plan.realm_id
                && other.encapsulation.fabric_domain_id == plan.encapsulation.fabric_domain_id
                && other.encapsulation.provider_segment_id == plan.encapsulation.provider_segment_id
        }) {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        let Some(current) = self.state.realms.get(&plan.realm_id).cloned() else {
            return Err(LinuxP11Error::CorruptState);
        };
        let mut desired = BTreeMap::new();
        for peer in &plan.peers {
            if !plan
                .routes
                .iter()
                .any(|route| route.target_host == peer.host_id)
            {
                continue;
            }
            let interface = geneve_name(plan.realm_id, &peer.host_id);
            let bridge = geneve_bridge_name(plan.realm_id, &peer.host_id);
            let realm_veth = geneve_realm_veth_name(plan.realm_id, &peer.host_id);
            let fabric_veth = geneve_fabric_veth_name(plan.realm_id, &peer.host_id);
            desired.insert(
                peer.host_id.clone(),
                GeneveOwnership {
                    target_host: peer.host_id.clone(),
                    interface,
                    remote_transport_ip: peer.fabric_transport_ip,
                    vni: plan.encapsulation.provider_segment_id,
                    binding_generation: plan.encapsulation.binding_generation,
                    local_tunnel_mac: tunnel_mac(plan.realm_id, &plan.local_host),
                    remote_tunnel_mac: tunnel_mac(plan.realm_id, &peer.host_id),
                    bridge,
                    realm_veth,
                    fabric_veth,
                    realized: current
                        .geneve
                        .get(&peer.host_id)
                        .is_some_and(|existing| existing.realized),
                },
            );
        }
        for (target_host, old) in &current.geneve {
            if desired.contains_key(target_host) {
                continue;
            }
            self.remove_geneve_attachment(old, &current.namespace)?;
            let (exists, output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &old.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if exists {
                if !geneve_link_matches(&output, old, self.config.geneve_port) {
                    return Err(LinuxP11Error::ForeignState);
                }
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &self.config.fabric_namespace,
                            "ip",
                            "link",
                            "del",
                            &old.interface,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
        }
        for (target_host, wanted) in &mut desired {
            if let Some(existing) = current.geneve.get(target_host)
                && (existing.target_host != wanted.target_host
                    || existing.interface != wanted.interface
                    || existing.remote_transport_ip != wanted.remote_transport_ip
                    || existing.vni != wanted.vni
                    || existing.binding_generation != wanted.binding_generation
                    || existing.local_tunnel_mac != wanted.local_tunnel_mac
                    || existing.remote_tunnel_mac != wanted.remote_tunnel_mac
                    || existing.bridge != wanted.bridge
                    || existing.realm_veth != wanted.realm_veth
                    || existing.fabric_veth != wanted.fabric_veth)
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            let (exists, output) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &wanted.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if exists && !geneve_link_matches(&output, wanted, self.config.geneve_port) {
                return Err(LinuxP11Error::ForeignState);
            }
            if !exists {
                wanted.realized = false;
            }
        }
        let mut next = current.clone();
        next.geneve = desired.clone();
        next.attachments = desired
            .values()
            .map(|geneve| {
                (
                    geneve.target_host.clone(),
                    FabricAttachmentOwnership {
                        target_host: geneve.target_host.clone(),
                        bridge: geneve.bridge.clone(),
                        realm_veth: geneve.realm_veth.clone(),
                        fabric_veth: geneve.fabric_veth.clone(),
                        local_tunnel_mac: geneve.local_tunnel_mac.clone(),
                        remote_tunnel_mac: geneve.remote_tunnel_mac.clone(),
                    },
                )
            })
            .collect();
        if next != current {
            self.state.realms.insert(plan.realm_id, next);
            store_state(&self.state_path, &self.state)?;
        }
        for (target_host, wanted) in &desired {
            let vni = wanted.vni.to_string();
            let remote = wanted.remote_transport_ip.to_string();
            let port = self.config.geneve_port.to_string();
            let (already_exists, _) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "-d",
                        "link",
                        "show",
                        "dev",
                        &wanted.interface,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if !already_exists
                && !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &self.config.fabric_namespace,
                            "ip",
                            "link",
                            "add",
                            &wanted.interface,
                            "type",
                            "geneve",
                            "id",
                            &vni,
                            "remote",
                            &remote,
                            "dstport",
                            &port,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            let tenant_mtu = plan.tenant_mtu.to_string();
            for args in [
                vec![
                    "netns",
                    "exec",
                    self.config.fabric_namespace.as_str(),
                    "ip",
                    "link",
                    "set",
                    "dev",
                    wanted.interface.as_str(),
                    "address",
                    wanted.local_tunnel_mac.as_str(),
                ],
                vec![
                    "netns",
                    "exec",
                    self.config.fabric_namespace.as_str(),
                    "ip",
                    "link",
                    "set",
                    "dev",
                    wanted.interface.as_str(),
                    "mtu",
                    tenant_mtu.as_str(),
                ],
            ] {
                if !self
                    .command
                    .run("ip", &args)
                    .map_err(LinuxP11Error::Storage)?
                {
                    return Err(LinuxP11Error::CommandFailed);
                }
            }
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &self.config.fabric_namespace,
                        "ip",
                        "link",
                        "set",
                        &wanted.interface,
                        "up",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            self.ensure_geneve_attachment(plan, wanted)?;
            if let Some(realized) = self
                .state
                .realms
                .get_mut(&plan.realm_id)
                .and_then(|realm| realm.geneve.get_mut(target_host))
            {
                realized.realized = true;
            }
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }
    pub(crate) fn ensure_geneve_attachment(
        &self,
        plan: &NamespacedRoutedFabricPlan,
        geneve: &GeneveOwnership,
    ) -> Result<(), LinuxP11Error> {
        let fabric_ns = self.config.fabric_namespace.as_str();
        let realm_ns = self
            .state
            .realms
            .get(&plan.realm_id)
            .ok_or(LinuxP11Error::CorruptState)?
            .namespace
            .clone();
        let (bridge_exists, bridge_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.bridge,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if bridge_exists && !bridge_output.contains("bridge") {
            return Err(LinuxP11Error::ForeignState);
        }
        if !bridge_exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "link",
                        "add",
                        &geneve.bridge,
                        "type",
                        "bridge",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        for args in [
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                geneve.bridge.as_str(),
                "up",
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                geneve.interface.as_str(),
                "master",
                geneve.bridge.as_str(),
            ],
        ] {
            if !self
                .command
                .run("ip", &args)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        let (realm_exists, realm_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    &realm_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.realm_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if realm_exists && !realm_output.contains("veth") {
            return Err(LinuxP11Error::ForeignState);
        }
        let (fabric_exists, fabric_output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "-d",
                    "link",
                    "show",
                    "dev",
                    &geneve.fabric_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if fabric_exists && !fabric_output.contains("veth") {
            return Err(LinuxP11Error::ForeignState);
        }
        if !realm_exists
            && (!self
                .command
                .run(
                    "ip",
                    &[
                        "link",
                        "add",
                        &geneve.realm_veth,
                        "type",
                        "veth",
                        "peer",
                        "name",
                        &geneve.fabric_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &["link", "set", &geneve.realm_veth, "netns", &realm_ns],
                    )
                    .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &["link", "set", &geneve.fabric_veth, "netns", fabric_ns],
                    )
                    .map_err(LinuxP11Error::Storage)?)
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        for args in [
            vec![
                "netns",
                "exec",
                &realm_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.realm_veth,
                "address",
                &geneve.local_tunnel_mac,
            ],
            vec![
                "netns",
                "exec",
                &realm_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.realm_veth,
                "up",
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "address",
                &geneve.local_tunnel_mac,
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "master",
                &geneve.bridge,
            ],
            vec![
                "netns",
                "exec",
                fabric_ns,
                "ip",
                "link",
                "set",
                "dev",
                &geneve.fabric_veth,
                "up",
            ],
        ] {
            if !self
                .command
                .run("ip", &args)
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for mac in [
            geneve.remote_tunnel_mac.as_str(),
            geneve.local_tunnel_mac.as_str(),
        ] {
            let device = if mac == geneve.remote_tunnel_mac {
                geneve.interface.as_str()
            } else {
                geneve.fabric_veth.as_str()
            };
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns", "exec", fabric_ns, "bridge", "fdb", "replace", mac, "dev", device,
                        "master", "static",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }
    pub(crate) fn remove_geneve_attachment(
        &self,
        geneve: &GeneveOwnership,
        realm_ns: &str,
    ) -> Result<(), LinuxP11Error> {
        let fabric_ns = self.config.fabric_namespace.as_str();
        let (exists, output) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    fabric_ns,
                    "ip",
                    "-d",
                    "link",
                    "show",
                    "dev",
                    &geneve.bridge,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if exists && !output.contains("bridge") {
            return Err(LinuxP11Error::ForeignState);
        }
        if exists {
            let (ports_exist, ports) = self
                .command
                .output(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "-o",
                        "link",
                        "show",
                        "master",
                        &geneve.bridge,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?;
            if ports_exist && !bridge_ports_are_owned(&ports, geneve) {
                return Err(LinuxP11Error::ForeignState);
            }
        }
        if exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        fabric_ns,
                        "ip",
                        "link",
                        "del",
                        &geneve.bridge,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        let (realm_veth_exists, _) = self
            .command
            .output(
                "ip",
                &[
                    "netns",
                    "exec",
                    realm_ns,
                    "ip",
                    "link",
                    "show",
                    "dev",
                    &geneve.realm_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?;
        if realm_veth_exists
            && !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        realm_ns,
                        "ip",
                        "link",
                        "del",
                        &geneve.realm_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        Ok(())
    }
}
