use std::{
    env,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use async_trait::async_trait;
use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use o3k_compute_agent::{
    AgentClient, AgentConfig, AgentError, ArtifactStore, CommandExecutionResult, CommandExecutor,
    ConsoleLogResult, TlsFiles,
};
use o3k_libvirt::{ErrorCategory, LibvirtAdapter, LibvirtConfig, stable_domain_name};
use o3k_provider_contract::compute_proto as proto;
use rustix::{
    fd::OwnedFd,
    process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal},
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Test-only fault pause (issue #87): sleeps the configured duration when the
/// named env var is set. Absent, empty, non-numeric, or zero values are no-ops;
/// production configuration never sets these variables.
fn test_fault_pause_ms(name: &str, env_var: &str) {
    let Some(ms) = test_fault_pause_ms_value(std::env::var(env_var).ok()) else {
        return;
    };
    tracing::info!(pause_ms = ms, "test-only fault pause {} enabled", name);
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

/// Parse/guard half of `test_fault_pause_ms`; split out so the no-op
/// conditions can be unit-tested without sleeping.
fn test_fault_pause_ms_value(raw: Option<String>) -> Option<u64> {
    let raw = raw?;
    let Ok(ms) = raw.parse::<u64>() else {
        return None;
    };
    if ms == 0 {
        return None;
    }
    Some(ms)
}

#[derive(Clone)]
struct HealthState {
    agent: AgentClient,
    /// Live agent identity captured at startup; `o3k doctor` reads it from
    /// the loopback `/readyz` (issue #617). Never secrets.
    agent_id: String,
    software_version: String,
    capabilities: proto::Capabilities,
    libvirt_ready: bool,
    libvirt_error: Option<String>,
}

struct LibvirtCommandExecutor {
    adapter: LibvirtAdapter,
    artifact_root: PathBuf,
    image_materializer: o3k_compute_agent::ImageMaterializer,
    network: Arc<o3k_network::HostNetworkManager>,
    dhcp: Arc<Mutex<DhcpRuntime>>,
    /// The agent's configured disk capacity (`O3K_COMPUTE_MAX_DISK_GB`). The
    /// same value is published to placement as the DISK_GB inventory; the
    /// create arm uses it as an agent-side backstop (issue #606).
    max_disk_gb: u64,
    network_owned_by_external_agent: bool,
}

struct DhcpRuntime {
    service: o3k_dhcp::DhcpService,
    supervisor: Option<o3k_dhcp::DnsmasqSupervisor>,
    binary: PathBuf,
    interface: String,
    root: PathBuf,
}

fn cleanup_config_drive_artifact(
    root: &std::path::Path,
    agent_id: &str,
    resource_id: &str,
) -> Result<(), AgentError> {
    let store = ArtifactStore::open(root, agent_id)
        .map_err(|_| AgentError::Protocol("artifact store is unavailable".to_owned()))?;
    store
        .cleanup_config_drive_for_resource(resource_id)
        .map(|_| ())
        .map_err(|_| AgentError::Protocol("owned config-drive cleanup failed".to_owned()))
}

/// Best-effort reaping of the resource's owned config-drive artifacts after
/// the delete's host mutation cleanup. A failed cleanup is logged and never
/// changes the already-successful delete outcome: the leak verifier catches
/// residue separately, so a cleanup error must not turn a successful delete
/// into a failed or unknown command outcome.
fn reap_config_drive_artifacts(artifact_root: &std::path::Path, agent_id: &str, resource_id: &str) {
    if let Err(error) = cleanup_config_drive_artifact(artifact_root, agent_id, resource_id) {
        tracing::warn!(
            resource_id = %resource_id,
            error = %error,
            "owned config-drive artifact cleanup failed; the delete outcome is unaffected"
        );
    }
}

/// Best-effort reaping of incomplete-transfer `.part` files that the
/// protocol can never resume (issue #88 S5 supplementary): a part with no
/// manifest or an expired incomplete transfer is an orphan (the control
/// plane expires the abandoned transfer row and never resumes it; re-drives
/// mint fresh transfer ids), while a non-expired incomplete transfer is
/// resumed with the SAME transfer id after reconnect and its part is kept.
/// The `resource_id` filter scopes the reap to one deleted resource; `None`
/// reaps globally at startup. A failed cleanup is logged and never crashes
/// startup or changes a delete outcome: the leak verifier catches residue
/// separately.
fn reap_orphaned_transfer_parts(root: &std::path::Path, agent_id: &str, resource_id: Option<&str>) {
    let store = match ArtifactStore::open(root, agent_id) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "artifact store is unavailable; transfer-part reap skipped"
            );
            return;
        }
    };
    let result = match resource_id {
        Some(resource_id) => store.reap_orphaned_parts_for_resource(resource_id),
        None => store.reap_orphaned_parts(),
    };
    match result {
        Ok(removed) => {
            tracing::debug!(
                resource_id = ?resource_id,
                removed,
                "orphaned transfer-part reap completed"
            );
        }
        Err(error) => {
            tracing::warn!(
                resource_id = ?resource_id,
                error = %error,
                "owned transfer-part cleanup failed; the outcome is unaffected"
            );
        }
    }
}

/// Liveness probe for the orphan reap: true while the pid exists as a live
/// process. A zombie (`/proc/<pid>/stat` state `Z`) has already terminated
/// and counts as dead; an unreadable proc entry is dead as well. Linux-only
/// by design: the reap is /proc-based and the project targets Linux.
fn pid_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    !stat
        .split_whitespace()
        .nth(2)
        .is_some_and(|state| state == "Z")
}

/// Ownership verification for the orphan reap: the process cmdline must
/// contain the O3K dhcp root path (the supervisor always launches dnsmasq
/// with `--conf-file=<root>/dnsmasq.conf`, so the root appears in the
/// argv). The canonicalized variant is accepted too, for hosts where the
/// agent and the spawned process disagree on symlinks. A read failure
/// (permissions, pid reuse race) is an unverifiable pid, never ownership.
fn cmdline_contains_dhcp_root(pid: i32, root: &std::path::Path) -> Result<bool, std::io::Error> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let cmdline = String::from_utf8_lossy(&raw);
    if cmdline.contains(root.to_string_lossy().as_ref()) {
        return Ok(true);
    }
    if let Ok(canonical) = std::fs::canonicalize(root) {
        return Ok(cmdline.contains(canonical.to_string_lossy().as_ref()));
    }
    Ok(false)
}

/// Opens a race-resistant Linux process handle after ownership verification.
/// Signals are sent through the pidfd, never through the reusable numeric PID.
fn open_owned_pidfd(
    pid: i32,
    root: &std::path::Path,
    expected_starttime: u64,
) -> Result<OwnedFd, std::io::Error> {
    let raw_pid = pid;
    let pid = Pid::from_raw(raw_pid).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid process id")
    })?;
    // Acquire the stable process handle before inspecting /proc.  Checking
    // cmdline first leaves a PID-reuse window in which the verified process
    // can exit and the numeric PID can be assigned to a foreign process
    // before pidfd_open(), causing a signal to target the replacement.
    let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(std::io::Error::from)?;
    if !cmdline_contains_dhcp_root(raw_pid, root)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process command line is not O3K-owned",
        ));
    }
    // The spawn identity is the independent ownership proof: the kernel
    // start time of the process must equal the value recorded next to the
    // pidfile at spawn. A same-user process that forges the O3K dnsmasq
    // argv has a different start time; a process that inherited the numeric
    // PID after the owned dnsmasq exited has a different start time. Neither
    // may ever be signaled, so an identity mismatch is an unverifiable pid.
    if o3k_dhcp::process_starttime(raw_pid) != Some(expected_starttime) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "process start time does not match the recorded spawn identity",
        ));
    }
    Ok(pidfd)
}

impl DhcpRuntime {
    fn open(
        root: impl Into<PathBuf>,
        binary: impl Into<PathBuf>,
        interface: String,
    ) -> Result<Self, o3k_dhcp::DhcpError> {
        let root = root.into();
        Ok(Self {
            service: o3k_dhcp::DhcpService::open(root.clone())?,
            supervisor: None,
            binary: binary.into(),
            interface,
            root,
        })
    }

