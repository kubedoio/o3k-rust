use super::*;

impl super::LinuxP11FabricBackend {
    pub(crate) fn realm_ownership(&self, plan: &NamespacedRoutedFabricPlan) -> RealmOwnership {
        let suffix = plan.realm_id.simple().to_string();
        RealmOwnership {
            realm_id: plan.realm_id,
            namespace: format!("o3k-r-{}", &suffix[..8]),
            bridge: format!("o3k-b-{}", &suffix[..8]),
            host_veth: format!("o3k-h-{}", &suffix[..8]),
            realm_veth: format!("o3k-n-{}", &suffix[..8]),
            fabric_veth: format!("o3k-f-{}", &suffix[..8]),
            fabric_realm_veth: format!("o3k-x-{}", &suffix[..8]),
            public_host_veth: format!("o3k-p-{}", &suffix[..8]),
            public_realm_veth: format!("o3k-q-{}", &suffix[..8]),
            geneve: BTreeMap::new(),
            attachments: BTreeMap::new(),
            endpoint_taps: BTreeMap::new(),
            pending_endpoint_taps: BTreeMap::new(),
            policy_generation: 0,
            policy_fingerprint: String::new(),
            public_generation: 0,
            public_fingerprint: String::new(),
            public_mark: 0,
            public_route_table: 0,
            public_addresses: Vec::new(),
            directory_generation: plan.directory_generation,
            local_fabric_generation: plan.local_fabric_generation,
        }
    }
    pub(crate) fn ensure_realm(
        &mut self,
        plan: &NamespacedRoutedFabricPlan,
    ) -> Result<(), LinuxP11Error> {
        let mut ownership = self.realm_ownership(plan);
        if let Some(existing) = self.state.realms.get(&plan.realm_id) {
            ownership.geneve = existing.geneve.clone();
            ownership.attachments = existing.attachments.clone();
            ownership.endpoint_taps = existing.endpoint_taps.clone();
            ownership.pending_endpoint_taps = existing.pending_endpoint_taps.clone();
            ownership.policy_generation = existing.policy_generation;
            ownership.policy_fingerprint = existing.policy_fingerprint.clone();
            if !existing.public_host_veth.is_empty() {
                ownership.public_host_veth = existing.public_host_veth.clone();
            }
            if !existing.public_realm_veth.is_empty() {
                ownership.public_realm_veth = existing.public_realm_veth.clone();
            }
            ownership.public_generation = existing.public_generation;
            ownership.public_fingerprint = existing.public_fingerprint.clone();
            ownership.public_mark = existing.public_mark;
            ownership.public_route_table = existing.public_route_table;
            ownership.public_addresses = existing.public_addresses.clone();
        }
        if let Some(existing) = self.state.realms.get(&plan.realm_id)
            && (existing.namespace != ownership.namespace
                || existing.bridge != ownership.bridge
                || existing.host_veth != ownership.host_veth
                || existing.realm_veth != ownership.realm_veth
                || existing.fabric_veth != ownership.fabric_veth
                || existing.fabric_realm_veth != ownership.fabric_realm_veth
                || existing.public_host_veth != ownership.public_host_veth
                || existing.public_realm_veth != ownership.public_realm_veth
                || plan.directory_generation < existing.directory_generation
                || plan.local_fabric_generation < existing.local_fabric_generation)
        {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        // If state says we own this realm but the bridge is gone (e.g. after a
        // crash or test fabric-interruption), clean up stale state so the
        // creation block below runs and recreates everything from scratch.
        if self.state.realms.contains_key(&plan.realm_id) {
            let (bridge_exists, _) = self
                .command
                .output("ip", &["link", "show", "dev", &ownership.bridge])
                .map_err(LinuxP11Error::Storage)?;
            if !bridge_exists {
                let _ = self
                    .command
                    .run("ip", &["netns", "delete", &ownership.namespace]);
                self.state.realms.remove(&plan.realm_id);
                store_state(&self.state_path, &self.state)?;
            }
        }
        if !self.state.realms.contains_key(&plan.realm_id) {
            let (exists, _) = self
                .command
                .output("ip", &["netns", "exec", &ownership.namespace, "true"])
                .map_err(LinuxP11Error::Storage)?;
            if exists {
                return Err(LinuxP11Error::ForeignState);
            }
            for interface in [
                &ownership.bridge,
                &ownership.host_veth,
                &ownership.realm_veth,
                &ownership.fabric_veth,
                &ownership.fabric_realm_veth,
            ] {
                let (interface_exists, _) = self
                    .command
                    .output("ip", &["link", "show", "dev", interface])
                    .map_err(LinuxP11Error::Storage)?;
                if interface_exists {
                    return Err(LinuxP11Error::ForeignState);
                }
            }
            self.state.realms.insert(plan.realm_id, ownership.clone());
            store_state(&self.state_path, &self.state)?;
            let commands = [
                vec!["netns", "add", ownership.namespace.as_str()],
                vec!["link", "add", ownership.bridge.as_str(), "type", "bridge"],
                vec!["link", "set", ownership.bridge.as_str(), "up"],
                vec![
                    "link",
                    "add",
                    ownership.host_veth.as_str(),
                    "type",
                    "veth",
                    "peer",
                    "name",
                    ownership.realm_veth.as_str(),
                ],
                vec![
                    "link",
                    "add",
                    ownership.fabric_veth.as_str(),
                    "type",
                    "veth",
                    "peer",
                    "name",
                    ownership.fabric_realm_veth.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.realm_veth.as_str(),
                    "netns",
                    ownership.namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.fabric_veth.as_str(),
                    "netns",
                    ownership.namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.fabric_realm_veth.as_str(),
                    "netns",
                    self.config.fabric_namespace.as_str(),
                ],
                vec![
                    "link",
                    "set",
                    ownership.host_veth.as_str(),
                    "master",
                    ownership.bridge.as_str(),
                ],
                vec!["link", "set", ownership.host_veth.as_str(), "up"],
            ];
            for args in commands {
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
                        &ownership.namespace,
                        "ip",
                        "link",
                        "set",
                        &ownership.realm_veth,
                        "up",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
            for (namespace, interface) in [
                (&ownership.namespace, &ownership.fabric_veth),
                (&self.config.fabric_namespace, &ownership.fabric_realm_veth),
            ] {
                if !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns", "exec", namespace, "ip", "link", "set", interface, "up",
                        ],
                    )
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
                        &ownership.namespace,
                        "sysctl",
                        "-w",
                        "net.ipv4.ip_forward=1",
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        } else if self.state.realms.get(&plan.realm_id) != Some(&ownership) {
            self.state.realms.insert(plan.realm_id, ownership.clone());
            store_state(&self.state_path, &self.state)?;
        }
        let tenant_mtu = plan.tenant_mtu.to_string();
        let fabric_mtu = plan.local_fabric_mtu.to_string();
        let gateway = u32::from(plan.realm_prefix.network)
            .checked_add(1)
            .map(std::net::Ipv4Addr::from)
            .filter(|gateway| plan.realm_prefix.contains(*gateway))
            .ok_or(LinuxP11Error::OwnershipConflict)?;
        let gateway_cidr = format!("{gateway}/{}", plan.realm_prefix.prefix_len);
        if !self
            .command
            .run(
                "ip",
                &[
                    "netns",
                    "exec",
                    &ownership.namespace,
                    "ip",
                    "addr",
                    "replace",
                    &gateway_cidr,
                    "dev",
                    &ownership.realm_veth,
                ],
            )
            .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        // Enable proxy ARP on the gateway so tenant VMs can reach remote realm
        // endpoints (hosted on other physical hosts) through the gateway.
        let _ = self.command.run(
            "sysctl",
            &[
                "-w",
                &format!("net.ipv4.conf.{}.proxy_arp=1", ownership.realm_veth),
            ],
        );
        for interface in [&ownership.bridge, &ownership.host_veth] {
            if !self
                .command
                .run("ip", &["link", "set", "dev", interface, "mtu", &tenant_mtu])
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        for (namespace, interface, mtu) in [
            (
                ownership.namespace.as_str(),
                ownership.realm_veth.as_str(),
                tenant_mtu.as_str(),
            ),
            (
                ownership.namespace.as_str(),
                ownership.fabric_veth.as_str(),
                fabric_mtu.as_str(),
            ),
            (
                self.config.fabric_namespace.as_str(),
                ownership.fabric_realm_veth.as_str(),
                fabric_mtu.as_str(),
            ),
        ] {
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns", "exec", namespace, "ip", "link", "set", "dev", interface, "mtu",
                        mtu,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }
    pub(crate) fn realize_routes(
        &self,
        plan: &NamespacedRoutedFabricPlan,
        ownership: &RealmOwnership,
    ) -> Result<(), LinuxP11Error> {
        for route in &plan.routes {
            let attachment = ownership
                .attachments
                .get(&route.target_host)
                .ok_or(LinuxP11Error::CorruptState)?;
            let destination = format!("{}/32", route.destination.network);
            if !self
                .command
                .run(
                    "ip",
                    &[
                        "netns",
                        "exec",
                        &ownership.namespace,
                        "ip",
                        "route",
                        "replace",
                        &destination,
                        "dev",
                        &attachment.realm_veth,
                    ],
                )
                .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &ownership.namespace,
                            "ip",
                            "neigh",
                            "replace",
                            &route.destination.network.to_string(),
                            "lladdr",
                            &attachment.remote_tunnel_mac,
                            "nud",
                            "permanent",
                            "dev",
                            &attachment.realm_veth,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
                || !self
                    .command
                    .run(
                        "ip",
                        &[
                            "netns",
                            "exec",
                            &ownership.namespace,
                            "ip",
                            "neigh",
                            "replace",
                            "proxy",
                            &route.destination.network.to_string(),
                            "dev",
                            &ownership.realm_veth,
                        ],
                    )
                    .map_err(LinuxP11Error::Storage)?
            {
                return Err(LinuxP11Error::CommandFailed);
            }
        }
        Ok(())
    }
    pub(crate) fn ensure_endpoint_taps(
        &mut self,
        plan: &NamespacedRoutedFabricPlan,
    ) -> Result<(), LinuxP11Error> {
        let Some(current) = self.state.realms.get(&plan.realm_id).cloned() else {
            return Err(LinuxP11Error::CorruptState);
        };
        let desired = plan
            .directory
            .entries
            .iter()
            .filter(|entry| entry.selected_host == plan.local_host)
            .map(|entry| {
                (
                    entry.endpoint_id,
                    EndpointTapOwnership {
                        endpoint_id: entry.endpoint_id,
                        interface: endpoint_tap_name(plan.realm_id, entry.endpoint_id),
                        mac: endpoint_tap_mac(plan.realm_id, entry.endpoint_id),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if !current.pending_endpoint_taps.is_empty() && current.pending_endpoint_taps != desired {
            return Err(LinuxP11Error::OwnershipConflict);
        }
        if current.endpoint_taps != desired && current.pending_endpoint_taps.is_empty() {
            let mut pending = current.clone();
            pending.pending_endpoint_taps = desired.clone();
            self.state.realms.insert(plan.realm_id, pending);
            store_state(&self.state_path, &self.state)?;
        }
        for (endpoint_id, old) in &current.endpoint_taps {
            if desired.contains_key(endpoint_id) {
                continue;
            }
            self.remove_endpoint_tap(old, &current.bridge)?;
        }
        for wanted in desired.values() {
            if let Some(existing) = current.endpoint_taps.get(&wanted.endpoint_id)
                && existing != wanted
            {
                return Err(LinuxP11Error::OwnershipConflict);
            }
            let (exists, output) = self
                .command
                .output("ip", &["-d", "link", "show", "dev", &wanted.interface])
                .map_err(LinuxP11Error::Storage)?;
            if exists && !tap_link_matches(&output, wanted, &current.bridge) {
                return Err(LinuxP11Error::ForeignState);
            }
            if exists
                && !current.endpoint_taps.contains_key(&wanted.endpoint_id)
                && !current
                    .pending_endpoint_taps
                    .contains_key(&wanted.endpoint_id)
            {
                return Err(LinuxP11Error::ForeignState);
            }
            if !exists {
                for args in [
                    vec![
                        "tuntap",
                        "add",
                        "dev",
                        wanted.interface.as_str(),
                        "mode",
                        "tap",
                    ],
                    vec![
                        "link",
                        "set",
                        "dev",
                        wanted.interface.as_str(),
                        "address",
                        wanted.mac.as_str(),
                    ],
                    vec![
                        "link",
                        "set",
                        "dev",
                        wanted.interface.as_str(),
                        "master",
                        current.bridge.as_str(),
                    ],
                    vec!["link", "set", "dev", wanted.interface.as_str(), "up"],
                ] {
                    if !self
                        .command
                        .run("ip", &args)
                        .map_err(LinuxP11Error::Storage)?
                    {
                        return Err(LinuxP11Error::CommandFailed);
                    }
                }
            }
        }
        if current.endpoint_taps != desired || !current.pending_endpoint_taps.is_empty() {
            let mut next = self
                .state
                .realms
                .get(&plan.realm_id)
                .cloned()
                .ok_or(LinuxP11Error::CorruptState)?;
            next.endpoint_taps = desired;
            next.pending_endpoint_taps.clear();
            self.state.realms.insert(plan.realm_id, next);
            store_state(&self.state_path, &self.state)?;
        }
        Ok(())
    }
    pub(crate) fn remove_endpoint_tap(
        &self,
        tap: &EndpointTapOwnership,
        bridge: &str,
    ) -> Result<(), LinuxP11Error> {
        let (exists, output) = self
            .command
            .output("ip", &["-d", "link", "show", "dev", &tap.interface])
            .map_err(LinuxP11Error::Storage)?;
        if exists && !tap_link_matches(&output, tap, bridge) {
            return Err(LinuxP11Error::ForeignState);
        }
        if exists
            && !self
                .command
                .run("ip", &["link", "del", "dev", &tap.interface])
                .map_err(LinuxP11Error::Storage)?
        {
            return Err(LinuxP11Error::CommandFailed);
        }
        Ok(())
    }
}
