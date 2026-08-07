//! Gate B — Cinder protocol gate.
//!
//! Proves the outbound Cinder v3 attachment client against a stateful server
//! that reproduces the exact Cinder 28.0.0 response shapes from
//! `contracts/cinder/attachment-interaction-v28.yaml`:
//!
//! - POST create returns 200 with the detail view (`status` carries
//!   `attach_status`, `attach_mode`, `connection_info` empty object);
//! - PUT connector returns 200 with the detail view and the flattened LVM/tgt
//!   CHAP connection information (`data` merged to the top level);
//! - GET show returns the detail view; GET list returns the summary view;
//! - POST `os-complete` returns 204 No Content with no body;
//! - DELETE returns 200 with the remaining shared-attachment summary list;
//! - missing, null and malformed `connection_info` are classified distinctly.

use o3k_cinder::testkit::{AttachmentStatus, faults};
use o3k_cinder::{CinderError, ComputeConnector, ConnectionInfoPresence};
use serde_json::json;

mod common;
use common::{PROJECT, setup};

fn connector() -> ComputeConnector {
    ComputeConnector {
        host: "compute-gate-b".to_owned(),
        ip: "10.0.0.5".to_owned(),
        platform: "x86_64".to_owned(),
        os_type: "linux".to_owned(),
        multipath: false,
        initiator: Some("iqn.1993-08.org.debian:01:o3k-gate-b".to_owned()),
    }
}