    fn validate(&self, attachments: &[proto::NetworkAttachment]) -> Result<(), AgentError> {
        let Some(first) = attachments.first() else {
            return Err(AgentError::Protocol(
                "DHCP requires a network attachment".to_owned(),
            ));
        };
        if attachments.iter().any(|attachment| {
            attachment.subnet_cidr != first.subnet_cidr
                || attachment.gateway_ipv4 != first.gateway_ipv4
        }) {
            return Err(AgentError::Protocol(
                "multiple network subnets are not supported by the flat DHCP profile".to_owned(),
            ));
        }
        let gateway = first
            .gateway_ipv4
            .parse()
            .map_err(|_| AgentError::Protocol("DHCP gateway address is invalid".to_owned()))?;
        let expected = o3k_dhcp::DhcpConfig {
            subnet: first.subnet_cidr.clone(),
            gateway,
            dns: vec![gateway],
            interface: self.interface.clone(),
            lease_seconds: 3600,
        };
        if let Some(existing) = self.service.configuration()
            && existing != &expected
        {
            return Err(AgentError::Protocol(
                "the managed bridge already has a different DHCP subnet".to_owned(),
            ));
        }
        for attachment in attachments {
            let address: Ipv4Addr = attachment
                .fixed_ipv4
                .parse()
                .map_err(|_| AgentError::Protocol("DHCP fixed address is invalid".to_owned()))?;
            if let Some(existing) = self.service.binding(&attachment.port_id)
                && (existing.mac != attachment.mac || existing.address != address)
            {
                return Err(AgentError::Protocol(
                    "DHCP port binding conflicts with durable state".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Applies only new bindings and returns those identities for precise rollback.
    fn apply(
        &mut self,
        attachments: &[proto::NetworkAttachment],
    ) -> Result<Vec<String>, AgentError> {
        self.validate(attachments)?;
        let first = attachments
            .first()
            .ok_or_else(|| AgentError::Protocol("DHCP requires a network attachment".to_owned()))?;
        let gateway = first
            .gateway_ipv4
            .parse()
            .map_err(|_| AgentError::Protocol("DHCP gateway address is invalid".to_owned()))?;
        if self.service.configuration().is_none() {
            self.service
                .configure(o3k_dhcp::DhcpConfig {
                    subnet: first.subnet_cidr.clone(),
                    gateway,
                    dns: vec![gateway],
                    interface: self.interface.clone(),
                    lease_seconds: 3600,
                })
                .map_err(|_| AgentError::Protocol("DHCP configuration is invalid".to_owned()))?;
        }
        let mut added = Vec::new();
        for attachment in attachments {
            if self.service.binding(&attachment.port_id).is_some() {
                continue;
            }
            let address = attachment
                .fixed_ipv4
                .parse()
                .map_err(|_| AgentError::Protocol("DHCP fixed address is invalid".to_owned()))?;
            if let Err(error) = self.service.upsert_binding(o3k_dhcp::Binding {
                port_id: attachment.port_id.clone(),
                mac: attachment.mac.clone(),
                address,
            }) {
                let _ = self.remove_ports(&added);
                return Err(AgentError::Protocol(format!(
                    "DHCP binding failed: {error}"
                )));
            }
            added.push(attachment.port_id.clone());
        }
        if let Some(supervisor) = self.supervisor.as_mut() {
            self.service.reload(supervisor).map_err(|_| {
                // Issue #88 C6a (DEV-1): a failed reload/start must roll
                // back the durable bindings of the ports added by this
                // apply, or a later restart re-serves them for a
                // never-created instance (bridge + owned dnsmasq leak).
                let _ = self.remove_ports(&added);
                AgentError::Protocol("DHCP reload failed".to_owned())
            })?;
        } else {
            self.supervisor = Some(self.service.start(&self.binary).map_err(|_| {
                let _ = self.remove_ports(&added);
                AgentError::Protocol("DHCP start failed".to_owned())
            })?);
        }
        Ok(added)
    }

    fn remove_ports(&mut self, ports: &[String]) -> Result<(), AgentError> {
        for port in ports {
            self.service
                .remove_binding(port)
                .map_err(|_| AgentError::Protocol("DHCP binding cleanup failed".to_owned()))?;
        }
        if self.service.configuration().is_none() {
            // A create that crashed before DHCP configuration never wrote a
            // dnsmasq.conf, so there is nothing to render, reload, or stop
            // (the supervisor cannot exist without a configuration). Never-
            // configured state is absent state, not an aborting error (issue
            // #608): the delete/reap must continue into TAP and bridge
            // cleanup. Bindings cannot be added without a configuration, but
            // clear any that may remain so nothing stale can be re-served.
            for port_id in self
                .service
                .bindings()
                .map(|binding| binding.port_id.clone())
                .collect::<Vec<_>>()
            {
                let _ = self.service.remove_binding(&port_id);
            }
            return Ok(());
        }
        self.service
            .write_config()
            .map_err(|_| AgentError::Protocol("DHCP configuration cleanup failed".to_owned()))?;
        if self.service.bindings().next().is_none() {
            if let Some(mut supervisor) = self.supervisor.take() {
                supervisor
                    .stop()
                    .map_err(|_| AgentError::Protocol("DHCP stop failed".to_owned()))?;
            }
        } else if let Some(supervisor) = self.supervisor.as_mut() {
            self.service
                .reload(supervisor)
                .map_err(|_| AgentError::Protocol("DHCP reload failed".to_owned()))?;
        }
        Ok(())
    }

    fn start_after_restart(
        &mut self,
        network: &o3k_network::HostNetworkManager,
    ) -> Result<(), AgentError> {
        if self.service.bindings().next().is_none() || self.supervisor.is_some() {
            return Ok(());
        }
        let config = self.service.configuration().cloned().ok_or_else(|| {
            AgentError::Protocol("DHCP bindings exist without configuration".to_owned())
        })?;
        let prefix_len = config
            .subnet
            .split_once('/')
            .and_then(|(_, prefix)| prefix.parse().ok())
            .ok_or_else(|| AgentError::Protocol("DHCP subnet prefix is invalid".to_owned()))?;
        network
            .ensure_gateway(o3k_network::GatewaySpec {
                address: config.gateway,
                prefix_len,
            })
            .map_err(|_| AgentError::Protocol("managed DHCP gateway is unavailable".to_owned()))?;
        self.supervisor = Some(
            self.service
                .start(&self.binary)
                .map_err(|_| AgentError::Protocol("DHCP restart failed".to_owned()))?,
        );
        Ok(())
    }

    /// Reaps every owned dnsmasq left behind by a previous agent process
    /// (issue #88 S3/S4): a crashed agent's dnsmasq was reparented to init
    /// and keeps running unsupervized. Invariant: at startup the supervisor
    /// is ALWAYS `None` (`DhcpRuntime::open` sets it; `start_after_restart`
    /// creates it later), so ANY owned dnsmasq found at startup is a
    /// leftover of a previous process — regardless of durable bindings. Live
    /// bindings are re-served by `start_after_restart` AFTER this residue
    /// cleanup (the caller's ordering), and the earlier stale-network reap
    /// already removed stale bindings first. Each `dnsmasq-*.pid` pidfile is
    /// verified by its recorded spawn identity (`<pidfile>.owner`, written at
    /// spawn with the process's kernel start time) and its process cmdline
    /// (it must contain the O3K dhcp root) before a pidfd is opened: SIGTERM,
    /// a bounded wait, SIGKILL only if still alive, then the pidfile and the
    /// identity file are removed. A pidfile whose process is already gone is
    /// just removed together with its identity file. A pidfile without a
    /// recorded identity, with a mismatched identity (argv spoof, PID reuse),
    /// or pointing at a foreign process is skipped with a warning and left
    /// for the inventory — an unprovable process must never be signaled.
    fn reap_owned_dnsmasq(&self) -> Result<(), AgentError> {
        let entries = std::fs::read_dir(&self.root)
            .map_err(|_| AgentError::Protocol("dhcp root is unreadable".to_owned()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("dnsmasq-") || !name.ends_with(".pid") {
                continue;
            }
            self.reap_owned_dnsmasq_pidfile(&path);
        }
        Ok(())
    }

    fn reap_owned_dnsmasq_pidfile(&self, pidfile: &std::path::Path) {
        let Ok(raw) = std::fs::read_to_string(pidfile) else {
            tracing::warn!(
                pidfile = %pidfile.display(),
                "owned dnsmasq pidfile is unreadable; left for the inventory"
            );
            return;
        };
        let Ok(pid) = raw.trim().parse::<i32>() else {
            tracing::warn!(
                pidfile = %pidfile.display(),
                "owned dnsmasq pidfile does not carry a pid; left for the inventory"
            );
            return;
        };
        if !pid_is_alive(pid) {
            // The process is already gone; only the stale pidfile (and its
            // recorded identity) remain.
            let identity_file = std::path::PathBuf::from(format!("{}.owner", pidfile.display()));
            for path in [pidfile, &identity_file] {
                if let Err(error) = std::fs::remove_file(path) {
                    tracing::warn!(
                        pidfile = %pidfile.display(),
                        error = %error,
                        "stale dnsmasq pidfile/identity removal failed"
                    );
                }
            }
            return;
        }
        let identity_file = std::path::PathBuf::from(format!("{}.owner", pidfile.display()));
        let expected_starttime = match std::fs::read_to_string(&identity_file) {
            Ok(raw) => match raw.trim().parse::<u64>() {
                Ok(starttime) => starttime,
                Err(_) => {
                    tracing::warn!(
                        pidfile = %pidfile.display(),
                        "dnsmasq pidfile identity is malformed; left for the inventory"
                    );
                    return;
                }
            },
            Err(_) => {
                tracing::warn!(
                    pidfile = %pidfile.display(),
                    "dnsmasq pidfile has no recorded spawn identity; left for the inventory"
                );
                return;
            }
        };
        let pidfd = match open_owned_pidfd(pid, &self.root, expected_starttime) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                tracing::warn!(
                    pid,
                    pidfile = %pidfile.display(),
                    error = %error,
                    "dnsmasq pidfile process is foreign or lacks pidfd support; left for the inventory"
                );
                return;
            }
        };
        let _ = pidfd_send_signal(&pidfd, Signal::Term);
        // Bounded wait for SIGTERM to take effect; SIGKILL only if the
        // process is still alive after the window.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2000);
        while pid_is_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if pid_is_alive(pid) {
            let _ = pidfd_send_signal(&pidfd, Signal::Kill);
            let kill_deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while pid_is_alive(pid) && std::time::Instant::now() < kill_deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        for path in [pidfile, &identity_file] {
            if let Err(error) = std::fs::remove_file(path) {
                tracing::warn!(
                    pidfile = %pidfile.display(),
                    error = %error,
                    "owned dnsmasq pidfile/identity removal failed"
                );
            }
        }
    }
}

struct NetworkPreparation {
    created_taps: Vec<o3k_network::TapSpec>,
    added_dhcp_ports: Vec<String>,
    external_owner: bool,
}

fn rollback_network(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
) -> Result<(), AgentError> {
    if preparation.external_owner {
        return Ok(());
    }
    let mut first_error = None;
    if let Ok(mut runtime) = dhcp.lock() {
        if let Err(error) = runtime.remove_ports(&preparation.added_dhcp_ports) {
            first_error = Some(error);
        }
    } else {
        first_error = Some(AgentError::Protocol(
            "DHCP runtime lock is poisoned".to_owned(),
        ));
    }
    for tap in preparation.created_taps.iter().rev() {
        if let Err(error) = network.delete_tap(tap) {
            first_error.get_or_insert_with(|| {
                AgentError::Protocol(format!("TAP rollback failed: {error}"))
            });
        }
    }
    if let Err(error) = network.cleanup_if_unused() {
        first_error.get_or_insert_with(|| {
            AgentError::Protocol(format!("network rollback failed: {error}"))
        });
    }
    first_error.map_or(Ok(()), Err)
}

fn return_after_network_rollback(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
    error: AgentError,
) -> AgentError {
    match rollback_network(network, dhcp, preparation) {
        Ok(()) => error,
        Err(rollback_error) => AgentError::Protocol(format!(
            "{error}; network rollback also failed: {rollback_error}"
        )),
    }
}

fn return_after_create_rollback(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    preparation: &NetworkPreparation,
    image_materializer: &o3k_compute_agent::ImageMaterializer,
    artifact_root: &std::path::Path,
    instance_id: &str,
    error: AgentError,
) -> AgentError {
    let error = return_after_network_rollback(network, dhcp, preparation, error);
    match image_materializer.delete_instance(instance_id) {
        Ok(()) => match cleanup_console_log(artifact_root, instance_id) {
            Ok(()) => error,
            Err(cleanup_error) => AgentError::Protocol(format!(
                "{error}; console rollback also failed: {cleanup_error}"
            )),
        },
        Err(cleanup_error) => AgentError::Protocol(format!(
            "{error}; image rollback also failed: {cleanup_error}"
        )),
    }
}

fn cleanup_instance_network(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    instance_id: &str,
) -> Result<(), AgentError> {
    let port_ids = network
        .owned_port_ids_for_instance(instance_id)
        .map_err(|_| AgentError::Protocol("owned network lookup failed".to_owned()))?;
    {
        let mut runtime = dhcp
            .lock()
            .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
        runtime.remove_ports(&port_ids)?;
    }
    network
        .delete_taps_for_instance(instance_id)
        .map_err(|error| AgentError::Protocol(format!("owned TAP cleanup failed: {error}")))?;
    network
        .cleanup_if_unused()
        .map_err(|error| AgentError::Protocol(format!("owned bridge cleanup failed: {error}")))
}

/// Domain-presence probe for the startup residue reaps. The real adapter
/// classifies libvirt outcomes; tests inject a fake for the absent shape
/// (the bin's default test build has no libvirt feature).
#[async_trait]
trait DomainPresence: Send + Sync {
    /// `Ok(true)`: the domain provably does not exist — its recorded network
    /// state may be reaped. `Ok(false)`: the domain exists. `Err`: presence
    /// is unknown — fail closed, the instance keeps its network state.
    async fn domain_is_absent(&self, name: &str) -> Result<bool, AgentError>;
}

#[async_trait]
impl DomainPresence for LibvirtAdapter {
    async fn domain_is_absent(&self, name: &str) -> Result<bool, AgentError> {
        match self.inspect(name.to_owned()).await {
            Err(error) if error.category == ErrorCategory::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(agent_error(error)),
        }
    }
}

/// Startup reconciliation for crash residue (issue #87 S3 rerun #5): a
/// create prepares the host network (bridge, TAPs, DHCP bindings) before the
/// domain is defined, so an agent death in that window leaves O3K-owned
/// artifacts behind while the control-plane delete converges through local
/// completion and never dispatches an agent delete. This reaps the recorded
/// network state of every manifest instance whose domain provably does not
/// exist; the durable ownership manifest is the only authority that binds a
/// host interface to an instance.
///
/// An observation failure skips the instance (fail closed: a live or
/// uninspectable domain must never lose its TAP). Reap errors are returned
/// for logging only and are never fatal, so the residue is retried on the
/// next restart. `cleanup_if_unused` keeps the shared bridge in place while
/// any other recorded instance still uses it, and every deletion is bounded
/// by the manifest and the kernel ownership checks.
async fn reap_stale_instance_networks(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    presence: &dyn DomainPresence,
) -> Result<(), AgentError> {
    let instance_ids = network
        .owned_instance_ids()
        .map_err(|error| AgentError::Protocol(format!("owned instance lookup failed: {error}")))?;
    let mut first_error = None;
    for instance_id in instance_ids {
        match presence
            .domain_is_absent(&stable_domain_name(&instance_id))
            .await
        {
            Ok(true) => {
                tracing::info!(
                    instance_id = %instance_id,
                    "reaping network residue of absent instance"
                );
                if let Err(error) = cleanup_instance_network(network, dhcp, &instance_id) {
                    first_error.get_or_insert(error);
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    instance_id = %instance_id,
                    "skipping network residue reap: domain presence is unknown"
                );
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn cleanup_console_log(
    artifact_root: &std::path::Path,
    instance_id: &str,
) -> Result<(), AgentError> {
    let domain_name = o3k_libvirt::stable_domain_name(instance_id);
    let path = artifact_root
        .parent()
        .ok_or_else(|| AgentError::Protocol("agent artifact root has no service root".to_owned()))?
        .join("console")
        .join(format!("{domain_name}.log"));
    match std::fs::remove_file(path) {
        Ok(()) => {
            let _ = std::fs::remove_dir(
                artifact_root
                    .parent()
                    .ok_or_else(|| {
                        AgentError::Protocol("agent artifact root has no service root".to_owned())
                    })?
                    .join("console"),
            );
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AgentError::Protocol(
            "console log cleanup failed".to_owned(),
        )),
    }
}

fn prepare_network(
    command: &proto::Command,
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
) -> Result<NetworkPreparation, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol("create action is missing".to_owned()));
    };
    let resolved = create
        .resolved
        .as_ref()
        .ok_or_else(|| AgentError::Protocol("resolved create inputs are missing".to_owned()))?;
    let runtime = dhcp
        .lock()
        .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
    runtime.validate(&resolved.network_attachments)?;
    let first = resolved
        .network_attachments
        .first()
        .ok_or_else(|| AgentError::Protocol("network attachment is missing".to_owned()))?;
    let gateway = first
        .gateway_ipv4
        .parse()
        .map_err(|_| AgentError::Protocol("network gateway address is invalid".to_owned()))?;
    let prefix_len = first
        .subnet_cidr
        .split_once('/')
        .and_then(|(_, prefix)| prefix.parse().ok())
        .ok_or_else(|| AgentError::Protocol("network subnet prefix is invalid".to_owned()))?;
    network
        .ensure_gateway(o3k_network::GatewaySpec {
            address: gateway,
            prefix_len,
        })
        .map_err(|error| AgentError::Protocol(format!("gateway preparation failed: {error}")))?;
    drop(runtime);
    let mut preparation = NetworkPreparation {
        created_taps: Vec::new(),
        added_dhcp_ports: Vec::new(),
        external_owner: false,
    };
    for attachment in &resolved.network_attachments {
        let spec = o3k_network::TapSpec {
            instance_id: command.resource_id.clone(),
            port_id: attachment.port_id.clone(),
            mac: attachment.mac.clone(),
        };
        match network.ensure_tap(&spec) {
            Ok((_, true)) => preparation.created_taps.push(spec.clone()),
            Ok((_, false)) => {}
            Err(error) => {
                return Err(return_after_network_rollback(
                    network,
                    dhcp,
                    &preparation,
                    AgentError::Protocol(format!("TAP preparation failed: {error}")),
                ));
            }
        }
    }
    let mut runtime = dhcp
        .lock()
        .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))?;
    match runtime.apply(&resolved.network_attachments) {
        Ok(added) => preparation.added_dhcp_ports = added,
        Err(error) => {
            drop(runtime);
            return Err(return_after_network_rollback(
                network,
                dhcp,
                &preparation,
                error,
            ));
        }
    }
    Ok(preparation)
}

/// Host-local evidence required to turn a create request into a libvirt
/// definition.  The path is supplied by the agent's committed artifact store;
/// it is never derived from an artifact id or digest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommittedArtifact {
    artifact_id: String,
    kind: proto::ArtifactKind,
    format: String,
    sha256: String,
    path: PathBuf,
}

/// A TAP name is usable only together with the network subsystem's ownership
/// evidence.  A port id and MAC address alone are not sufficient proof that a
/// host device may be attached to a domain.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedTap {
    port_id: String,
    tap_name: String,
    mac_address: String,
    ownership_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CreateDomainIdentity {
    server_id: String,
    project_id: String,
    generation: u64,
    operation_id: String,
    managed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommittedCreateInputs {
    image: CommittedArtifact,
    config_drive: CommittedArtifact,
    owned_taps: Vec<OwnedTap>,
    identity: CreateDomainIdentity,
}

/// Resolve the host-local inputs for a create command before touching
/// libvirt.
///
/// The control-plane command deliberately carries artifact references, not
/// host paths.  The agent-side artifact store also requires the complete
/// authenticated `ArtifactOffer` (including its transfer identity and
/// expiry) to resolve a committed file.  Those fields are not part of
/// `CreateCommand.resolved`, so deriving a path from a digest or rebuilding an
/// offer here would weaken the transfer identity fence.  Network attachments
/// likewise contain only port/MAC/IP data; a libvirt interface requires a TAP
/// name that has been proven to be owned by the host network subsystem.
///
/// Keep this boundary explicit and fail closed until both authenticated
/// lookup metadata and a durable network-ownership lookup are present in the
/// command/executor contract.
fn resolve_create_domain_spec(
    command: &proto::Command,
    committed: Option<&CommittedCreateInputs>,
) -> Result<o3k_libvirt::DomainSpec, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol(
            "create command action is missing or has the wrong type".to_owned(),
        ));
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(AgentError::Protocol(
            "create command resolved inputs are missing".to_owned(),
        ));
    };
    if resolved.image_artifact_id.trim().is_empty()
        || resolved.image_sha256.trim().is_empty()
        || resolved.image_format.trim().is_empty()
        || resolved.config_drive_artifact_id.trim().is_empty()
        || resolved.config_drive_sha256.trim().is_empty()
    {
        return Err(AgentError::Protocol(
            "create command artifact references are incomplete".to_owned(),
        ));
    }

    let Some(committed) = committed else {
        return Err(AgentError::Protocol(
            "create is fail-closed: committed artifact bytes and owned TAP names are not available"
                .to_owned(),
        ));
    };

    if committed.image.artifact_id != resolved.image_artifact_id
        || committed.image.kind != proto::ArtifactKind::ImageBase
        || committed.image.sha256 != resolved.image_sha256
        || committed.image.format != resolved.image_format
        || committed.config_drive.artifact_id != resolved.config_drive_artifact_id
        || committed.config_drive.kind != proto::ArtifactKind::ConfigDriveIso
        || committed.config_drive.sha256 != resolved.config_drive_sha256
        || committed.config_drive.format != "iso"
    {
        return Err(AgentError::Protocol(
            "committed artifact evidence does not match create references".to_owned(),
        ));
    }
    if committed.identity.server_id != command.resource_id
        || committed.identity.project_id.trim().is_empty()
        || committed.identity.operation_id != command.operation_id
        || committed.identity.managed_by.trim().is_empty()
    {
        return Err(AgentError::Protocol(
            "create domain ownership identity is incomplete or mismatched".to_owned(),
        ));
    }
    if committed.image.path.as_os_str().is_empty()
        || !committed.image.path.is_absolute()
        || committed.config_drive.path.as_os_str().is_empty()
        || !committed.config_drive.path.is_absolute()
    {
        return Err(AgentError::Protocol(
            "committed artifact paths must be absolute host-local paths".to_owned(),
        ));
    }
    if committed.owned_taps.len() != resolved.network_attachments.len()
        || committed
            .owned_taps
            .iter()
            .any(|tap| tap.ownership_token.trim().is_empty())
    {
        return Err(AgentError::Protocol(
            "owned TAP evidence is incomplete or does not cover network attachments".to_owned(),
        ));
    }
    let network_interfaces = resolved
        .network_attachments
        .iter()
        .map(|attachment| {
            let tap = committed
                .owned_taps
                .iter()
                .find(|tap| tap.port_id == attachment.port_id);
            let Some(tap) = tap else {
                return Err(AgentError::Protocol(
                    "network attachment has no matching owned TAP".to_owned(),
                ));
            };
            if tap.mac_address != attachment.mac || tap.tap_name.trim().is_empty() {
                return Err(AgentError::Protocol(
                    "owned TAP evidence does not match network attachment".to_owned(),
                ));
            }
            Ok(o3k_libvirt::DomainNetworkInterface {
                tap_name: tap.tap_name.clone(),
                mac_address: tap.mac_address.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let spec = o3k_libvirt::DomainSpec {
        metadata: o3k_libvirt::DomainMetadata {
            server_id: committed.identity.server_id.clone(),
            project_id: committed.identity.project_id.clone(),
            generation: committed.identity.generation,
            operation_id: committed.identity.operation_id.clone(),
            managed_by: committed.identity.managed_by.clone(),
        },
        vcpus: resolved.vcpus,
        memory_mib: resolved.memory_mib,
        image_id: committed.image.path.to_string_lossy().into_owned(),
        config_drive_image: Some(o3k_libvirt::ConfigDriveImage {
            path: committed.config_drive.path.to_string_lossy().into_owned(),
            sha256: committed.config_drive.sha256.clone(),
        }),
        network_interfaces,
    };
    o3k_libvirt::build_domain_xml(&spec)
        .map(|_| spec)
        .map_err(|_| {
            AgentError::Protocol("resolved domain inputs failed libvirt validation".to_owned())
        })
}

fn resolve_committed_create_inputs(
    command: &proto::Command,
    artifact_root: &std::path::Path,
    image_materializer: &o3k_compute_agent::ImageMaterializer,
    network: &o3k_network::HostNetworkManager,
    network_owned_by_external_agent: bool,
) -> Result<CommittedCreateInputs, AgentError> {
    let Some(proto::command::Action::Create(create)) = command.action.as_ref() else {
        return Err(AgentError::Protocol("create action is missing".to_owned()));
    };
    let Some(resolved) = create.resolved.as_ref() else {
        return Err(AgentError::Protocol(
            "resolved create inputs are missing".to_owned(),
        ));
    };
    let store = o3k_compute_agent::ArtifactStore::open(artifact_root, &command.agent_id)
        .map_err(|_| AgentError::Protocol("agent artifact store is unavailable".to_owned()))?;
    store
        .resolve_committed_artifact(&o3k_compute_agent::CommittedArtifactQuery {
            command_id: command.command_id.clone(),
            operation_id: command.operation_id.clone(),
            resource_id: command.resource_id.clone(),
            artifact_id: resolved.image_artifact_id.clone(),
            kind: proto::ArtifactKind::ImageBase,
            sha256: resolved.image_sha256.clone(),
            format: resolved.image_format.clone(),
        })
        .map_err(|_| AgentError::Protocol("committed image artifact is unavailable".to_owned()))?;
    let materialization_request = o3k_compute_agent::image_materialization_request(command)
        .map_err(|_| {
            AgentError::Protocol("image materialization identity is invalid".to_owned())
        })?;
    let image_path = image_materializer
        .materialize(&materialization_request)
        .map_err(|_| {
            AgentError::Protocol("instance image overlay could not be realized".to_owned())
        })?
        .overlay_path;
    let config_path = store
        .resolve_committed_artifact(&o3k_compute_agent::CommittedArtifactQuery {
            command_id: command.command_id.clone(),
            operation_id: command.operation_id.clone(),
            resource_id: command.resource_id.clone(),
            artifact_id: resolved.config_drive_artifact_id.clone(),
            kind: proto::ArtifactKind::ConfigDriveIso,
            sha256: resolved.config_drive_sha256.clone(),
            format: "iso".to_owned(),
        })
        .map_err(|_| {
            AgentError::Protocol("committed config-drive artifact is unavailable".to_owned())
        })?;
    let mut owned_taps = Vec::with_capacity(resolved.network_attachments.len());
    for attachment in &resolved.network_attachments {
        let tap_name = network
            .resolve_owned_tap(&o3k_network::TapSpec {
                instance_id: if network_owned_by_external_agent {
                    attachment.port_id.clone()
                } else {
                    command.resource_id.clone()
                },
                port_id: attachment.port_id.clone(),
                mac: attachment.mac.clone(),
            })
            .map_err(|error| {
                let managed_taps = network.discover_managed().unwrap_or_default();
                AgentError::Protocol(format!(
                    "owned TAP is unavailable for port {}: {error}; managed_taps={managed_taps:?}",
                    attachment.port_id,
                ))
            })?;
        owned_taps.push(OwnedTap {
            port_id: attachment.port_id.clone(),
            tap_name,
            mac_address: attachment.mac.clone(),
            ownership_token: "durable-network-manifest".to_owned(),
        });
    }
    Ok(CommittedCreateInputs {
        image: CommittedArtifact {
            artifact_id: resolved.image_artifact_id.clone(),
            kind: proto::ArtifactKind::ImageBase,
            format: resolved.image_format.clone(),
            sha256: resolved.image_sha256.clone(),
            path: image_path,
        },
        config_drive: CommittedArtifact {
            artifact_id: resolved.config_drive_artifact_id.clone(),
            kind: proto::ArtifactKind::ConfigDriveIso,
            format: "iso".to_owned(),
            sha256: resolved.config_drive_sha256.clone(),
            path: config_path,
        },
        owned_taps,
        identity: CreateDomainIdentity {
            server_id: command.resource_id.clone(),
            project_id: resolved.project_id.clone(),
            generation: 1,
            operation_id: command.operation_id.clone(),
            managed_by: "o3k-compute".to_owned(),
        },
    })
}

#[async_trait]
impl CommandExecutor for LibvirtCommandExecutor {
    async fn execute(
        &self,
        command: &proto::Command,
    ) -> Result<CommandExecutionResult, AgentError> {
        let name = stable_domain_name(&command.resource_id);
        let success = |message: &str, resource_state: proto::ResourceState| {
            Ok(CommandExecutionResult {
                state: proto::OperationState::Succeeded as i32,
                error_category: proto::ErrorCategory::Unspecified as i32,
                resource_state: resource_state as i32,
                redacted_message: message.to_owned(),
                provider_resource_id: name.clone(),
                console_log: None,
                block_device: None,
            })
        };
        match command.action.as_ref() {
            Some(proto::command::Action::Inspect(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(inspection) => inspection,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        return Ok(inspect_not_found_result(name));
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                success(
                    if inspection.active {
                        "domain is active"
                    } else {
                        "domain is inactive"
                    },
                    resource_state(&inspection),
                )
            }
            Some(proto::command::Action::Start(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(value) => value,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        return Err(agent_error(error));
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                self.adapter
                    .start_owned(name.clone(), command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success("domain started", resource_state(&inspection))
            }
            Some(proto::command::Action::Stop(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                // CirrOS guests ignore ACPI shutdown requests, and public
                // Nova/libvirt powers off hard by default; a graceful
                // shutdown would never reach SHUTOFF. Force the stop, then
                // confirm the guest is actually inactive before projecting
                // the stopped state.
                self.adapter
                    .force_stop_owned(name.clone(), command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .wait_for_domain_inactive(name.clone(), &command.resource_id)
                    .await?;
                success("domain stopped", resource_state(&inspection))
            }
            Some(proto::command::Action::Reboot(_)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                // Hard reboot only exists as force stop plus start: guests
                // without ACPI handling (CirrOS) never react to an ACPI
                // reboot request.
                self.adapter
                    .force_stop_owned(name.clone(), command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                self.adapter
                    .start_owned(name.clone(), command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                success("domain rebooted", resource_state(&inspection))
            }
            Some(proto::command::Action::Delete(_)) => {
                let inspection = match self.adapter.inspect(name.clone()).await {
                    Ok(value) => value,
                    Err(error) if error.category == ErrorCategory::NotFound => {
                        if !self.network_owned_by_external_agent {
                            cleanup_instance_network(
                                &self.network,
                                &self.dhcp,
                                &command.resource_id,
                            )?;
                        }
                        self.image_materializer
                            .delete_instance(&command.resource_id)
                            .map_err(|_| {
                                AgentError::Protocol("instance image cleanup failed".to_owned())
                            })?;
                        reap_config_drive_artifacts(
                            &self.artifact_root,
                            &command.agent_id,
                            &command.resource_id,
                        );
                        reap_orphaned_transfer_parts(
                            &self.artifact_root,
                            &command.agent_id,
                            Some(&command.resource_id),
                        );
                        cleanup_console_log(&self.artifact_root, &command.resource_id)?;
                        return success("domain already absent", proto::ResourceState::Deleted);
                    }
                    Err(error) => return Err(agent_error(error)),
                };
                verify_owned_domain(&inspection, &command.resource_id)?;
                if inspection.active {
                    self.adapter
                        .force_stop_owned(name.clone(), command.resource_id.clone())
                        .await
                        .map_err(agent_error)?;
                }
                self.adapter
                    .undefine_owned(name.clone(), command.resource_id.clone())
                    .await
                    .map_err(agent_error)?;
                if !self.network_owned_by_external_agent {
                    cleanup_instance_network(&self.network, &self.dhcp, &command.resource_id)?;
                }
                self.image_materializer
                    .delete_instance(&command.resource_id)
                    .map_err(|_| {
                        AgentError::Protocol("instance image cleanup failed".to_owned())
                    })?;
                reap_config_drive_artifacts(
                    &self.artifact_root,
                    &command.agent_id,
                    &command.resource_id,
                );
                reap_orphaned_transfer_parts(
                    &self.artifact_root,
                    &command.agent_id,
                    Some(&command.resource_id),
                );
                cleanup_console_log(&self.artifact_root, &command.resource_id)?;
                success("domain deleted", proto::ResourceState::Deleted)
            }
            Some(proto::command::Action::Create(_)) => {
                // Agent-side disk-capacity backstop (issue #606): reject an
                // over-capacity create BEFORE any host mutation. The
                // placement gate normally rejects it earlier, but in the
                // agent-restart staleness window the ledger can still carry
                // the pre-restart capacity; the capacity classification makes
                // this indistinguishable from a placement rejection on the
                // control plane. The guard reads only the resolved protobuf
                // inputs, so no TAP, bridge, overlay, or domain exists yet.
                if let Some(disk_gib) = create_disk_gib(command)
                    && disk_gib > self.max_disk_gb
                {
                    tracing::warn!(
                        resource_id = %command.resource_id,
                        disk_gib,
                        max_disk_gb = self.max_disk_gb,
                        "create rejected before host mutation: requested disk \
                         exceeds the agent capacity"
                    );
                    return Ok(capacity_failure_result(disk_gib, self.max_disk_gb));
                }
                // Failures that provably happened before libvirt could create
                // the domain are definitive: the instance does not exist, so
                // the operation is terminally Failed rather than of unknown
                // outcome. Unknown-outcome reporting is preserved for
                // failures after a possible provider side effect (define,
                // start, or a failed rollback) and for observation errors.
                let definitive_failure = |error: AgentError| {
                    definitive_create_failure_result(
                        &self.artifact_root,
                        &command.agent_id,
                        &command.resource_id,
                        &command.operation_id,
                        error,
                    )
                };
                let preparation = if self.network_owned_by_external_agent {
                    NetworkPreparation {
                        created_taps: Vec::new(),
                        added_dhcp_ports: Vec::new(),
                        external_owner: true,
                    }
                } else {
                    match prepare_network(command, &self.network, &self.dhcp) {
                        Ok(preparation) => preparation,
                        Err(error) => return definitive_failure(error),
                    }
                };
                match self.adapter.inspect(name.clone()).await {
                    Ok(existing) => {
                        if let Err(error) = verify_owned_domain(&existing, &command.resource_id) {
                            return definitive_failure(return_after_network_rollback(
                                &self.network,
                                &self.dhcp,
                                &preparation,
                                error,
                            ));
                        }
                        return success("domain already exists", resource_state(&existing));
                    }
                    Err(error) if error.category == ErrorCategory::NotFound => {}
                    Err(error) => {
                        return Err(return_after_network_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            agent_error(error),
                        ));
                    }
                }
                let committed = match resolve_committed_create_inputs(
                    command,
                    &self.artifact_root,
                    &self.image_materializer,
                    &self.network,
                    self.network_owned_by_external_agent,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        // Issue #611 (ASR-021 agent-control-plane-network-interruption):
                        // a missing committed artifact is a CONTROL-PLANE delivery
                        // problem, not a definitive absence. The artifact transfer
                        // can be re-offered by the create re-drive, so this failure
                        // must never be reported as a terminal absence-proven
                        // failure (which would strand the server in ERROR with no
                        // recovery path). The create provably never executed (the
                        // failure is upstream of the define/start boundary), so the
                        // unknown-outcome classification is safe: the reconciler
                        // re-drives the create and the transfer loop re-offers the
                        // missing artifact. The network/console rollback still runs,
                        // exactly as for the definitive classification.
                        return unknown_create_outcome_result(
                            &self.artifact_root,
                            &command.agent_id,
                            &command.resource_id,
                            return_after_create_rollback(
                                &self.network,
                                &self.dhcp,
                                &preparation,
                                &self.image_materializer,
                                &self.artifact_root,
                                &command.resource_id,
                                error,
                            ),
                        );
                    }
                };
                let spec = match resolve_create_domain_spec(command, Some(&committed)) {
                    Ok(value) => value,
                    Err(error) => {
                        return definitive_failure(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            error,
                        ));
                    }
                };
                let definition = match o3k_libvirt::build_domain_xml(&spec) {
                    Ok(value) => value,
                    Err(_) => {
                        return definitive_failure(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            AgentError::Protocol("domain XML is invalid".to_owned()),
                        ));
                    }
                };
                let definition_name = definition.name.clone();
                let console_path = match o3k_libvirt::console_log_path(
                    &committed.image.path.to_string_lossy(),
                    &definition_name,
                ) {
                    Ok(path) => path,
                    Err(error) => {
                        return definitive_failure(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            agent_error(error),
                        ));
                    }
                };
                let console_root = match std::path::Path::new(&console_path)
                    .parent()
                    .ok_or_else(|| AgentError::Protocol("console log root is invalid".to_owned()))
                {
                    Ok(root) => root,
                    Err(error) => {
                        return definitive_failure(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            error,
                        ));
                    }
                };
                if let Err(error) = std::fs::create_dir_all(console_root) {
                    return definitive_failure(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        AgentError::Protocol(format!(
                            "console log root could not be created: {error}"
                        )),
                    ));
                }
                #[cfg(unix)]
                if let Err(error) =
                    std::fs::set_permissions(console_root, std::fs::Permissions::from_mode(0o2730))
                {
                    return definitive_failure(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        AgentError::Protocol(format!(
                            "console log root permissions could not be set: {error}"
                        )),
                    ));
                }
                let console_file = match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&console_path)
                {
                    Ok(file) => file,
                    Err(error) => {
                        return definitive_failure(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            AgentError::Protocol(format!(
                                "console log could not be created: {error}"
                            )),
                        ));
                    }
                };
                #[cfg(unix)]
                if let Err(error) =
                    console_file.set_permissions(std::fs::Permissions::from_mode(0o660))
                {
                    return definitive_failure(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        AgentError::Protocol(format!(
                            "console log permissions could not be set: {error}"
                        )),
                    ));
                }
                if let Err(error) = self
                    .adapter
                    .define(o3k_libvirt::DomainDefinition {
                        name: definition_name.clone(),
                        xml: definition.xml,
                    })
                    .await
                {
                    return Err(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        agent_error(error),
                    ));
                }
                test_fault_pause_ms("after-define", "O3K_TEST_FAULT_PAUSE_AFTER_DEFINE_MS");
                if let Err(error) = self
                    .adapter
                    .start_owned(definition_name.clone(), command.resource_id.clone())
                    .await
                {
                    let undefine_result = self
                        .adapter
                        .undefine_owned(definition_name.clone(), command.resource_id.clone())
                        .await;
                    let error = match undefine_result {
                        Ok(()) => agent_error(error),
                        Err(cleanup_error) => AgentError::Protocol(format!(
                            "{}; domain rollback also failed: {}",
                            agent_error(error),
                            cleanup_error
                        )),
                    };
                    return Err(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        error,
                    ));
                }
                test_fault_pause_ms("after-start", "O3K_TEST_FAULT_PAUSE_AFTER_START_MS");
                let inspection = match self.adapter.inspect(definition_name.clone()).await {
                    Ok(value) => value,
                    Err(error) => {
                        let error = match self
                            .adapter
                            .undefine_owned(name.clone(), command.resource_id.clone())
                            .await
                        {
                            Ok(()) => agent_error(error),
                            Err(cleanup_error) => AgentError::Protocol(format!(
                                "{}; domain rollback also failed: {}",
                                agent_error(error),
                                cleanup_error
                            )),
                        };
                        return Err(return_after_create_rollback(
                            &self.network,
                            &self.dhcp,
                            &preparation,
                            &self.image_materializer,
                            &self.artifact_root,
                            &command.resource_id,
                            error,
                        ));
                    }
                };
                if let Err(error) = verify_owned_domain(&inspection, &command.resource_id) {
                    let error = match self
                        .adapter
                        .undefine_owned(name.clone(), command.resource_id.clone())
                        .await
                    {
                        Ok(()) => error,
                        Err(cleanup_error) => AgentError::Protocol(format!(
                            "{error}; domain rollback also failed: {cleanup_error}"
                        )),
                    };
                    return Err(return_after_create_rollback(
                        &self.network,
                        &self.dhcp,
                        &preparation,
                        &self.image_materializer,
                        &self.artifact_root,
                        &command.resource_id,
                        error,
                    ));
                }
                let console_log = match self
                    .adapter
                    .read_console(
                        definition_name,
                        o3k_console::MAX_CONSOLE_BYTES,
                        command.resource_id.clone(),
                    )
                    .await
                {
                    Ok(bytes) if !bytes.is_empty() => Some(ConsoleLogResult {
                        truncated: bytes.len() == o3k_console::MAX_CONSOLE_BYTES,
                        complete: bytes.len() < o3k_console::MAX_CONSOLE_BYTES,
                        offset: 0,
                        bytes,
                    }),
                    Ok(_) => None,
                    Err(error) => {
                        tracing::warn!(%error, server_id = %command.resource_id, "initial console capture failed");
                        None
                    }
                };
                let mut result = success("domain created", resource_state(&inspection))?;
                result.console_log = console_log;
                Ok(result)
            }
            Some(proto::command::Action::ConsoleLog(request)) => {
                if request.offset > 0 {
                    return Err(AgentError::Protocol(
                        "libvirt console snapshots only support offset zero".to_owned(),
                    ));
                }
                let max_bytes = usize::try_from(request.max_bytes)
                    .map_err(|_| AgentError::Protocol("console bound is invalid".to_owned()))?
                    .min(o3k_console::MAX_CONSOLE_BYTES);
                if max_bytes == 0 {
                    return Err(AgentError::Protocol("console bound is invalid".to_owned()));
                }
                tracing::info!(
                    server_id = %command.resource_id,
                    domain = %name,
                    max_bytes,
                    "console inspect start"
                );
                let inspection = self.adapter.inspect(name.clone()).await.map_err(|error| {
                    tracing::warn!(
                        %error,
                        server_id = %command.resource_id,
                        "console inspect failed"
                    );
                    agent_error(error)
                })?;
                verify_owned_domain(&inspection, &command.resource_id).inspect_err(|error| {
                    tracing::warn!(
                        %error,
                        server_id = %command.resource_id,
                        "console ownership verification failed"
                    );
                })?;
                tracing::info!(
                    server_id = %command.resource_id,
                    active = inspection.active,
                    persistent = inspection.persistent,
                    state = %inspection.state,
                    "console inspect end"
                );
                let bytes = self
                    .adapter
                    .read_console(name.clone(), max_bytes, command.resource_id.clone())
                    .await
                    .map_err(|error| {
                        tracing::warn!(
                            %error,
                            server_id = %command.resource_id,
                            "console read failed"
                        );
                        agent_error(error)
                    })?;
                tracing::info!(
                    server_id = %command.resource_id,
                    bytes = bytes.len(),
                    "console read end"
                );
                Ok(CommandExecutionResult {
                    state: proto::OperationState::Succeeded as i32,
                    error_category: proto::ErrorCategory::Unspecified as i32,
                    resource_state: resource_state(&inspection) as i32,
                    redacted_message: "libvirt console output read".to_owned(),
                    provider_resource_id: name,
                    console_log: Some(ConsoleLogResult {
                        truncated: bytes.len() == max_bytes,
                        complete: bytes.len() < max_bytes,
                        offset: 0,
                        bytes,
                    }),
                    block_device: None,
                })
            }
            Some(proto::command::Action::CollectConnector(_)) => {
                let connector = collect_host_connector()?;
                let mut result = success("connector collected", proto::ResourceState::Running)?;
                result.block_device = Some(proto::BlockDeviceObservation {
                    volume_id: String::new(),
                    attachment_id: String::new(),
                    driver_volume_type: String::new(),
                    device_path: String::new(),
                    host_path: String::new(),
                    attached: false,
                    found: true,
                    initiator: connector.initiator.clone().unwrap_or_default(),
                    host_name: connector.host,
                    ip_address: connector.ip,
                    iscsi_logged_in: false,
                });
                Ok(result)
            }
            Some(proto::command::Action::AttachDisk(device)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                if device.driver_volume_type != "iscsi" && device.driver_volume_type != "local" {
                    return Err(AgentError::Protocol(format!(
                        "unsupported driver_volume_type {}",
                        device.driver_volume_type
                    )));
                }
                let host_path = if device.driver_volume_type == "iscsi" {
                    let chap_auth =
                        if device.auth_username.is_empty() || device.auth_password.is_empty() {
                            None
                        } else {
                            Some((device.auth_username.as_str(), device.auth_password.as_str()))
                        };
                    let host_path = iscsi_login(
                        &device.target_iqn,
                        &device.target_portal,
                        device.target_lun,
                        chap_auth,
                    )?;
                    host_path.ok_or_else(|| {
                        AgentError::Protocol(
                            "iscsi login succeeded but no device path was observed".to_owned(),
                        )
                    })?
                } else {
                    device.device_path.clone()
                };
                let guest_device = attach_device_letter(&command.resource_id, &device.volume_id);
                // Idempotent hotplug: a concurrent attach or a reconciler
                // resume may already have hotplugged the disk. If the durable
                // o3k-<uuid> disk serial is present, skip the attach and
                // report success rather than failing with "device already
                // exists".
                if self
                    .adapter
                    .observe_disk(name.clone(), device.volume_id.clone())
                    .await
                    .unwrap_or(false)
                {
                    let host_path = host_path.clone();
                    let mut result =
                        success("block device attached", proto::ResourceState::Running)?;
                    result.block_device = Some(proto::BlockDeviceObservation {
                        volume_id: device.volume_id.clone(),
                        attachment_id: device.attachment_id.clone(),
                        driver_volume_type: device.driver_volume_type.clone(),
                        device_path: format!("/dev/{guest_device}"),
                        host_path,
                        attached: true,
                        found: true,
                        initiator: device.initiator.clone(),
                        host_name: String::new(),
                        ip_address: String::new(),
                        iscsi_logged_in: device.driver_volume_type == "iscsi",
                    });
                    return Ok(result);
                }
                if let Err(error) = self
                    .adapter
                    .attach_disk(
                        name.clone(),
                        device.volume_id.clone(),
                        device.attachment_id.clone(),
                        host_path.clone(),
                        guest_device.clone(),
                    )
                    .await
                {
                    // The hotplug raced with a concurrent attach: verify by the
                    // durable ownership metadata before failing.
                    if self
                        .adapter
                        .observe_disk(name.clone(), device.volume_id.clone())
                        .await
                        .unwrap_or(false)
                    {
                        tracing::info!(
                            volume_id = %device.volume_id,
                            "disk already hotplugged by a concurrent attach; treating as success"
                        );
                    } else {
                        return Err(agent_error(error));
                    }
                }
                let mut result = success("block device attached", proto::ResourceState::Running)?;
                result.block_device = Some(proto::BlockDeviceObservation {
                    volume_id: device.volume_id.clone(),
                    attachment_id: device.attachment_id.clone(),
                    driver_volume_type: device.driver_volume_type.clone(),
                    device_path: format!("/dev/{guest_device}"),
                    host_path,
                    attached: true,
                    found: true,
                    initiator: device.initiator.clone(),
                    host_name: String::new(),
                    ip_address: String::new(),
                    iscsi_logged_in: device.driver_volume_type == "iscsi",
                });
                Ok(result)
            }
            Some(proto::command::Action::DetachDisk(device)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                let detached = self
                    .adapter
                    .detach_disk(name.clone(), device.volume_id.clone())
                    .await
                    .map_err(agent_error)?;
                if device.driver_volume_type == "iscsi" {
                    let _ = iscsi_logout(&device.target_iqn, &device.target_portal);
                }
                let mut result = success("block device detached", proto::ResourceState::Running)?;
                result.block_device = Some(proto::BlockDeviceObservation {
                    volume_id: device.volume_id.clone(),
                    attachment_id: device.attachment_id.clone(),
                    driver_volume_type: device.driver_volume_type.clone(),
                    device_path: String::new(),
                    host_path: String::new(),
                    attached: false,
                    found: detached,
                    initiator: device.initiator.clone(),
                    host_name: String::new(),
                    ip_address: String::new(),
                    iscsi_logged_in: false,
                });
                Ok(result)
            }
            Some(proto::command::Action::ObserveDisk(observe)) => {
                let inspection = self
                    .adapter
                    .inspect(name.clone())
                    .await
                    .map_err(agent_error)?;
                verify_owned_domain(&inspection, &command.resource_id)?;
                let attached = self
                    .adapter
                    .observe_disk(name.clone(), observe.volume_id.clone())
                    .await
                    .map_err(agent_error)?;
                let mut result = success(
                    if attached {
                        "disk is attached"
                    } else {
                        "disk is not attached"
                    },
                    proto::ResourceState::Running,
                )?;
                result.block_device = Some(proto::BlockDeviceObservation {
                    volume_id: observe.volume_id.clone(),
                    attachment_id: observe.attachment_id.clone(),
                    driver_volume_type: String::new(),
                    device_path: String::new(),
                    host_path: String::new(),
                    attached,
                    found: attached,
                    initiator: String::new(),
                    host_name: String::new(),
                    ip_address: String::new(),
                    iscsi_logged_in: attached,
                });
                Ok(result)
            }
            None => Err(AgentError::Protocol("command action is missing".to_owned())),
        }
    }
}

