use std::io;
use std::process::Command;

pub(crate) trait PolicyCommand: Send + Sync {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, args: &[&str]) -> io::Result<bool>;
}

pub(crate) struct SystemPolicyCommand {
    pub(crate) namespace: Option<String>,
}

impl PolicyCommand for SystemPolicyCommand {
    fn output(&self, args: &[&str]) -> io::Result<(bool, String)> {
        let mut command = if let Some(namespace) = &self.namespace {
            let mut command = Command::new("ip");
            command.args(["netns", "exec", namespace, "nft"]);
            command
        } else {
            Command::new("nft")
        };
        let output = command.args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, args: &[&str]) -> io::Result<bool> {
        let mut command = if let Some(namespace) = &self.namespace {
            let mut command = Command::new("ip");
            command.args(["netns", "exec", namespace, "nft"]);
            command
        } else {
            Command::new("nft")
        };
        Ok(command.args(args).status()?.success())
    }
}
