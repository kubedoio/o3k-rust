#[cfg(test)]
mod block_device_tests {
    use crate::*;

    fn attach_device() -> proto::AttachDiskCommand {
        proto::AttachDiskCommand {
            volume_id: "volume-1".to_owned(),
            attachment_id: "attachment-1".to_owned(),
            driver_volume_type: "iscsi".to_owned(),
            target_iqn: "iqn.2026-01.example.com:volume-1".to_owned(),
            target_portal: "10.0.0.10:3260".to_owned(),
            target_lun: 1,
            device_path: String::new(),
            multipath: false,
            initiator: "iqn.1993-08.org.debian:01:o3k-compute".to_owned(),
            auth_method: "CHAP".to_owned(),
            auth_username: "chap-user".to_owned(),
            auth_password: "chap-password".to_owned(),
        }
    }

    #[test]
    fn block_device_commands_are_bounded_and_deterministic() -> Result<(), AgentError> {
        let collect = build_block_device_command(
            BlockDeviceCommand::CollectConnector,
            "agent-1",
            "epoch-1",
            "op-1",
            "server-1",
        )?;
        assert!(collect.payload_fingerprint_sha256.len() == 64);
        let collect_again = build_block_device_command(
            BlockDeviceCommand::CollectConnector,
            "agent-1",
            "epoch-1",
            "op-1",
            "server-1",
        )?;
        assert_eq!(collect.command_id, collect_again.command_id);
        assert_eq!(
            collect.payload_fingerprint_sha256,
            collect_again.payload_fingerprint_sha256
        );
        validate_command(&collect)?;

        let attach = build_block_device_command(
            BlockDeviceCommand::Attach {
                device: attach_device(),
            },
            "agent-1",
            "epoch-1",
            "op-2",
            "server-1",
        )?;
        validate_command(&attach)?;
        assert!(matches!(
            attach.action,
            Some(proto::command::Action::AttachDisk(_))
        ));
        Ok(())
    }

    #[test]
    fn attach_disk_requires_a_supported_driver_volume_type() {
        let mut device = attach_device();
        device.driver_volume_type = "rbd".to_owned();
        assert!(
            build_block_device_command(
                BlockDeviceCommand::Attach { device },
                "agent-1",
                "epoch-1",
                "op-2",
                "server-1",
            )
            .is_err()
        );

        let mut device = attach_device();
        device.driver_volume_type = "iscsi".to_owned();
        device.target_iqn = String::new();
        assert!(
            build_block_device_command(
                BlockDeviceCommand::Attach { device },
                "agent-1",
                "epoch-1",
                "op-2",
                "server-1",
            )
            .is_err()
        );
    }

    #[test]
    fn detach_and_observe_commands_are_validated() -> Result<(), AgentError> {
        let detach = build_block_device_command(
            BlockDeviceCommand::Detach {
                device: proto::DetachDiskCommand {
                    volume_id: "volume-1".to_owned(),
                    attachment_id: "attachment-1".to_owned(),
                    driver_volume_type: "iscsi".to_owned(),
                    target_iqn: "iqn.2026-01.example.com:volume-1".to_owned(),
                    target_portal: "10.0.0.10:3260".to_owned(),
                    target_lun: 1,
                    device_path: String::new(),
                    multipath: false,
                    initiator: String::new(),
                },
            },
            "agent-1",
            "epoch-1",
            "op-3",
            "server-1",
        )?;
        validate_command(&detach)?;

        let observe = build_block_device_command(
            BlockDeviceCommand::Observe {
                volume_id: "volume-1".to_owned(),
                attachment_id: "attachment-1".to_owned(),
            },
            "agent-1",
            "epoch-1",
            "op-4",
            "server-1",
        )?;
        validate_command(&observe)?;
        Ok(())
    }

    #[tokio::test]
    async fn fake_executor_attach_detach_observe_is_idempotent() -> Result<(), AgentError> {
        let executor = FakeCommandExecutor::default();
        let server_id = "server-1";

        let attach = build_block_device_command(
            BlockDeviceCommand::Attach {
                device: attach_device(),
            },
            "agent-1",
            "epoch-1",
            "op-attach",
            server_id,
        )?;
        let first = executor.execute(&attach).await?;
        let observation = first
            .block_device
            .ok_or_else(|| AgentError::Protocol("attach observation missing".to_owned()))?;
        assert!(observation.attached);
        assert!(observation.host_path.contains("/dev/sd"));

        // Idempotent: a second attach returns success without duplication.
        let second = executor.execute(&attach).await?;
        assert_eq!(second.block_device.as_ref().map(|o| o.attached), Some(true));

        let observe = build_block_device_command(
            BlockDeviceCommand::Observe {
                volume_id: "volume-1".to_owned(),
                attachment_id: "attachment-1".to_owned(),
            },
            "agent-1",
            "epoch-1",
            "op-observe",
            server_id,
        )?;
        let observed = executor.execute(&observe).await?;
        assert!(observed.block_device.is_some_and(|o| o.attached));

        let detach = build_block_device_command(
            BlockDeviceCommand::Detach {
                device: proto::DetachDiskCommand {
                    volume_id: "volume-1".to_owned(),
                    attachment_id: "attachment-1".to_owned(),
                    driver_volume_type: "iscsi".to_owned(),
                    target_iqn: "iqn.2026-01.example.com:volume-1".to_owned(),
                    target_portal: "10.0.0.10:3260".to_owned(),
                    target_lun: 1,
                    device_path: String::new(),
                    multipath: false,
                    initiator: String::new(),
                },
            },
            "agent-1",
            "epoch-1",
            "op-detach",
            server_id,
        )?;
        let detached = executor.execute(&detach).await?;
        assert!(detached.block_device.is_some_and(|o| !o.attached));

        // Repeated detach is idempotent.
        let again = executor.execute(&detach).await?;
        assert!(again.block_device.is_some_and(|o| !o.attached));

        let observed_after = executor.execute(&observe).await?;
        assert!(observed_after.block_device.is_some_and(|o| !o.attached));
        Ok(())
    }

    #[tokio::test]
    async fn fake_executor_rejects_unsupported_driver_before_dispatch() -> Result<(), AgentError> {
        let mut device = attach_device();
        device.driver_volume_type = "nfs".to_owned();
        let command = build_block_device_command(
            BlockDeviceCommand::Attach { device },
            "agent-1",
            "epoch-1",
            "op-attach-bad",
            "server-1",
        );
        assert!(command.is_err());
        Ok(())
    }
}