fn inspect_not_found_result(provider_resource_id: String) -> CommandExecutionResult {
    CommandExecutionResult {
        state: proto::OperationState::Failed as i32,
        error_category: proto::ErrorCategory::NotFound as i32,
        resource_state: proto::ResourceState::Error as i32,
        redacted_message: "requested domain was not found".to_owned(),
        provider_resource_id,
        console_log: None,
        block_device: None,
    }
}

/// Builds the definitive (absence-proven) terminal failure result for a
/// create that failed before libvirt could define the domain. The control
/// plane terminalizes this outcome as Failed and later completes the delete
/// locally through the never-reached-provider path — no agent delete command
/// is ever dispatched — so the resource's committed config-drive transfer
/// state would otherwise leak (issue #88 C6). The resource's owned
/// config-drive artifacts are therefore reaped here, best-effort: a failed
/// reap is logged and never changes the command outcome. This is the ONLY
/// create path that reaps; unknown-outcome and retryable failures never
/// reach it, so a retried create still finds its committed manifests.
fn definitive_create_failure_result(
    artifact_root: &std::path::Path,
    agent_id: &str,
    resource_id: &str,
    operation_id: &str,
    error: AgentError,
) -> Result<CommandExecutionResult, AgentError> {
    // The redacted reason is also carried in the result so the control plane
    // can persist it; log the same redacted string here so host-side
    // diagnosis does not require the durable store.
    tracing::warn!(
        error = %error,
        operation_id = %operation_id,
        resource_id = %resource_id,
        "create failed definitively; reporting terminal failure"
    );
    reap_config_drive_artifacts(artifact_root, agent_id, resource_id);
    Ok(definitive_failure_result(&error))
}