#[tokio::test]
async fn full_lifecycle_reproduces_exact_cinder_28_shapes() -> Result<(), Box<dyn std::error::Error>>
{
    let (client, fake, _endpoint) = setup().await?;
    let volume = client.create_volume(PROJECT, 1, "gate-b-vol").await?;

    // POST create: 200, detail view, attach_status "reserved", connection_info
    // is an empty object, `attach_mode` "null".
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;
    assert_eq!(attachment.status, "reserved");
    assert_eq!(attachment.volume_id, volume.id);
    assert_eq!(
        attachment.connection_info_presence(),
        ConnectionInfoPresence::Present
    );
    assert_eq!(
        fake.attachment_status(&attachment.id).as_deref(),
        Some(AttachmentStatus::Reserved.as_str())
    );

    // PUT connector: 200, STALE detail view. Cinder 28 serializes the
    // pre-update API object, so `status` is still the reserved attach_status
    // and `attach_mode` is still "null"; connection_info IS populated with the
    // flattened CHAP data. A subsequent GET show returns the fresh view.
    let updated = client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await?;
    assert_eq!(updated.status, "reserved");
    assert_eq!(
        updated.connection_info_presence(),
        ConnectionInfoPresence::Present
    );
    let connection_info = updated.connection_info.ok_or("connection_info missing")?;
    assert_eq!(connection_info.driver_volume_type(), Some("iscsi"));
    assert!(connection_info.has_usable_target());
    let target = connection_info
        .attach_target()
        .ok_or("attach target missing")?;
    assert_eq!(
        target.target_iqn.as_deref(),
        Some("iqn.2010-10.org.openstack:volume-00000001")
    );
    assert_eq!(target.target_portal.as_deref(), Some("10.0.0.10:3260"));
    assert_eq!(target.target_lun, Some(1));
    assert_eq!(target.auth_method.as_deref(), Some("CHAP"));
    assert_eq!(
        fake.attachment_status(&attachment.id).as_deref(),
        Some(AttachmentStatus::Attaching.as_str())
    );

    // GET show returns the fresh detail view (attach_status attaching).
    let shown = client.show_attachment(PROJECT, &attachment.id).await?;
    assert_eq!(shown.id, attachment.id);
    assert_eq!(shown.status, "attaching");
    assert!(shown.connection_info.is_some());

    // GET list returns the summary view.
    let listed = client.list_attachments(PROJECT).await?;
    assert!(
        listed.iter().any(|item| item.id == attachment.id),
        "list must contain the attachment"
    );

    // POST os-complete: 204 No Content.
    client.complete_attachment(PROJECT, &attachment.id).await?;
    assert_eq!(
        fake.attachment_status(&attachment.id).as_deref(),
        Some(AttachmentStatus::Attached.as_str())
    );

    // DELETE: 200, service token validated, attachment removed.
    client.terminate_attachment(PROJECT, &attachment.id).await?;
    assert!(!fake.attachment_ids().contains(&attachment.id));
    assert_eq!(fake.last_delete_service_token_validated(), Some(true));

    client.delete_volume(PROJECT, &volume.id).await?;
    assert!(fake.volume_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_connection_info_is_classified_distinctly() -> Result<(), Box<dyn std::error::Error>>
{
    let (client, fake, _endpoint) = setup().await?;
    let volume = client.create_volume(PROJECT, 1, "gate-b-missing").await?;
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;
    fake.set_fault(faults::missing_connection_info_on_update, true);
    let updated = client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await?;
    assert_eq!(
        updated.connection_info_presence(),
        ConnectionInfoPresence::Missing
    );
    assert!(updated.connection_info.is_none());
    client.delete_volume(PROJECT, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn null_connection_info_is_classified_distinctly() -> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _endpoint) = setup().await?;
    let volume = client.create_volume(PROJECT, 1, "gate-b-null").await?;
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;
    fake.set_fault(faults::null_connection_info_on_update, true);
    let updated = client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await?;
    assert_eq!(
        updated.connection_info_presence(),
        ConnectionInfoPresence::Null
    );
    assert!(updated.connection_info.is_none());
    client.delete_volume(PROJECT, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn malformed_connection_info_is_classified_distinctly()
-> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _endpoint) = setup().await?;
    let volume = client.create_volume(PROJECT, 1, "gate-b-malformed").await?;
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;
    fake.set_fault(faults::malformed_connection_info_on_update, true);
    let updated = client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await?;
    assert_eq!(
        updated.connection_info_presence(),
        ConnectionInfoPresence::Malformed
    );
    assert!(updated.connection_info.is_none());
    client.delete_volume(PROJECT, &volume.id).await?;
    Ok(())
}

#[tokio::test]
async fn successful_update_response_is_parsed_from_the_actual_cinder_detail_view()
-> Result<(), Box<dyn std::error::Error>> {
    // Directly parse the exact detail-view JSON body that real Cinder 28
    // returns for a PUT connector (flattened connection_info).
    let raw = json!({
        "attachment": {
            "id": "3931eb26-3a98-41f0-9d0c-f6076e5a014d",
            "status": "reserved",
            "instance": "9dd22dc6-ea63-5bea-b994-f5a3796a3c59",
            "volume_id": "ada22501-2a90-4307-a985-25f81531fef8",
            "attached_at": "",
            "detached_at": "",
            "attach_mode": "rw",
            "connection_info": {
                "driver_volume_type": "iscsi",
                "target_discovered": false,
                "target_portal": "10.5.199.161:3260",
                "target_iqn": "iqn.2010-10.org.openstack:volume-ada22501-2a90-4307-a985-25f81531fef8",
                "target_lun": 1,
                "volume_id": "ada22501-2a90-4307-a985-25f81531fef8",
                "auth_method": "CHAP",
                "auth_username": "u-placeholder",
                "auth_password": "p-placeholder",
                "encrypted": false,
                "qos_specs": null,
                "attachment_id": "3931eb26-3a98-41f0-9d0c-f6076e5a014d",
                "enforce_multipath": false
            }
        }
    });
    let attachment = o3k_cinder::CinderAttachment::parse(&raw)?;
    assert_eq!(attachment.id, "3931eb26-3a98-41f0-9d0c-f6076e5a014d");
    assert_eq!(attachment.status, "reserved");
    assert_eq!(
        attachment.connection_info_presence(),
        ConnectionInfoPresence::Present
    );
    let connection_info = attachment
        .connection_info
        .ok_or("missing connection_info")?;
    assert!(connection_info.has_usable_target());
    let target = connection_info.attach_target().ok_or("missing target")?;
    assert_eq!(
        target.target_iqn.as_deref(),
        Some("iqn.2010-10.org.openstack:volume-ada22501-2a90-4307-a985-25f81531fef8")
    );
    assert_eq!(target.target_portal.as_deref(), Some("10.5.199.161:3260"));
    assert_eq!(target.target_lun, Some(1));
    assert_eq!(target.auth_method.as_deref(), Some("CHAP"));
    Ok(())
}

#[tokio::test]
async fn timeout_on_update_is_an_unknown_outcome() -> Result<(), Box<dyn std::error::Error>> {
    let (client, fake, _endpoint) = setup().await?;
    let client = client.with_timeout(std::time::Duration::from_secs(1));
    let volume = client.create_volume(PROJECT, 1, "gate-b-timeout").await?;
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;
    fake.set_fault(faults::timeout_update_connector, true);
    let error = match client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await
    {
        Err(error) => error,
        Ok(_) => return Err("expected timeout".into()),
    };
    assert!(
        error.is_unknown_outcome(),
        "a timed-out PUT must classify as unknown: {error}"
    );
    assert!(matches!(error, CinderError::UnknownOutcome(_)));
    Ok(())
}

#[tokio::test]
async fn detail_view_fields_are_emitted_exactly() -> Result<(), Box<dyn std::error::Error>> {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use o3k_identity::testkit::test_service;

    let identity = test_service("http://127.0.0.1:8080").await?;
    let (client, fake, endpoint) = setup().await?;
    let volume = client.create_volume(PROJECT, 1, "gate-b-shape").await?;
    let attachment = client
        .create_attachment(
            PROJECT,
            &volume.id,
            Some("9dd22dc6-ea63-5bea-b994-f5a3796a3c59"),
        )
        .await?;

    let (service_token, _) = identity.issue(
        &o3k_identity::testkit::cinder_service_request("password"),
        std::time::SystemTime::now(),
    )?;
    let http = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(hyper_util::client::legacy::connect::HttpConnector::new());

    async fn get_json(
        http: &hyper_util::client::legacy::Client<
            hyper_util::client::legacy::connect::HttpConnector,
            Body,
        >,
        url: &str,
        token: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(url)
            .header("x-auth-token", token)
            .body(Body::empty())?;
        let resp = http.request(req).await?;
        let bytes = resp.into_body().collect().await?.to_bytes();
        Ok(serde_json::from_slice(&bytes)?)
    }

    // POST create detail view: status reserved, attach_mode "null",
    // connection_info an empty object.
    let url = format!("{endpoint}/v3/{PROJECT}/attachments/{}", attachment.id);
    let shown = get_json(&http, &url, &service_token).await?;
    let attachment_view = &shown["attachment"];
    assert_eq!(attachment_view["status"], "reserved");
    assert_eq!(attachment_view["attach_mode"], "null");
    assert_eq!(attachment_view["attached_at"], "");
    assert_eq!(attachment_view["detached_at"], "");
    assert_eq!(
        attachment_view["instance"],
        "9dd22dc6-ea63-5bea-b994-f5a3796a3c59"
    );

    // After PUT, a fresh show returns the fresh view: attaching / rw with the
    // flattened CHAP connection_info.
    client
        .update_attachment_connector(PROJECT, &attachment.id, &connector())
        .await?;
    let shown = get_json(&http, &url, &service_token).await?;
    let attachment_view = &shown["attachment"];
    assert_eq!(attachment_view["status"], "attaching");
    assert_eq!(attachment_view["attach_mode"], "rw");
    let connection_info = &attachment_view["connection_info"];
    assert_eq!(connection_info["driver_volume_type"], "iscsi");
    assert_eq!(connection_info["target_lun"], 1);
    assert_eq!(connection_info["auth_method"], "CHAP");
    assert!(connection_info["target_iqn"].is_string());
    assert!(connection_info["target_portal"].is_string());
    assert!(connection_info["attachment_id"].is_string());
    assert_eq!(connection_info["enforce_multipath"], false);
    assert!(connection_info.as_object().is_some());

    let _ = fake;
    Ok(())
}
