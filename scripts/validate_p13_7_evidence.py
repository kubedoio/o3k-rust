#!/usr/bin/env python3
"""P13.7 real-host IaC acceptance evidence validator.

Validates the evidence document emitted by
tests/p13_7_real_host_iac_acceptance.sh. A gate row that claims "passed"
without its mandatory proof fields is rejected; the aggregate verdict is
accepted only when every gate R1..R10 passed with complete proofs, the
provider is unmodified, and no fake-provider flag is present.

  * --self-test proves that tampered / missing-proof evidence is rejected.
  * validation failure exits 2 (harness/CI convention).
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import tempfile

EXPECTED_ARTIFACT_TYPE = "o3k-p13-7-real-host-iac-evidence"
EXPECTED_PHASE = "P13.7"
EXPECTED_PROFILE = "p13-iac-compatibility-v1"
EXPECTED_BACKEND = "postgresql"
EXPECTED_OPENTOFU = "1.12.6"
EXPECTED_PROVIDER = "terraform-provider-openstack/openstack 3.4.0"
EXPECTED_IMAGE_SHA256 = (
    "7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b"
)

GATES = [f"R{i}" for i in range(1, 11)]

# Mandatory proof keys per gate. A "passed" row missing any of these is
# rejected; values must additionally satisfy the per-gate truthiness rules
# below (bare placeholders like false/0/"" never satisfy a proof field).
GATE_REQUIRED_KEYS: dict[str, dict[str, object]] = {
    "R1": {
        "token_acquired": True,
        "catalog_services": list,
        "image_data_source_resolved": True,
        "flavor_data_source_resolved": True,
    },
    "R2": {
        "bridge_name": str,
        "bridge_present": True,
        "dnsmasq_inventory_seen": True,
        "network_id": str,
        "subnet_id": str,
        "port_id": str,
        "router_id": str,
        "owner_project": str,
        "external_network_id": str,
        "external_realm_id": str,
        "external_realm_matches_plan": True,
    },
    "R3": {
        "libvirt_domain": str,
        "domain_running": True,
        "boot_marker": str,
        "boot_marker_seen": True,
        "fixed_ip": str,
        "dhcp_lease_matches_port": True,
        "ssh_path": str,
        "ssh_ok": True,
    },
    "R4": {
        "denied_observed": True,
        "allowed_observed": True,
        "toggle_via": str,
        "nft_counter_packets_before": int,
        "nft_counter_packets_after": int,
        "nft_drop_counter_packets_before": int,
        "nft_drop_counter_packets_after": int,
        "nft_drop_counter_seen": True,
    },
    "R5": {
        "lv_name": str,
        "guest_device": str,
        "marker_sha256": str,
        "post_reattach_sha256": str,
        "checksum_match": True,
        "reattach_mechanism": str,
    },
    "R6": {
        "initial_plan_noop": True,
        "drift_detected": True,
        "drift_resource": str,
        "drift_attribute": str,
        "drift_exactly_one_change": True,
        "restored_by_apply": True,
        "final_plan_noop": True,
    },
    "R7": {
        "restart_clean_sigterm": True,
        "identities_equal": True,
        "identities": dict,
        "post_restart_plan_noop": True,
        "ssh_after_restart": True,
        "volume_marker_intact": True,
    },
    "R8": {
        "mechanism": str,
        "virsh_transitions": list,
        "stop_observed": True,
        "start_observed": True,
        "reboot_observed": True,
        "post_recovery_plan_noop": True,
        "ssh_after_recovery": True,
        "volume_marker_intact": True,
    },
    "R9": {
        "zero_servers": True,
        "zero_ports": True,
        "zero_networks": True,
        "zero_subnets": True,
        "zero_routers": True,
        "zero_security_groups": True,
        "zero_floating_ips": True,
        "zero_volumes": True,
        "zero_attachments": True,
        "attachment_count": 0,
        "zero_libvirt_domains": True,
        "zero_lvs": True,
        "zero_nft_tables": True,
        "zero_bridges": True,
        "zero_dnsmasq": True,
        "zero_non_terminal_operations": True,
    },
    "R10": {
        "owned_leaks": 0,
        "foreign_state_changes": 0,
        "foreign_baseline_entries": int,
    },
}

SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")

SECRET_VALUE_RES = [
    re.compile(r"-----BEGIN"),
    re.compile(r"Bearer\s+[A-Za-z0-9._-]{10,}"),
    re.compile(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\."),  # JWT shape
    re.compile(r"\"(password|token|private_key|secret)\"\s*:\s*\"[^\"]{8,}\"", re.I),
]


class Failure(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def walk_for_secrets(obj, path="$", findings=None):
    if findings is None:
        findings = []
    if isinstance(obj, str):
        for pattern in SECRET_VALUE_RES:
            if pattern.search(obj):
                findings.append(f"{path}: value matches secret pattern {pattern.pattern!r}")
    elif isinstance(obj, dict):
        for key, value in obj.items():
            walk_for_secrets(value, f"{path}.{key}", findings)
    elif isinstance(obj, list):
        for index, value in enumerate(obj):
            walk_for_secrets(value, f"{path}[{index}]", findings)
    return findings


def check_gate(row: dict) -> None:
    gate = row.get("gate")
    require(gate in GATES, f"unknown gate {gate!r}")
    require(row.get("phase") == EXPECTED_PHASE, f"{gate}: phase mismatch")
    require(row.get("result") in ("passed", "failed", "blocked"),
            f"{gate}: uncontrolled result {row.get('result')!r}")
    if row["result"] != "passed":
        return
    for key, expected in GATE_REQUIRED_KEYS[gate].items():
        require(key in row, f"{gate}: passed row missing proof field '{key}'")
        value = row[key]
        if expected is True:
            require(value is True, f"{gate}: proof field '{key}' must be true (got {value!r})")
        elif expected == 0:
            require(value == 0, f"{gate}: proof field '{key}' must be 0 (got {value!r})")
        elif isinstance(expected, type):
            require(isinstance(value, expected) and value not in ("", [], {}),
                    f"{gate}: proof field '{key}' must be a non-empty {expected.__name__}")
    if gate == "R4":
        require(row["nft_counter_packets_after"] > 0,
                "R4: nft counter evidence must show packets")
        require(row["nft_drop_counter_packets_before"] >= 0
                and row["nft_drop_counter_packets_after"] >= 0,
                "R4: nft drop counter values must be non-negative")
        require(row["nft_drop_counter_packets_after"] >= row["nft_drop_counter_packets_before"],
                "R4: nft drop counter regressed after allow probe")
    if gate == "R5":
        require(SHA256_RE.fullmatch(row["marker_sha256"]),
                "R5: marker_sha256 is not a sha256 hex digest")
        require(row["marker_sha256"] == row["post_reattach_sha256"],
                "R5: post-reattach checksum differs from the original marker")
    if gate == "R7":
        require(bool(row["identities"]), "R7: identities map must not be empty")
    if gate == "R8":
        transitions = row["virsh_transitions"]
        require("shut off" in transitions and transitions.count("running") >= 2,
                "R8: virsh transitions must record running -> shut off -> running")


def validate_document(document: dict) -> dict:
    require(document.get("artifact_type") == EXPECTED_ARTIFACT_TYPE,
            f"artifact_type must be {EXPECTED_ARTIFACT_TYPE!r}")
    require(document.get("schema_version") == 1, "schema_version must be 1")
    require(document.get("phase") == EXPECTED_PHASE, "phase mismatch")
    require(document.get("profile") == EXPECTED_PROFILE, "profile mismatch")
    require(document.get("backend") == EXPECTED_BACKEND,
            "backend must be postgresql")
    require(isinstance(document.get("execution_tier"), str)
            and "real-host" in document["execution_tier"],
            "execution_tier must name the real-host tier")
    require(SHA1_RE.fullmatch(str(document.get("tested_runtime_head_sha", ""))),
            "tested_runtime_head_sha must be a 40-hex commit sha")
    require(isinstance(document.get("execution_host"), str)
            and document["execution_host"].strip(),
            "execution_host must identify the execution host")
    require(re.match(r"^16\.[0-9]+(?:\s|$)", str(document.get("postgresql_server_version", ""))),
            "postgresql_server_version must be PostgreSQL 16.x")

    toolchain = document.get("toolchain", {})
    require(toolchain.get("opentofu") == EXPECTED_OPENTOFU, "toolchain.opentofu mismatch")
    require(SHA256_RE.fullmatch(str(toolchain.get("opentofu_archive_sha256", ""))),
            "toolchain.opentofu_archive_sha256 must be a sha256 hex digest")
    require(toolchain.get("provider") == EXPECTED_PROVIDER, "toolchain.provider mismatch")
    for key in ("provider_archive_sha256", "provider_binary_sha256"):
        require(SHA256_RE.fullmatch(str(toolchain.get(key, ""))),
                f"toolchain.{key} must be a sha256 hex digest")
    require(toolchain.get("provider_modified") is False,
            "toolchain.provider_modified must be false")

    image = document.get("image", {})
    require(image.get("sha256") == EXPECTED_IMAGE_SHA256,
            "image sha256 does not match the pinned CirrOS 0.6.3 digest")
    require(isinstance(image.get("source_url"), str)
            and image["source_url"].startswith("https://"),
            "image provenance URL missing")

    require(document.get("fake_provider") is False, "fake-provider evidence is not acceptance")

    gates = document.get("gates")
    require(isinstance(gates, list), "gates must be a list")
    by_gate: dict[str, dict] = {}
    for row in gates:
        check_gate(row)
        gate = row["gate"]
        require(gate not in by_gate, f"duplicate gate row for {gate}")
        by_gate[gate] = row
    missing = [g for g in GATES if g not in by_gate]
    require(not missing, f"missing gate rows: {missing}")

    findings = walk_for_secrets(document)
    require(not findings, f"possible secret material: {findings}")

    all_passed = all(by_gate[g]["result"] == "passed" for g in GATES)
    verdict = document.get("result")
    if verdict == "passed":
        require(all_passed, "aggregate 'passed' but not all gates passed")
    else:
        require(verdict in ("failed", "blocked"), f"uncontrolled aggregate result {verdict!r}")
    return {"verdict": verdict, "all_passed": all_passed, "gates": len(gates)}


def load_json(path: str) -> dict:
    return json.loads(pathlib.Path(path).read_text(encoding="utf-8"))


def self_test() -> None:
    """Prove that tampered and missing-proof evidence is rejected."""
    valid = {
        "artifact_type": EXPECTED_ARTIFACT_TYPE,
        "schema_version": 1,
        "phase": EXPECTED_PHASE,
        "profile": EXPECTED_PROFILE,
        "backend": "postgresql",
        "execution_tier": "real-host-kvm-libvirt-disposable-lvm",
        "tested_runtime_head_sha": "0" * 40,
        "execution_host": "self-test-host",
        "postgresql_server_version": "16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)",
        "toolchain": {
            "opentofu": EXPECTED_OPENTOFU,
            "opentofu_archive_sha256": "0" * 64,
            "provider": EXPECTED_PROVIDER,
            "provider_archive_sha256": "0" * 64,
            "provider_binary_sha256": "0" * 64,
            "provider_modified": False,
        },
        "image": {
            "name": "cirros-0.6.3-x86_64-disk.img",
            "source_url": "https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img",
            "sha256": EXPECTED_IMAGE_SHA256,
        },
        "fake_provider": False,
        "gates": [
            {"gate": "R1", "phase": "P13.7", "result": "passed",
             "token_acquired": True, "catalog_services": ["identity", "compute"],
             "image_data_source_resolved": True, "flavor_data_source_resolved": True},
            {"gate": "R2", "phase": "P13.7", "result": "passed",
             "bridge_name": "o3kp137x", "bridge_present": True,
             "nft_policy_table_present": True, "dnsmasq_inventory_seen": True,
             "network_id": "n", "subnet_id": "s", "port_id": "p", "router_id": "r",
             "owner_project": "eba29e2d-53de-461d-ae91-ede7402713cb",
             "external_network_id": "external-network", "external_realm_id": "external-realm",
             "external_realm_matches_plan": True},
            {"gate": "R3", "phase": "P13.7", "result": "passed",
             "libvirt_domain": "dom", "domain_running": True,
             "boot_marker": "login as 'cirros' user", "boot_marker_seen": True,
             "fixed_ip": "192.0.2.10", "dhcp_lease_matches_port": True,
             "ssh_path": "floating_ip", "ssh_ok": True},
            {"gate": "R4", "phase": "P13.7", "result": "passed",
             "denied_observed": True, "allowed_observed": True,
             "toggle_via": "openstack_networking_secgroup_rule_v2",
             "nft_counter_packets_before": 0, "nft_counter_packets_after": 5,
             "nft_drop_counter_packets_before": 1, "nft_drop_counter_packets_after": 1,
             "nft_drop_counter_seen": True},
            {"gate": "R5", "phase": "P13.7", "result": "passed",
             "lv_name": "o3k-v-x", "guest_device": "/dev/vdb",
             "marker_sha256": "a" * 64, "post_reattach_sha256": "a" * 64,
             "checksum_match": True,
             "reattach_mechanism": "tofu taint openstack_compute_volume_attach_v2"},
            {"gate": "R6", "phase": "P13.7", "result": "passed",
             "initial_plan_noop": True, "drift_detected": True,
             "drift_resource": "openstack_networking_network_v2.network",
             "drift_attribute": "name", "drift_exactly_one_change": True,
             "restored_by_apply": True, "final_plan_noop": True},
            {"gate": "R7", "phase": "P13.7", "result": "passed",
             "restart_clean_sigterm": True, "identities_equal": True,
             "identities": {"server_id": "x"}, "post_restart_plan_noop": True,
             "ssh_after_restart": True, "volume_marker_intact": True},
            {"gate": "R8", "phase": "P13.7", "result": "passed",
             "mechanism": "power_state + os-reboot",
             "virsh_transitions": ["running", "shut off", "running", "running"],
             "stop_observed": True, "start_observed": True, "reboot_observed": True,
             "post_recovery_plan_noop": True, "ssh_after_recovery": True,
             "volume_marker_intact": True},
            {"gate": "R9", "phase": "P13.7", "result": "passed",
             **{k: True for k in GATE_REQUIRED_KEYS["R9"] if k != "attachment_count"},
             "attachment_count": 0},
            {"gate": "R10", "phase": "P13.7", "result": "passed",
             "owned_leaks": 0, "foreign_state_changes": 0, "foreign_baseline_entries": 4},
        ],
        "result": "passed",
    }
    outcome = validate_document(valid)
    require(outcome["verdict"] == "passed" and outcome["all_passed"],
            "self-test: valid fixture was rejected")

    def expect_reject(mutate, label):
        doc = json.loads(json.dumps(valid))
        mutate(doc)
        try:
            validate_document(doc)
        except Failure as error:
            print(f"self-test OK ({label}): {error}")
            return
        raise Failure(f"self-test FAILED: {label} was accepted")

    # Bare "passed" without proof fields.
    expect_reject(lambda d: d["gates"].__setitem__(2, {"gate": "R3", "phase": "P13.7",
                                                       "result": "passed"}),
                  "R3 bare passed without proofs")
    # False proof value.
    expect_reject(lambda d: d["gates"][3].__setitem__("denied_observed", False),
                  "R4 denied_observed=false")
    # R5 checksum divergence.
    expect_reject(lambda d: d["gates"][4].__setitem__("post_reattach_sha256", "b" * 64),
                  "R5 checksum divergence")
    # R10 leaks.
    expect_reject(lambda d: d["gates"][9].__setitem__("owned_leaks", 1),
                  "R10 owned_leaks=1")
    # Modified provider.
    expect_reject(lambda d: d["toolchain"].__setitem__("provider_modified", True),
                  "provider_modified=true")
    # Fake provider flag.
    expect_reject(lambda d: d.__setitem__("fake_provider", True),
                  "fake_provider=true")
    # Aggregate "passed" with a failed gate.
    def fail_gate(d):
        d["gates"][8]["result"] = "failed"
    expect_reject(fail_gate, "aggregate passed with failed R9")
    # Missing gate.
    expect_reject(lambda d: d["gates"].pop(0), "missing R1 row")
    # Duplicate gate.
    expect_reject(lambda d: d["gates"].append(dict(d["gates"][0])), "duplicate R1 row")
    # Secret material.
    expect_reject(lambda d: d.__setitem__("note", "-----BEGIN PRIVATE KEY-----"),
                  "embedded private key")
    expect_reject(lambda d: d.__setitem__("token", "Bearer abcdefghijklmnop"),
                  "bearer token")
    # Wrong backend.
    expect_reject(lambda d: d.__setitem__("backend", "sqlite"), "sqlite backend")
    print("P13.7 evidence validator self-test: PASS")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", nargs="?", help="evidence JSON to validate")
    parser.add_argument("--self-test", action="store_true",
                        help="prove tampered/missing-proof evidence is rejected")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return
    if not args.evidence:
        parser.error("evidence path is required unless --self-test is given")
    outcome = validate_document(load_json(args.evidence))
    print(f"P13.7 evidence validation: PASS ({outcome['gates']} gates, "
          f"verdict {outcome['verdict'].upper()})")


if __name__ == "__main__":
    try:
        main()
    except Failure as error:
        print(f"P13.7 evidence validator: FAIL: {error}", file=sys.stderr)
        sys.exit(2)