/// Result for a create failure that provably happened before libvirt could
/// define the domain (issue-87 C-1 qemu-img materialization, network
/// preparation, domain-spec and console-log failures). Absence is proven by
/// construction: every caller is upstream of the define/start boundary, so
/// the instance can never exist and the operation is terminally Failed
/// rather than of unknown outcome. The category reports the absence so the
/// control plane can recognize that no provider side effect can exist and
/// complete a local delete; the redacted reason is carried in the message
/// for the durable record.
fn definitive_failure_result(error: &AgentError) -> CommandExecutionResult {
    CommandExecutionResult {
        state: proto::OperationState::Failed as i32,
        error_category: proto::ErrorCategory::NotFound as i32,
        resource_state: proto::ResourceState::Error as i32,
        redacted_message: error.to_string(),
        provider_resource_id: String::new(),
        console_log: None,
        block_device: None,
    }
}

/// Result for a create that failed before libvirt could define the domain
/// because a REQUIRED COMMITTED ARTIFACT was missing (issue #611,
/// ASR-021 agent-control-plane-network-interruption). Absence is proven by
/// construction (the failure is upstream of the define/start boundary), but
/// the missing artifact is a control-plane delivery problem the create
/// re-drive can fix by re-offering the transfer — so the outcome is UNKNOWN,
/// never terminal. The reconciler's unknown-outcome recovery re-drives the
/// create, and the provider's transfer loop re-offers the missing artifact.
fn unknown_create_outcome_result(
    artifact_root: &std::path::Path,
    agent_id: &str,
    resource_id: &str,
    error: AgentError,
) -> Result<CommandExecutionResult, AgentError> {
    tracing::warn!(
        error = %error,
        resource_id = %resource_id,
        "create could not resolve committed artifacts; reporting an unknown outcome"
    );
    reap_config_drive_artifacts(artifact_root, agent_id, resource_id);
    Ok(CommandExecutionResult {
        state: proto::OperationState::UnknownOutcome as i32,
        error_category: proto::ErrorCategory::UnknownOutcome as i32,
        resource_state: proto::ResourceState::Error as i32,
        redacted_message: error.to_string(),
        provider_resource_id: String::new(),
        console_log: None,
        block_device: None,
    })
}

