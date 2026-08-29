use std::{io, process::Command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkCommandOutput {
    pub(crate) success: bool,
    pub(crate) stdout: String,
}

pub(crate) trait NetworkCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput>;
    fn status(&self, args: &[&str]) -> io::Result<bool>;
}

pub(crate) struct SystemNetworkCommand;

impl NetworkCommand for SystemNetworkCommand {
    fn output(&self, args: &[&str]) -> io::Result<NetworkCommandOutput> {
        let output = Command::new("ip").args(args).output()?;
        Ok(NetworkCommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }

    fn status(&self, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new("ip").args(args).status()?.success())
    }
}
