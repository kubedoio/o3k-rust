use super::*;

pub(crate) fn load_state(path: &Path) -> Result<ProviderState, LinuxFabricError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| LinuxFabricError::CorruptState),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ProviderState::default()),
        Err(error) => Err(LinuxFabricError::Storage(error)),
    }
}

pub(crate) fn load_plans(
    path: &Path,
) -> Result<BTreeMap<Uuid, NamespacedRoutedFabricPlan>, LinuxFabricError> {
    let mut plans = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() || !entry.file_type()?.is_file() {
            return Err(LinuxFabricError::ForeignState);
        }
        let plan: NamespacedRoutedFabricPlan = serde_json::from_slice(&fs::read(entry.path())?)
            .map_err(|_| LinuxFabricError::CorruptState)?;
        plans.insert(plan.realm_id, plan);
    }
    Ok(plans)
}

pub(crate) fn store_state(path: &Path, state: &ProviderState) -> Result<(), LinuxFabricError> {
    store_json(path, state)
}

pub(crate) fn store_plan(
    path: &Path,
    plan: &NamespacedRoutedFabricPlan,
) -> Result<(), LinuxFabricError> {
    store_json(path, plan)
}

pub(crate) fn store_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LinuxFabricError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| LinuxFabricError::CorruptState)?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn write_private_key(
    path: &Path,
    command: &Arc<dyn LinuxFabricCommand>,
) -> Result<(), LinuxFabricError> {
    if path.exists() {
        return validate_private_key_file(path);
    }
    let (success, key) = command
        .output("wg", &["genkey"])
        .map_err(LinuxFabricError::Storage)?;
    if !success || !valid_wireguard_key(key.trim()) || key.lines().count() != 1 {
        return Err(LinuxFabricError::CommandFailed);
    }
    let mut file = fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file.mode(0o600);
    }
    let mut file = file.open(path)?;
    file.write_all(key.trim().as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn validate_private_key_file(path: &Path) -> Result<(), LinuxFabricError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(LinuxFabricError::ForeignState);
    }
    let key = fs::read_to_string(path)?;
    if !valid_wireguard_key(key.trim()) {
        return Err(LinuxFabricError::ForeignState);
    }
    Ok(())
}