/// Result for a create rejected by the agent's disk-capacity backstop
/// (issue #606). The rejection provably happens before any host mutation, so
/// the capacity classification mirrors the placement gate's rejection and the
/// control plane persists the same durable `capacity` category.
fn capacity_failure_result(disk_gib: u64, max_disk_gb: u64) -> CommandExecutionResult {
    CommandExecutionResult {
        state: proto::OperationState::Failed as i32,
        error_category: proto::ErrorCategory::Capacity as i32,
        resource_state: proto::ResourceState::Error as i32,
        redacted_message: format!(
            "create requires {disk_gib} GiB disk but the agent capacity is {max_disk_gb} GiB"
        ),
        provider_resource_id: String::new(),
        console_log: None,
        block_device: None,
    }
}

/// The create command's resolved disk demand, or `None` when the command is
/// not a create or carries no resolved inputs. Pure protobuf read: no host
/// state is touched, so the create arm can evaluate it before any mutation.
fn create_disk_gib(command: &proto::Command) -> Option<u64> {
    let proto::command::Action::Create(create) = command.action.as_ref()? else {
        return None;
    };
    create.resolved.as_ref().map(|resolved| resolved.disk_gib)
}

fn resource_state(inspection: &o3k_libvirt::DomainInspection) -> proto::ResourceState {
    match o3k_libvirt::project_domain_state(inspection.active, &inspection.state) {
        o3k_provider::InstanceState::Running => proto::ResourceState::Running,
        o3k_provider::InstanceState::Stopped => proto::ResourceState::Stopped,
        o3k_provider::InstanceState::Creating => proto::ResourceState::Creating,
        o3k_provider::InstanceState::Deleting => proto::ResourceState::Deleting,
        o3k_provider::InstanceState::Deleted => proto::ResourceState::Deleted,
        o3k_provider::InstanceState::Error => proto::ResourceState::Error,
    }
}

