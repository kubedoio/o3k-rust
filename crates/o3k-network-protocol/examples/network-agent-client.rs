use std::{env, fs, path::PathBuf};

use o3k_network_protocol::{NetworkAgentClient, proto};

fn required(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("missing {name}"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let endpoint = required(&mut args, "endpoint")?;
    let server_name = required(&mut args, "server_name")?;
    let ca = PathBuf::from(required(&mut args, "ca")?);
    let client_certificate = PathBuf::from(required(&mut args, "client_certificate")?);
    let client_key = PathBuf::from(required(&mut args, "client_key")?);
    let agent_id = required(&mut args, "agent_id")?;
    let agent_epoch = required(&mut args, "agent_epoch")?;
    let controller_id = required(&mut args, "controller_id")?;
    let controller_epoch = required(&mut args, "controller_epoch")?;
    let fencing_token = required(&mut args, "fencing_token")?;
    let command_id = required(&mut args, "command_id")?;
    let operation_id = required(&mut args, "operation_id")?;
    let idempotency_key = required(&mut args, "idempotency_key")?;
    let deadline_unix_ms = required(&mut args, "deadline_unix_ms")?.parse::<u64>()?;
    let plan_path = PathBuf::from(required(&mut args, "plan_path")?);
    let remove = matches!(args.next().as_deref(), Some("remove"));

    let plan_json = fs::read_to_string(plan_path)?;
    let client =
        NetworkAgentClient::connect(&endpoint, &server_name, ca, client_certificate, client_key)
            .await?;
    let result = client
        .execute(
            proto::Register {
                agent_id: agent_id.clone(),
                agent_epoch: agent_epoch.clone(),
            },
            proto::NetworkCommand {
                command_id,
                operation_id,
                idempotency_key,
                agent_id,
                agent_epoch,
                controller_id,
                controller_epoch,
                fencing_token: fencing_token.parse()?,
                deadline_unix_ms,
                plan_json,
                remove,
            },
        )
        .await?;
    println!("{} {}", result.status, result.replayed);
    Ok(())
}
