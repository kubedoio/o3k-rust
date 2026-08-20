use super::*;

pub(crate) fn load_state(path: &Path) -> Result<ProviderState, LinuxP11Error> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| LinuxP11Error::CorruptState),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ProviderState::default()),
        Err(error) => Err(LinuxP11Error::Storage(error)),
    }
}

pub(crate) fn load_plans(
    path: &Path,
) -> Result<BTreeMap<Uuid, NamespacedRoutedFabricPlan>, LinuxP11Error> {
    let mut plans = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() || !entry.file_type()?.is_file() {
            return Err(LinuxP11Error::ForeignState);
        }
        let plan: NamespacedRoutedFabricPlan = serde_json::from_slice(&fs::read(entry.path())?)
            .map_err(|_| LinuxP11Error::CorruptState)?;
        plans.insert(plan.realm_id, plan);
    }
    Ok(plans)
}

pub(crate) fn store_state(path: &Path, state: &ProviderState) -> Result<(), LinuxP11Error> {
    store_json(path, state)
}

pub(crate) fn store_plan(
    path: &Path,
    plan: &NamespacedRoutedFabricPlan,
) -> Result<(), LinuxP11Error> {
    store_json(path, plan)
}

pub(crate) fn store_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LinuxP11Error> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| LinuxP11Error::CorruptState)?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

pub(crate) fn write_private_key(
    path: &Path,
    command: &Arc<dyn LinuxP11Command>,
) -> Result<(), LinuxP11Error> {
    if path.exists() {
        return validate_private_key_file(path);
    }
    let (success, key) = command
        .output("wg", &["genkey"])
        .map_err(LinuxP11Error::Storage)?;
    if !success || !valid_wireguard_key(key.trim()) || key.lines().count() != 1 {
        return Err(LinuxP11Error::CommandFailed);
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

pub(crate) fn validate_private_key_file(path: &Path) -> Result<(), LinuxP11Error> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(LinuxP11Error::ForeignState);
    }
    let key = fs::read_to_string(path)?;
    if !valid_wireguard_key(key.trim()) {
        return Err(LinuxP11Error::ForeignState);
    }
    Ok(())
}