fn verify_owned_domain(
    inspection: &o3k_libvirt::DomainInspection,
    expected_server_id: &str,
) -> Result<(), AgentError> {
    match o3k_libvirt::discover_domain_xml(&inspection.name, &inspection.xml) {
        o3k_libvirt::DiscoveryResult::Owned { metadata, .. }
            if metadata.server_id == expected_server_id =>
        {
            Ok(())
        }
        _ => Err(AgentError::Protocol(
            "libvirt domain ownership verification failed".to_owned(),
        )),
    }
}

fn agent_error(_error: o3k_libvirt::LibvirtError) -> AgentError {
    AgentError::Protocol("libvirt command failed".to_owned())
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(|value| normalized_hostname(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "o3k-compute".to_owned())
}

fn normalized_hostname(value: &str) -> Option<String> {
    let value =
        value.trim_matches(|character: char| character == '\0' || character.is_whitespace());
    if value.is_empty()
        || value.len() > 253
        || value.chars().any(|character| character.is_control())
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn read_first_ip() -> String {
    match std::process::Command::new("hostname").arg("-I").output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .split_whitespace()
                .next()
                .unwrap_or("127.0.0.1")
                .to_owned()
        }
        _ => "127.0.0.1".to_owned(),
    }
}

fn read_iscsi_initiator() -> Option<String> {
    let contents = std::fs::read_to_string("/etc/iscsi/initiatorname.iscsi").ok()?;
    contents
        .lines()
        .find_map(|line| line.strip_prefix("InitiatorName="))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Collects the compute connector description required by the Cinder
/// connector-update flow. Bounded, non-secret values only.
fn collect_host_connector() -> Result<o3k_provider::ConnectorInfo, AgentError> {
    Ok(o3k_provider::ConnectorInfo {
        host: read_hostname(),
        ip: read_first_ip(),
        platform: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        os_type: std::env::consts::OS.to_owned(),
        multipath: false,
        initiator: read_iscsi_initiator(),
    })
}

fn iscsiadm_command() -> std::process::Command {
    if std::env::var_os("O3K_ISCSIADM_SUDO").is_some_and(|value| value == "1") {
        let mut command = std::process::Command::new("sudo");
        command.args(["--non-interactive", "--", "/usr/sbin/iscsiadm"]);
        command
    } else {
        std::process::Command::new("iscsiadm")
    }
}

/// Logs into the iSCSI target and returns the observed host device path. A
/// missing iscsiadm is an explicit unsupported-connector failure; a successful
/// login without an observed device is an unknown outcome. The node record is
/// created when absent (os-brick sequence: show, then `--op new`), and
/// optional CHAP credentials are applied to the node session before login;
/// credentials are never logged (the redacted-message contract forbids
/// logging command arguments or raw iscsiadm output).
fn iscsi_login(
    target_iqn: &str,
    target_portal: &str,
    _target_lun: u32,
    chap_auth: Option<(&str, &str)>,
) -> Result<Option<String>, AgentError> {
    if target_iqn.trim().is_empty() || target_portal.trim().is_empty() {
        return Err(AgentError::Protocol(
            "iscsi target is incomplete".to_owned(),
        ));
    }
    // The node record must exist before CHAP settings can be applied. Show
    // the node first (os-brick tolerates "no records found") and create the
    // record only when it is absent.
    let node_show = iscsiadm_command()
        .args(["--mode", "node", "-T", target_iqn, "-p", target_portal])
        .output();
    match node_show {
        Ok(output) if output.status.success() => {}
        Ok(_) | Err(_) => {
            let node_new = iscsiadm_command()
                .args([
                    "--mode",
                    "node",
                    "-T",
                    target_iqn,
                    "-p",
                    target_portal,
                    "--op",
                    "new",
                ])
                .output();
            match node_new {
                Ok(output) if output.status.success() => {}
                Ok(_) => {
                    // The redacted-message contract forbids raw command
                    // output: iscsiadm stderr can disclose target details.
                    tracing::debug!("iscsi node create command exited unsuccessfully");
                    return Err(AgentError::Protocol("iscsi node create failed".to_owned()));
                }
                Err(_) => {
                    return Err(AgentError::Protocol(
                        "iscsiadm is not available; iscsi connector is unsupported on this host"
                            .to_owned(),
                    ));
                }
            }
        }
    }
    if let Some((username, password)) = chap_auth {
        // Apply CHAP to the node session before login. Credentials are passed
        // as arguments and never logged or echoed into errors.
        let update = iscsiadm_command()
            .args([
                "--mode",
                "node",
                "-T",
                target_iqn,
                "-p",
                target_portal,
                "--op",
                "update",
                "-n",
                "node.session.auth.authmethod",
                "-v",
                "CHAP",
                "-n",
                "node.session.auth.username",
                "-v",
                username,
                "-n",
                "node.session.auth.password",
                "-v",
                password,
            ])
            .output();
        match update {
            Ok(output) if output.status.success() => {}
            Ok(_) => {
                tracing::debug!("iscsi CHAP update command exited unsuccessfully");
                return Err(AgentError::Protocol("iscsi CHAP update failed".to_owned()));
            }
            Err(_) => {
                return Err(AgentError::Protocol("iscsiadm is not available".to_owned()));
            }
        }
    }
    let login = iscsiadm_command()
        .args([
            "--mode",
            "node",
            "-T",
            target_iqn,
            "-p",
            target_portal,
            "--login",
        ])
        .output();
    match login {
        // Exit 15 means the session already exists, which is a successful
        // login (os-brick behavior).
        Ok(output) if output.status.success() || output.status.code() == Some(15) => {
            // Determine the host device path from the login session output.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let session = stdout
                .lines()
                .find_map(|line| line.split("with session").nth(1))
                .map(|value| value.trim().to_owned());
            for _ in 0..10 {
                let device = iscsiadm_command()
                    .args(["--mode", "session", "-P", "3"])
                    .output()
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
                    .unwrap_or_default();
                let device = discover_iscsi_device(&device, target_iqn);
                if let Some(device) = device {
                    return Ok(Some(device));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            // A successful login with an unknown device path is an unknown
            // outcome, never an automatic failure.
            tracing::warn!(
                target_iqn,
                session = ?session,
                "iscsi login succeeded but device path is not yet observable"
            );
            Ok(None)
        }
        Ok(output) => {
            tracing::debug!(
                status = ?output.status,
                "iscsi login command exited unsuccessfully"
            );
            Err(AgentError::Protocol("iscsi login failed".to_owned()))
        }
        Err(_) => Err(AgentError::Protocol("iscsiadm is not available".to_owned())),
    }
}

fn discover_iscsi_device(session_output: &str, target_iqn: &str) -> Option<String> {
    let lines: Vec<&str> = session_output.lines().collect();
    let mut in_target = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("Target:") && trimmed.contains(target_iqn) {
            in_target = true;
            continue;
        }
        if in_target {
            if let Some(dev_name) = trimmed
                .strip_prefix("Attached scsi disk ")
                .and_then(|s| s.split_whitespace().next())
            {
                let path = format!("/dev/{dev_name}");
                if std::path::Path::new(&path).exists() {
                    return Some(path);
                }
            }
            if trimmed.starts_with("/dev/") {
                let dev_path = trimmed.split_whitespace().next().unwrap_or(trimmed);
                if std::path::Path::new(dev_path).exists() {
                    return Some(dev_path.to_owned());
                }
            }
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.starts_with("Target:") {
                break;
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir("/dev/disk/by-path") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(target_iqn) {
                return Some(entry.path().to_string_lossy().to_string());
            }
        }
    }
    None
}

fn iscsi_logout(target_iqn: &str, target_portal: &str) -> Result<(), AgentError> {
    let _ = target_portal;
    let logout = iscsiadm_command()
        .args(["--mode", "node", "-T", target_iqn, "--logout"])
        .output();
    match logout {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => Ok(()),
        Err(_) => Ok(()),
    }
}

/// Determines the next guest virtio block-device letter for a server. The
/// letter is stable for the volume on this server via a deterministic UUID
/// mapping over the bounded b..=z alphabet.
fn attach_device_letter(resource_id: &str, volume_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    resource_id.hash(&mut hasher);
    volume_id.hash(&mut hasher);
    let index = (hasher.finish() % 24) as u8;
    let letter = (b'b' + index) as char;
    format!("vd{letter}")
}

impl LibvirtCommandExecutor {
    /// Polls until the domain is inactive or the bounded wait expires.
    async fn wait_for_domain_inactive(
        &self,
        name: String,
        resource_id: &str,
    ) -> Result<o3k_libvirt::DomainInspection, AgentError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let inspection = self
                .adapter
                .inspect(name.clone())
                .await
                .map_err(agent_error)?;
            verify_owned_domain(&inspection, resource_id)?;
            if !inspection.active {
                return Ok(inspection);
            }
            if std::time::Instant::now() >= deadline {
                return Err(AgentError::Protocol(
                    "domain did not stop within the bounded wait".to_owned(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// Startup DHCP reconciliation for persisted bindings (issue #87 S3 rerun
/// #5). The caller must treat a failure as a logged, non-fatal condition:
/// the agent has to stay up (control-plane connection, journal replay) even
/// when DHCP cannot start at boot, and DHCP is retried on the next restart
/// or the next create. Create-time DHCP failures stay fail-closed in
/// [`DhcpRuntime::apply`]; only the boot reconciliation may fail softly.
fn reconcile_dhcp_on_startup(
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    network: &o3k_network::HostNetworkManager,
) -> Result<(), String> {
    dhcp.lock()
        .map_err(|_| "DHCP runtime lock is poisoned".to_owned())?
        .start_after_restart(network)
        .map_err(|error| format!("DHCP reconciliation failed: {error}"))
}

/// Bounded window for the startup domain restoration. A host reboot leaves
/// every qemu domain defined but inactive; the restore observes each owned
/// domain, starts the ones whose last lifecycle mutation provably left them
/// running, and retries failed attempts inside this window (libvirtd can
/// still be accepting its first connections when the agent starts). Startup
/// never blocks on the restore: the pass is best-effort and re-runs on the
/// next agent restart.
const STARTUP_DOMAIN_RESTORE_WINDOW: Duration = Duration::from_secs(60);
const STARTUP_DOMAIN_RESTORE_RETRY: Duration = Duration::from_millis(1000);

/// Observe-and-restore port for one O3K-owned domain. The real adapter
/// classifies libvirt outcomes; tests inject fakes without the libvirt
/// feature.
#[async_trait]
trait StartupDomainRestore: Send + Sync {
    /// Observes the owned domain and, when it is defined but inactive,
    /// starts it and confirms the start with a second inspection. Returns
    /// `Ok(true)` when a restore happened, `Ok(false)` when none was needed
    /// (the domain is absent or already active), and `Err` when the outcome
    /// is unknown (retried by the bounded window).
    async fn restore_owned_domain(&self, resource_id: &str) -> Result<bool, AgentError>;
}

#[async_trait]
impl StartupDomainRestore for LibvirtAdapter {
    async fn restore_owned_domain(&self, resource_id: &str) -> Result<bool, AgentError> {
        let name = stable_domain_name(resource_id);
        let inspection = match self.inspect(name.clone()).await {
            Ok(inspection) => inspection,
            Err(error) if error.category == ErrorCategory::NotFound => return Ok(false),
            Err(error) => return Err(agent_error(error)),
        };
        verify_owned_domain(&inspection, resource_id)?;
        if inspection.active {
            return Ok(false);
        }
        self.start_owned(name.clone(), resource_id.to_owned())
            .await
            .map_err(agent_error)?;
        let confirmed = self.inspect(name.clone()).await.map_err(agent_error)?;
        verify_owned_domain(&confirmed, resource_id)?;
        Ok(confirmed.active)
    }
}

/// Observe-and-restore port for the owned TAP devices of one O3K-owned
/// instance (issue #613 blocker A): a host reboot deletes the ephemeral TAP
/// devices while the persisted domain XML still references them, so the
/// domain start would fail. The real implementation reuses the create-time
/// [`o3k_network::HostNetworkManager::ensure_tap`] ownership path; tests
/// inject fakes without touching the host network.
#[async_trait]
trait StartupTapRestore: Send + Sync {
    /// Ensures every TAP recorded as O3K-owned for the instance exists and
    /// is attached to the managed bridge before the domain start. An absent
    /// TAP is re-created under the recorded deterministic name, a present
    /// owned TAP is verified and reused, and a foreign link at the recorded
    /// name fails closed without being touched. `Err` means the outcome is
    /// unknown or foreign: the caller must hold back the instance's domain
    /// start and retries inside the bounded window.
    async fn restore_owned_taps(&self, resource_id: &str) -> Result<(), AgentError>;
}

/// Real TAP restoration driven by the durable network ownership manifest.
/// Each recorded spec is re-verified by `ensure_tap` against both the
/// manifest and the kernel, so a forged or stale record can never create or
/// mutate a foreign interface.
struct NetworkStartupTapRestore {
    network: Arc<o3k_network::HostNetworkManager>,
    external_owner: bool,
}

#[async_trait]
impl StartupTapRestore for NetworkStartupTapRestore {
    async fn restore_owned_taps(&self, resource_id: &str) -> Result<(), AgentError> {
        let specs = self
            .network
            .owned_tap_specs_for_instance(resource_id)
            .map_err(|error| AgentError::Protocol(format!("owned TAP lookup failed: {error}")))?;
        for spec in specs {
            if self.external_owner {
                self.network.resolve_owned_tap(&spec).map_err(|error| {
                    AgentError::Protocol(format!("external network TAP is unavailable: {error}"))
                })?;
                continue;
            }
            let (name, created) = self.network.ensure_tap(&spec).map_err(|error| {
                AgentError::Protocol(format!("owned TAP restoration failed: {error}"))
            })?;
            if created {
                tracing::info!(
                    resource_id = %resource_id,
                    tap = %name,
                    "re-created owned TAP during startup domain restoration"
                );
            }
        }
        Ok(())
    }
}

/// Re-reads the durable command journal at the start of every restore pass
/// (stale-snapshot fence, see [`restore_expected_running_domains`]). The
/// real implementation observes the same journal file the control
/// connection writes; tests inject fakes whose snapshot changes between
/// passes.
trait StartupJournalRefresh: Send + Sync {
    fn latest_lifecycle_states(
        &self,
    ) -> Result<std::collections::HashMap<String, (u64, proto::ResourceState)>, AgentError>;
}

/// Journal re-read driven by the durable command journal file. Read-only:
/// the live journal instance owned by the control connection is opened
/// separately by the agent, and this snapshot only ever observes.
struct CommandJournalStartupRefresh {
    identity_path: PathBuf,
    agent_id: String,
}

impl StartupJournalRefresh for CommandJournalStartupRefresh {
    fn latest_lifecycle_states(
        &self,
    ) -> Result<std::collections::HashMap<String, (u64, proto::ResourceState)>, AgentError> {
        o3k_compute_agent::load_journal_lifecycle_resource_states(
            &self.identity_path,
            &self.agent_id,
        )
    }
}

/// Startup restoration of O3K-owned libvirt domains (one-line TestLab host
/// reboot contract, issue #613 blocker A): a host reboot leaves every qemu
/// domain defined but inactive while the control plane's durable server
/// state stays ACTIVE, and no control-plane operation is left non-terminal
/// to re-drive. The agent's command journal records the last lifecycle
/// mutation it executed per resource, so the domains whose last mutation
/// provably left them running are restored here by starting them again.
///
/// Ordering: a reboot also deletes the ephemeral TAP devices, so the owned
/// TAPs recorded in the network ownership manifest are re-created (or
/// ownership-verified and reused) BEFORE the domain start — the persisted
/// domain XML references them with `managed="no"` and libvirt refuses to
/// start without them. The bridge is guaranteed to exist first: the startup
/// DHCP reconciliation ran before this pass, and `ensure_tap` re-ensures
/// the bridge itself.
///
/// Observe-before-mutate: every TAP restoration is ownership-verified
/// against the manifest and the kernel, every start is preceded by an
/// ownership-verified inspection and confirmed by a second inspection, so a
/// retried attempt can never double-start a domain that came up after an
/// unknown outcome. A failed TAP restoration holds back that instance's
/// domain start (fail closed: an unverified or foreign interface must never
/// be mutated, and a start without its TAP cannot succeed). Absent domains
/// are left alone (their convergence is owned by the control plane), and
/// failures are retried inside the bounded window and logged; the agent
/// never fails startup on a restore.
///
/// Stale-snapshot race: the seed snapshot is taken before the control
/// connection starts, so a lifecycle command accepted afterwards (a fresh
/// user stop or start) is invisible to it. At the start of EVERY retry pass
/// the durable journal is re-read and every still-pending resource whose
/// latest terminal lifecycle state is no longer `Running` is dropped (only
/// the per-resource state is re-snapshotted — the pending set itself is
/// never rebuilt from scratch). The re-read closes the race for commands
/// accepted before the pass begins; a stop accepted after the re-read but
/// before that pass's start of the resource can still be undone, so a
/// residual single-pass window remains (the executor and the restore use
/// separate libvirt connections with no mutual exclusion).
async fn restore_expected_running_domains(
    tap_restorer: &dyn StartupTapRestore,
    restorer: &dyn StartupDomainRestore,
    journal_refresh: &dyn StartupJournalRefresh,
    states: &std::collections::HashMap<String, (u64, proto::ResourceState)>,
) -> Result<(), AgentError> {
    restore_expected_running_domains_with_window(
        tap_restorer,
        restorer,
        journal_refresh,
        states,
        STARTUP_DOMAIN_RESTORE_WINDOW,
        STARTUP_DOMAIN_RESTORE_RETRY,
    )
    .await
}

async fn restore_expected_running_domains_with_window(
    tap_restorer: &dyn StartupTapRestore,
    restorer: &dyn StartupDomainRestore,
    journal_refresh: &dyn StartupJournalRefresh,
    states: &std::collections::HashMap<String, (u64, proto::ResourceState)>,
    window: Duration,
    retry: Duration,
) -> Result<(), AgentError> {
    let mut pending: std::collections::BTreeSet<String> = states
        .iter()
        .filter(|(_, (_, state))| *state == proto::ResourceState::Running)
        .map(|(resource_id, _)| resource_id.clone())
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + window;
    let mut first_error = None;
    loop {
        // Stale-snapshot fence: re-read the durable journal before mutating
        // anything this pass. A resource whose fresh last terminal
        // lifecycle state is no longer `Running` was stopped (or deleted)
        // by the control connection since the seed snapshot and must not be
        // re-started. Only the per-resource state is re-snapshotted — the
        // pending set is never rebuilt from scratch, so a resource that
        // already converged or was dropped stays dropped. See the
        // `restore_expected_running_domains` doc comment for the residual
        // single-pass window.
        match journal_refresh.latest_lifecycle_states() {
            Ok(latest) => {
                pending.retain(|resource_id| {
                    latest
                        .get(resource_id)
                        .is_some_and(|(_, state)| *state == proto::ResourceState::Running)
                });
                if pending.is_empty() {
                    return Ok(());
                }
                let mut next = std::collections::BTreeSet::new();
                for resource_id in &pending {
                    if let Err(error) = tap_restorer.restore_owned_taps(resource_id).await {
                        tracing::warn!(
                            resource_id = %resource_id,
                            error = %error,
                            "owned TAP restoration failed; holding back the domain start and \
                             retrying inside the startup window"
                        );
                        next.insert(resource_id.clone());
                        first_error.get_or_insert(error);
                        continue;
                    }
                    match restorer.restore_owned_domain(resource_id).await {
                        Ok(true) => {
                            tracing::info!(
                                resource_id = %resource_id,
                                "restored O3K-owned domain to running"
                            );
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(
                                resource_id = %resource_id,
                                error = %error,
                                "domain restore attempt failed; retrying inside the startup window"
                            );
                            next.insert(resource_id.clone());
                            first_error.get_or_insert(error);
                        }
                    }
                }
                pending = next;
                if pending.is_empty() {
                    return Ok(());
                }
            }
            Err(error) => {
                // Fail closed: without a fresh journal snapshot the last
                // lifecycle state cannot be proven, so this pass mutates
                // nothing (no TAP restoration, no domain start) and retries
                // inside the window.
                tracing::warn!(
                    error = %error,
                    "command journal could not be re-read before the restore pass; \
                     holding back the pass and retrying inside the startup window"
                );
                first_error.get_or_insert(error);
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(retry).await;
    }
    if let Some(error) = first_error {
        tracing::warn!(
            pending = pending.len(),
            error = %error,
            "startup domain restoration did not converge within the startup window; \
             retried on the next agent restart"
        );
        Err(error)
    } else {
        Ok(())
    }
}

/// Startup residue cleanup for crash residue (issue #87 S3 rerun #5 and
/// issue #88 S3/S4 reruns): the stale-network reap removes the persisted
/// DHCP bindings and TAPs of instances whose domains provably do not exist,
/// and the owned-dnsmasq reap stops every owned dnsmasq left behind by a
/// previous agent process. The provisional-link reap removes `o3ktmp-*` TAPs
/// and `o3kbm-*` bridges left by a create that died before its ownership
/// record became durable (issues #602, #608); such links are self-identifying
/// residue — no manifest record or domain ever references a provisional name
/// — so deleting them needs no manifest proof and cannot touch a fenced
/// deterministic `o3ktap-`/`o3k-b*` interface.
/// Ordering invariant: the provisional-link reap runs first (it is independent
/// of instance state), then the stale-network reap MUST run (a crashed create
/// whose DHCP prep completed persists its binding, so the stale binding must
/// not survive to be re-served), then the owned-dnsmasq reap (at startup the
/// supervisor is always None, so every owned dnsmasq is a leftover regardless
/// of bindings), then live bindings get a fresh supervisor in
/// [`reconcile_dhcp_on_startup`]. Errors are logged and never fatal, so
/// residue is retried on the next restart; startup is never blocked by an
/// unreachable or unknown libvirt.
async fn reap_startup_residue(
    network: &o3k_network::HostNetworkManager,
    dhcp: &Arc<Mutex<DhcpRuntime>>,
    presence: &dyn DomainPresence,
) -> Result<(), AgentError> {
    let partial_error = network
        .reap_partial_links()
        .map_err(|error| AgentError::Protocol(format!("provisional link reap failed: {error}")))
        .err();
    let stale_error = reap_stale_instance_networks(network, dhcp, presence)
        .await
        .err();
    let reap_error = dhcp
        .lock()
        .map_err(|_| AgentError::Protocol("DHCP runtime lock is poisoned".to_owned()))
        .and_then(|runtime| runtime.reap_owned_dnsmasq())
        .err();
    match (partial_error, stale_error, reap_error) {
        (Some(error), _, _) | (None, Some(error), _) | (None, None, Some(error)) => Err(error),
        (None, None, None) => Ok(()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = config_from_env()?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let libvirt = LibvirtAdapter::new(LibvirtConfig::default())?;
    let (libvirt_ready, libvirt_error) = match libvirt.capabilities().await {
        Ok(capabilities) => {
            let max_disk_gb = config.capabilities.max_disk_gb;
            config.capabilities = o3k_provider_contract::compute_proto::Capabilities {
                max_disk_gb,
                ..capabilities.to_protocol_capabilities()
            };
            (true, None)
        }
        Err(error) => {
            let message = error.to_string();
            tracing::warn!(error = %message, "local libvirt is unavailable");
            (false, Some(message))
        }
    };
    let agent = AgentClient::new(config.clone())?;
    let agent_id = agent.load_identity()?;
    let artifact_root = agent.identity_file().with_extension("artifacts");
    let network_root = env::var_os("O3K_COMPUTE_NETWORK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            agent
                .identity_file()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("network")
        });
    let network = Arc::new(o3k_network::HostNetworkManager::with_ownership_root(
        o3k_network::HostNetworkConfig {
            bridge_name: env::var("O3K_COMPUTE_BRIDGE_NAME")
                .unwrap_or_else(|_| "o3k-br0".to_owned()),
            uplink: env::var("O3K_COMPUTE_UPLINK").ok(),
        },
        network_root.clone(),
    )?);
    let network_owned_by_external_agent = matches!(
        env::var("O3K_COMPUTE_NETWORK_EXTERNAL").as_deref(),
        Ok("1" | "true" | "yes")
    );
    let bridge_name = env::var("O3K_COMPUTE_BRIDGE_NAME").unwrap_or_else(|_| "o3k-br0".to_owned());
    let service_root = agent
        .identity_file()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let dhcp = Arc::new(Mutex::new(DhcpRuntime::open(
        service_root.join("dhcp"),
        env::var("O3K_COMPUTE_DHCP_BINARY").unwrap_or_else(|_| "dnsmasq".to_owned()),
        bridge_name.clone(),
    )?));
    // Startup residue cleanup (issue #87 S3 rerun #5, issue #88 S3/S4
    // reruns): the stale-network reap removes the persisted DHCP bindings
    // and TAPs of instances whose domains provably do not exist FIRST, then
    // the owned-dnsmasq reap stops EVERY owned dnsmasq — at startup the
    // supervisor is always None, so any owned dnsmasq is a leftover of a
    // previous process regardless of bindings (a live-bound orphan would
    // hold the DHCP socket and block the fresh supervisor). Live bindings
    // then get their fresh supervisor below. Errors are logged and retried
    // on the next restart; startup is never blocked.
    if !network_owned_by_external_agent
        && let Err(error) = reap_startup_residue(&network, &dhcp, &libvirt).await
    {
        tracing::warn!(
            error = %error,
            "startup residue reap failed; retried on the next restart"
        );
    }
    // Reap incomplete-transfer `.part` files that can never be resumed
    // (issue #88 S5 supplementary): a crashed agent's part survives its
    // restart and the resource delete (the delete arm reaps config-drive
    // artifacts only), and the control plane expires the abandoned transfer
    // row (#571) without ever resuming it. The rule mirrors
    // `artifact_statuses`: a part with no manifest or an expired incomplete
    // transfer is an orphan; a non-expired incomplete transfer is resumed
    // with the SAME transfer id after reconnect and its part is kept.
    // Best-effort and never fatal; the inventory catches residue.
    reap_orphaned_transfer_parts(&artifact_root, &agent_id, None);
    // A DHCP that cannot start at boot (missing capabilities, a port
    // conflict, the host's own dnsmasq on 127.0.0.1:53, ...) must not take
    // the agent down: the failure is logged, the agent stays up, and DHCP
    // is retried on the next restart or the next create. Create-time DHCP
    // failures remain fail-closed in DhcpRuntime::apply.
    if !network_owned_by_external_agent
        && let Err(error) = reconcile_dhcp_on_startup(&dhcp, &network)
    {
        tracing::warn!(
            error = %error,
            "DHCP reconciliation failed at startup; the agent stays up and \
             retries on the next restart or create"
        );
    }
    // Host-reboot restoration (issue #613 blocker A): domains whose last
    // lifecycle mutation provably left them running are started again inside
    // a bounded window. The restore runs as a task next to the control
    // connection so a slow restore can never delay agent registration, and
    // it only ever starts domains the agent's own journal proves were
    // running. A failed restore is logged and retried on the next restart.
    let restore_states = match o3k_compute_agent::load_journal_lifecycle_resource_states(
        agent.identity_file(),
        &agent_id,
    ) {
        Ok(states) => states,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "command journal could not be read for domain restoration; \
                 restoration skipped for this start"
            );
            std::collections::HashMap::new()
        }
    };
    let restore_journal_refresh = CommandJournalStartupRefresh {
        identity_path: agent.identity_file().to_path_buf(),
        agent_id: agent_id.clone(),
    };
    let restore_task = tokio::spawn({
        let adapter = libvirt.clone();
        let network = Arc::clone(&network);
        async move {
            let tap_restorer = NetworkStartupTapRestore {
                network,
                external_owner: network_owned_by_external_agent,
            };
            // The unconverged outcome is logged once inside the restore pass
            // (with the pending count); the returned error needs no second
            // log site here.
            let _ = restore_expected_running_domains(
                &tap_restorer,
                &adapter,
                &restore_journal_refresh,
                &restore_states,
            )
            .await;
        }
    });
    let executor = Arc::new(LibvirtCommandExecutor {
        adapter: libvirt.clone(),
        artifact_root,
        image_materializer: o3k_compute_agent::ImageMaterializer::open(
            o3k_compute_agent::ArtifactStore::open(
                agent.identity_file().with_extension("artifacts"),
                agent_id.clone(),
            )?,
            service_root.join("image-cache"),
            2 * 1024 * 1024 * 1024,
        )?,
        network,
        dhcp,
        max_disk_gb: config.capabilities.max_disk_gb,
        network_owned_by_external_agent,
    });
    info!(
        endpoint = %config.endpoint,
        host_label = %config.host_label,
        bridge = %bridge_name,
        network_root = %network_root.display(),
        network_owned_by_external_agent,
        "o3k-compute starting"
    );
    let health_addr = env::var("O3K_COMPUTE_HEALTH_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:9100".to_owned())
        .parse::<SocketAddr>()?;
    let state = HealthState {
        agent: agent.clone(),
        agent_id: agent_id.clone(),
        software_version: config.software_version.clone(),
        capabilities: config.capabilities.clone(),
        libvirt_ready,
        libvirt_error,
    };
    let health_server = axum::serve(TcpListener::bind(health_addr).await?, health_router(state));
    tokio::select! {
        result = agent.run_with_executor(shutdown_signal(), executor) => {
            restore_task.abort();
            let _ = restore_task.await;
            result?;
        }
        result = health_server.with_graceful_shutdown(shutdown_signal()) => {
            restore_task.abort();
            let _ = restore_task.await;
            result?;
        }
    }
    info!("o3k-compute stopped");
    Ok(())
}

fn health_router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/metrics", get(metrics))
        .with_state(state)
}

async fn liveness() -> impl IntoResponse {
    (StatusCode::OK, "{\"status\":\"alive\"}\n")
}

/// The 200 `/readyz` body (issue #617): the existing `status` key plus the
/// agent's live, loopback-only identity. `agent_epoch` is omitted until the
/// control plane has validated the current registration — a ready agent has
/// always published one first, but the field stays `skip_serializing_if None`
/// so the doctor can parse leniently. No secrets.
#[derive(serde::Serialize)]
struct ReadyBody {
    status: &'static str,
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_epoch: Option<String>,
    software_version: String,
    capabilities: ReadyCapabilities,
}

#[derive(serde::Serialize)]
struct ReadyCapabilities {
    max_vcpus: u32,
    max_memory_mib: u64,
    max_disk_gb: u64,
}

async fn readiness(State(state): State<HealthState>) -> impl IntoResponse {
    if state.agent.is_ready() && state.libvirt_ready {
        let body = ReadyBody {
            status: "ready",
            agent_id: state.agent_id.clone(),
            agent_epoch: state.agent.current_epoch().await,
            software_version: state.software_version.clone(),
            capabilities: ReadyCapabilities {
                max_vcpus: state.capabilities.max_vcpus,
                max_memory_mib: state.capabilities.max_memory_mib,
                max_disk_gb: state.capabilities.max_disk_gb,
            },
        };
        let body = match serde_json::to_string(&body) {
            Ok(serialized) => serialized,
            // Plain string/integer fields cannot fail JSON serialization;
            // never panic on the health path.
            Err(_) => "{\"status\":\"ready\"}".to_owned(),
        };
        (StatusCode::OK, format!("{body}\n"))
    } else {
        let error = state
            .libvirt_error
            .as_deref()
            .unwrap_or("control plane is not connected");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{{\"status\":\"not_ready\",\"reason\":{}}}\n",
                serde_json::to_string(error).unwrap_or_else(|_| "\"unavailable\"".to_owned())
            ),
        )
    }
}

async fn metrics(State(state): State<HealthState>) -> impl IntoResponse {
    let ready = u8::from(state.agent.is_ready() && state.libvirt_ready);
    (StatusCode::OK, format!("o3k_compute_ready {ready}\n"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

fn config_from_env() -> Result<AgentConfig, Box<dyn std::error::Error>> {
    let data_dir = PathBuf::from(
        env::var("O3K_COMPUTE_DATA_DIR").unwrap_or_else(|_| "./compute-data".to_owned()),
    );
    let endpoint = env::var("O3K_COMPUTE_CONTROL_ENDPOINT")
        .unwrap_or_else(|_| "https://127.0.0.1:50051".to_owned());
    let server_name =
        env::var("O3K_COMPUTE_SERVER_NAME").unwrap_or_else(|_| "o3k-control-plane".to_owned());
    let host_label =
        env::var("O3K_COMPUTE_HOST_LABEL").unwrap_or_else(|_| "compute-host".to_owned());
    let software_version =
        env::var("O3K_COMPUTE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    let tls_dir = PathBuf::from(
        env::var("O3K_COMPUTE_TLS_DIR").unwrap_or_else(|_| "./compute-tls".to_owned()),
    );
    Ok(AgentConfig {
        endpoint,
        server_name,
        tls: TlsFiles {
            ca_certificate: tls_dir.join("ca.pem"),
            certificate: tls_dir.join("agent.pem"),
            private_key: tls_dir.join("agent-key.pem"),
        },
        identity_file: data_dir.join("agent-id"),
        host_label,
        software_version,
        heartbeat_interval: Duration::from_secs(5),
        max_reconnect_delay: Duration::from_secs(30),
        capabilities: o3k_provider_contract::compute_proto::Capabilities {
            architecture: env::consts::ARCH.to_owned(),
            agent_provider_name: "o3k-compute".to_owned(),
            agent_provider_version: env!("CARGO_PKG_VERSION").to_owned(),
            max_disk_gb: env::var("O3K_COMPUTE_MAX_DISK_GB")
                .unwrap_or_else(|_| "0".to_owned())
                .parse()?,
            ..Default::default()
        },
    })
}


#[cfg(test)]
mod tests;
