use std::io;
use std::process::Command;

pub(crate) trait LinuxGatewayCommand: Send + Sync {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)>;
    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool>;
    fn supports_gateway_marker(&self) -> bool {
        false
    }
    fn gateway_marker(&self, namespace: &str, table: &str) -> io::Result<Option<String>> {
        let _ = (namespace, table);
        Ok(None)
    }
}

pub(crate) struct SystemLinuxGatewayCommand;

impl LinuxGatewayCommand for SystemLinuxGatewayCommand {
    fn output(&self, program: &str, args: &[&str]) -> io::Result<(bool, String)> {
        let output = Command::new(program).args(args).output()?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn run(&self, program: &str, args: &[&str]) -> io::Result<bool> {
        Ok(Command::new(program).args(args).status()?.success())
    }

    fn gateway_marker(&self, namespace: &str, table: &str) -> io::Result<Option<String>> {
        let output = Command::new("ip")
            .args([
                "netns", "exec", namespace, "nft", "list", "table", "ip", table,
            ])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .split("comment ")
            .nth(1)
            .and_then(|value| value.split('"').nth(1))
            .map(ToOwned::to_owned))
    }

    fn supports_gateway_marker(&self) -> bool {
        true
    }
}
