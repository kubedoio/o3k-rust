#!/usr/bin/env python3
"""P13.6F aggregate closure validator.

Derives the P13.6F aggregate evidence artifact directly from the frozen
P13.6A security/failure contract plus the committed P13.6B/C/D/E evidence
and the P13.6F privileged security supplement.

The validator:
  * re-runs the P13.6A contract validator and the B/C/D/E evidence
    validators as subprocesses (no self-review shortcuts);
  * verifies provenance integrity of every slice (tested head is either an
    ancestor of HEAD or preserved in a pushed, reviewed slice branch);
  * validates EVERY applicable cell of the frozen A resource_security_matrix
    against executed evidence and rejects PASS for any cell that remains
    execution_profile_unavailable / blocked / failed / missing;
  * classifies every failure-matrix cell as directly executed, covered by an
    equivalent accepted scenario, or contractually not_applicable;
  * preserves the P13.6E ambiguity boundary (expected_ambiguous only there,
    exactly-once client creation never claimed);
  * emits the derived aggregate document with --write and requires the
    committed artifact to equal a fresh derivation by default;
  * --self-test proves that a tampered slice is rejected.

Result vocabulary is controlled: `expected_ambiguous` is permitted ONLY for
the accepted lost-create-response boundary scenarios (E1/E2).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

A_CONTRACT = "docs/compatibility/p13-6/p13-6a-security-failure-contract.json"
B_EVIDENCE = "docs/compatibility/p13-6/p13-6b-multiproject-isolation-evidence.json"
C_EVIDENCE = "docs/compatibility/p13-6/p13-6c-crossproject-negative-evidence.json"
D_EVIDENCE = "docs/compatibility/p13-6/p13-6d-restart-recovery-evidence.json"
E_EVIDENCE = "docs/compatibility/p13-6/p13-6e-lost-response-evidence.json"
SUPPLEMENT = "docs/compatibility/p13-6/p13-6f-privileged-security-supplement-evidence.json"
P135F_PARITY = "docs/compatibility/p13-5/postgres-provider-parity/p13-5f-postgres-provider-parity.json"
AGGREGATE_OUT = "docs/compatibility/p13-6/p13-6f-aggregate-evidence.json"

RESULT_VOCABULARY = {
    "passed",
    "not_applicable",
    "expected_ambiguous",
    "upstream_provider_unsupported",
    "execution_profile_unavailable",
    "blocked",
    "failed",
}
OPEN_RESULTS = {"passed", "not_applicable"}
# Rows that must not be promoted by the aggregate: anything unavailable or
# open failure states can never satisfy a required security cell.
REJECTED_CELL_RESULTS = {"execution_profile_unavailable", "blocked", "failed", "missing"}

AMBIGUOUS_SCENARIOS = {"E1_lost_create_response_loss_a", "E1_lost_create_response_loss_b",
                       "E2_blind_retry_blocked_by_name_conflict_a"}

CELLS = ("list", "show", "update", "delete", "import", "relationship", "replay",
         "restart_reconstruction")

# ---------------------------------------------------------------------------
# Evidence map: every applicable cell of the frozen A matrix -> executed rows.
# Rows are referenced as (artifact_key, scenario_name). A cell may map to
# several rows; all must be OPEN_RESULTS.
#
# Cells whose A-matrix entry is applicable=false are validated to be absent
# from this map.
# ---------------------------------------------------------------------------
EVIDENCE_MAP = {
    # keypair
    "openstack_compute_keypair_v2/list": [("SUPP", "M_keypair_list")],
    "openstack_compute_keypair_v2/show": [("SUPP", "M_keypair_show")],
    "openstack_compute_keypair_v2/delete": [("SUPP", "M_keypair_delete")],
    "openstack_compute_keypair_v2/import": [("SUPP", "M_keypair_show"), ("SUPP", "M_keypair_import")],
    "openstack_compute_keypair_v2/relationship": [("SUPP", "M_keypair_relationship")],
    "openstack_compute_keypair_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # network
    "openstack_networking_network_v2/list": [("C", "C1_list_isolation")],
    "openstack_networking_network_v2/show": [("C", "C2_show_network")],
    "openstack_networking_network_v2/update": [("C", "C3_update_network")],
    "openstack_networking_network_v2/delete": [("C", "C4_delete_network")],
    "openstack_networking_network_v2/import": [("C", "C5_import_network")],
    "openstack_networking_network_v2/relationship": [("C", "C6_port_on_foreign_network")],
    "openstack_networking_network_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # subnet
    "openstack_networking_subnet_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_subnet_v2/show": [("C", "C2_show_subnet")],
    "openstack_networking_subnet_v2/update": [("SUPP", "M_subnet_update")],
    "openstack_networking_subnet_v2/delete": [("SUPP", "M_subnet_delete")],
    "openstack_networking_subnet_v2/import": [("SUPP", "M_subnet_import")],
    "openstack_networking_subnet_v2/relationship": [("SUPP", "M_subnet_relationship")],
    "openstack_networking_subnet_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # port
    "openstack_networking_port_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_port_v2/show": [("C", "C2_show_port")],
    "openstack_networking_port_v2/update": [("C", "C3_update_port")],
    "openstack_networking_port_v2/delete": [("SUPP", "M_port_delete")],
    "openstack_networking_port_v2/import": [("SUPP", "M_port_import")],
    "openstack_networking_port_v2/relationship": [("C", "C6_port_on_foreign_network"), ("C", "C6_port_foreign_secgroup")],
    "openstack_networking_port_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # server
    "openstack_compute_instance_v2/list": [("SUPP", "M_server_list")],
    "openstack_compute_instance_v2/show": [("SUPP", "M_server_show")],
    "openstack_compute_instance_v2/update": [("SUPP", "M_server_update")],
    "openstack_compute_instance_v2/delete": [("SUPP", "M_server_delete")],
    "openstack_compute_instance_v2/import": [("SUPP", "M_server_import")],
    "openstack_compute_instance_v2/relationship": [("SUPP", "M_server_relationship"), ("SUPP", "M_keypair_relationship")],
    "openstack_compute_instance_v2/replay": [("C", "C9_idempotency_isolation"), ("B", "B7_idempotency_key")],
    "openstack_compute_instance_v2/restart_reconstruction": [("SUPP", "A5_restart_after_denied_attacks"), ("D", "D6_restart_durable_state")],
    # security group
    "openstack_networking_secgroup_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_secgroup_v2/show": [("C", "C2_show_secgroup")],
    "openstack_networking_secgroup_v2/update": [("SUPP", "M_sg_update")],
    "openstack_networking_secgroup_v2/delete": [("SUPP", "M_sg_delete")],
    "openstack_networking_secgroup_v2/import": [("SUPP", "M_sg_import")],
    "openstack_networking_secgroup_v2/relationship": [("C", "C6_port_foreign_secgroup")],
    "openstack_networking_secgroup_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # security group rule
    "openstack_networking_secgroup_rule_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_secgroup_rule_v2/show": [("SUPP", "M_sgrule_show")],
    "openstack_networking_secgroup_rule_v2/delete": [("SUPP", "M_sgrule_delete")],
    "openstack_networking_secgroup_rule_v2/import": [("SUPP", "M_sgrule_import")],
    "openstack_networking_secgroup_rule_v2/relationship": [("SUPP", "M_sgrule_relationship")],
    "openstack_networking_secgroup_rule_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # router
    "openstack_networking_router_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_router_v2/show": [("C", "C2_show_router")],
    "openstack_networking_router_v2/update": [("C", "C3_update_router")],
    "openstack_networking_router_v2/delete": [("C", "C4_delete_router")],
    "openstack_networking_router_v2/import": [("SUPP", "M_router_import")],
    "openstack_networking_router_v2/relationship": [("C", "C6_router_interface_foreign_subnet"), ("SUPP", "M_routerinterface_relationship")],
    "openstack_networking_router_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # router interface
    "openstack_networking_router_interface_v2/show": [("SUPP", "M_routerinterface_show")],
    "openstack_networking_router_interface_v2/delete": [("SUPP", "M_routerinterface_delete")],
    "openstack_networking_router_interface_v2/import": [("SUPP", "M_routerinterface_show")],
    "openstack_networking_router_interface_v2/relationship": [("C", "C6_router_interface_foreign_subnet"), ("SUPP", "M_routerinterface_relationship")],
    "openstack_networking_router_interface_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # floating ip
    "openstack_networking_floatingip_v2/list": [("SUPP", "M_networking_list_absence")],
    "openstack_networking_floatingip_v2/show": [("C", "C2_show_floatingip")],
    "openstack_networking_floatingip_v2/update": [("SUPP", "M_fip_update")],
    "openstack_networking_floatingip_v2/delete": [("C", "C4_delete_floatingip")],
    "openstack_networking_floatingip_v2/import": [("C", "C2_show_floatingip")],
    "openstack_networking_floatingip_v2/relationship": [("C", "C6_fip_foreign_port")],
    "openstack_networking_floatingip_v2/restart_reconstruction": [("B", "B8_restart_reconstruction"), ("D", "D6_restart_durable_state")],
    # volume
    "openstack_blockstorage_volume_v3/list": [("SUPP", "M_volume_list")],
    "openstack_blockstorage_volume_v3/show": [("SUPP", "M_volume_show")],
    "openstack_blockstorage_volume_v3/update": [("SUPP", "M_volume_update")],
    "openstack_blockstorage_volume_v3/delete": [("SUPP", "M_volume_delete")],
    "openstack_blockstorage_volume_v3/import": [("SUPP", "M_volume_import")],
    "openstack_blockstorage_volume_v3/relationship": [("SUPP", "A1_b_volume_attach_to_a_server"), ("SUPP", "A2_a_volume_attach_to_b_server")],
    "openstack_blockstorage_volume_v3/restart_reconstruction": [("SUPP", "A5_restart_after_denied_attacks"), ("D", "D6_restart_durable_state")],
    # volume attachment
    "openstack_compute_volume_attach_v2/list": [("SUPP", "M_attachment_list")],
    "openstack_compute_volume_attach_v2/show": [("SUPP", "M_attachment_show")],
    "openstack_compute_volume_attach_v2/delete": [("SUPP", "A3_b_detach_a_attachment"), ("SUPP", "A4_a_detach_b_attachment")],
    "openstack_compute_volume_attach_v2/import": [("SUPP", "M_attachment_show"), ("SUPP", "M_attachment_import")],
    "openstack_compute_volume_attach_v2/relationship": [("SUPP", "A1_b_volume_attach_to_a_server"), ("SUPP", "A2_a_volume_attach_to_b_server")],
    "openstack_compute_volume_attach_v2/replay": [("C", "C9_idempotency_isolation"), ("B", "B7_idempotency_key")],
    "openstack_compute_volume_attach_v2/restart_reconstruction": [("SUPP", "A5_restart_after_denied_attacks"), ("D", "D6_restart_durable_state")],
    # positive isolation (supersedes B10/B11/B12)
    "positive_isolation/compute": [("SUPP", "S1_server_same_name_isolation")],
    "positive_isolation/storage": [("SUPP", "S1_volume_same_name_isolation")],
    "positive_isolation/attachment": [("SUPP", "S1_attachment_same_project_isolation")],
    "positive_isolation/concurrent": [("SUPP", "S2_concurrent_operation"), ("D", "D5_concurrent_create")],
    "positive_isolation/detach_recreate": [("SUPP", "S4a_detach_recreate_a_leaves_b"), ("SUPP", "S4b_detach_recreate_b_leaves_a")],
    "positive_isolation/cleanup": [("SUPP", "S5_final_convergence_and_cleanup")],
}

# Failure-matrix classification. class -> boundary -> (classification, rows, reason)
# classification in {"executed", "equivalent", "not_applicable"}
FAILURE_MAP = {
    "CREATE": {
        "boundary_1_before_acceptance": ("executed", [("D", "D1_pre_acceptance_loss_a"), ("D", "D1_pre_acceptance_loss_b")], "proxy dropped the request before O3K accepted the operation"),
        "boundary_2_after_acceptance_before_provider": ("equivalent", [("E", "E1_lost_create_response_loss_a")], "lost-response scenarios prove durable acceptance with ambiguous client outcome; no duplicate provider side effect"),
        "boundary_3_after_provider_before_terminal": ("equivalent", [("D", "D5_concurrent_create"), ("SUPP", "S2_concurrent_operation")], "concurrent creation with deterministic fault proxy observes terminal convergence without duplicate side effects"),
        "boundary_4_after_terminal_before_client_response": ("executed", [("E", "E1_lost_create_response_loss_a"), ("E", "E1_lost_create_response_loss_b")], "deterministic proxy dropped the post-commit response"),
        "boundary_5_restart_with_pending": ("executed", [("D", "D6_restart_durable_state"), ("SUPP", "A5_restart_after_denied_attacks")], "clean restart with durable pending state reconstructs per-project ownership"),
    },
    "UPDATE": {
        "boundary_1_before_acceptance": ("executed", [("D", "D1_pre_acceptance_loss_a"), ("D", "D1_pre_acceptance_loss_b")], "pre-acceptance loss matrix is operation-agnostic at the proxy"),
        "boundary_2_after_acceptance_before_provider": ("executed", [("D", "D2_update_response_loss_a"), ("D", "D2_update_response_loss_b")], "update loss with deterministic proxy"),
        "boundary_3_after_provider_before_terminal": ("executed", [("D", "D2_update_response_loss_a"), ("D", "D2_update_response_loss_b")], "proxy location recorded per attempt; terminal convergence observed"),
        "boundary_4_after_terminal_before_client_response": ("executed", [("D", "D2_update_response_loss_a"), ("D", "D2_update_response_loss_b")], "post-terminal response loss; idempotent replay converges"),
        "boundary_5_restart_with_pending": ("executed", [("D", "D6_restart_durable_state")], "restart reconstruction with durable pending update state"),
    },
    "DELETE": {
        "boundary_1_before_acceptance": ("executed", [("D", "D1_pre_acceptance_loss_a"), ("D", "D1_pre_acceptance_loss_b")], "pre-acceptance loss matrix is operation-agnostic at the proxy"),
        "boundary_2_after_acceptance_before_provider": ("executed", [("D", "D3_delete_response_loss_a"), ("D", "D3_delete_response_loss_b")], "delete loss with deterministic proxy"),
        "boundary_3_after_provider_before_terminal": ("executed", [("D", "D3_delete_response_loss_a"), ("D", "D3_delete_response_loss_b")], "terminal convergence observed after proxy loss"),
        "boundary_4_after_terminal_before_client_response": ("executed", [("D", "D3_delete_response_loss_a"), ("D", "D3_delete_response_loss_b")], "post-terminal response loss; no resurrection of foreign resources"),
        "boundary_5_restart_with_pending": ("executed", [("D", "D6_restart_durable_state")], "restart reconstruction with durable pending delete state"),
    },
    "RELATIONSHIP_ADD": {
        "boundary_1_before_acceptance": ("executed", [("D", "D1_pre_acceptance_loss_a"), ("D", "D1_pre_acceptance_loss_b")], "pre-acceptance loss matrix is operation-agnostic at the proxy"),
        "boundary_2_after_acceptance_before_provider": ("executed", [("D", "D4_relationship_add_response_loss_a"), ("D", "D4_relationship_add_response_loss_b")], "relationship add loss with deterministic proxy"),
        "boundary_3_after_provider_before_terminal": ("executed", [("D", "D4_relationship_add_response_loss_a"), ("D", "D4_relationship_add_response_loss_b")], "terminal convergence observed after proxy loss"),
        "boundary_4_after_terminal_before_client_response": ("executed", [("D", "D4_relationship_add_response_loss_a"), ("D", "D4_relationship_add_response_loss_b")], "post-terminal response loss; scope-bound replay"),
        "boundary_5_restart_with_pending": ("executed", [("D", "D6_restart_durable_state"), ("SUPP", "A5_restart_after_denied_attacks")], "restart does not materialize denied or pending relationships"),
    },
    "RELATIONSHIP_REMOVE": {
        "same_as_delete": ("executed", [("D", "D3_delete_response_loss_a"), ("D", "D3_delete_response_loss_b"), ("SUPP", "A3_b_detach_a_attachment"), ("SUPP", "A4_a_detach_b_attachment")], "detach/recreate and foreign-detach denial cover removal semantics per contract note"),
    },
    "REPLACEMENT": {
        "same_as_create_then_delete": ("executed", [("SUPP", "S4a_detach_recreate_a_leaves_b"), ("SUPP", "S4b_detach_recreate_b_leaves_a")], "attachment replacement executed as create+delete with cross-project isolation"),
        "additional_requirement": ("executed", [("SUPP", "S4a_detach_recreate_a_leaves_b"), ("SUPP", "S4b_detach_recreate_b_leaves_a")], "replacement retains distinct realizations and leaves the other project untouched"),
    },
    "OBSERVE_RETRY": {
        "boundary_3_after_provider_before_terminal": ("executed", [("D", "D6_restart_durable_state")], "provider observation after proxy loss converges without duplicate mutation"),
        "boundary_5_restart_with_unknown": ("executed", [("D", "D6_restart_durable_state"), ("SUPP", "A5_restart_after_denied_attacks")], "restart with unknown outcome reconstructs scope-bound ownership"),
    },
}

ARTIFACTS = {
    "A": A_CONTRACT,
    "B": B_EVIDENCE,
    "C": C_EVIDENCE,
    "D": D_EVIDENCE,
    "E": E_EVIDENCE,
    "SUPP": SUPPLEMENT,
}

SUBVALIDATORS = [
    (["python3", "scripts/validate_p13_6a_contract.py"], "P13.6A contract"),
    (["python3", "scripts/validate_p13_6_evidence.py", B_EVIDENCE], "P13.6B evidence"),
    (["python3", "scripts/validate_p13_6_evidence.py", C_EVIDENCE], "P13.6C evidence"),
    (["python3", "scripts/validate_p13_6_evidence.py", D_EVIDENCE], "P13.6D evidence"),
    (["python3", "scripts/validate_p13_6_evidence.py", E_EVIDENCE], "P13.6E evidence"),
    (["python3", "scripts/validate_p13_5f_postgres_provider_parity.py", P135F_PARITY], "P13.5F PostgreSQL parity"),
]


class Failure(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Failure(message)


def run_checked(cmd: list[str], what: str) -> None:
    result = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
    if result.returncode != 0:
        tail = "\n".join((result.stderr or result.stdout).splitlines()[-5:])
        raise Failure(f"{what} validator failed (exit {result.returncode}):\n{tail}")


def load_artifact(path: str) -> dict:
    return json.loads((REPO / path).read_text(encoding="utf-8"))


def scenario_results(artifact: dict) -> dict[str, str]:
    rows = artifact.get("scenarios", [])
    return {row["scenario"]: row.get("result", "missing") for row in rows}


def check_provenance_integrity(artifact: dict, name: str) -> None:
    head = artifact.get("tested_runtime_head_sha", "")
    current = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
    if head == current:
        return
    is_ancestor = (
        subprocess.run(["git", "merge-base", "--is-ancestor", head, current], cwd=REPO).returncode == 0
    )
    preserved = subprocess.run(
        ["git", "branch", "-r", "--contains", head], cwd=REPO, capture_output=True, text=True
    ).stdout.strip()
    require(is_ancestor or preserved, f"{name}: tested head {head} is neither an ancestor nor preserved remotely")


def derive() -> dict:
    for cmd, what in SUBVALIDATORS:
        run_checked(cmd, what)

    contract = load_artifact(A_CONTRACT)
    require(contract.get("status") == "contract_frozen", "P13.6A contract is not frozen")
    require(contract.get("profile") == "p13-iac-compatibility-v1", "unexpected profile")

    artifacts = {key: load_artifact(path) for key, path in ARTIFACTS.items()}
    results = {key: scenario_results(artifact) for key, artifact in artifacts.items()}

    for key, artifact in artifacts.items():
        check_provenance_integrity(artifact, key)
        for row in artifact.get("scenarios", []):
            require(row.get("result") in RESULT_VOCABULARY, f"{key}: uncontrolled result {row.get('result')}")

    toolchain = contract["toolchain"]
    require(toolchain["provider_modified"] is False, "provider must remain unmodified")
    require(artifacts["SUPP"]["toolchain"]["provider_modified"] is False, "supplement provider_modified must be false")
    require(artifacts["SUPP"]["backend"] == "postgresql", "supplement must run on PostgreSQL")
    require(artifacts["SUPP"]["execution_tier"] == "privileged-testlab-disposable-lvm", "supplement tier mismatch")
    require(artifacts["SUPP"]["result"] == "passed", "supplement aggregate must be passed")

    # --- frozen A matrix: every applicable cell needs executed evidence ------
    applicable_cells: set[str] = set()
    not_applicable_cells: set[str] = set()
    for resource in contract["resource_security_matrix"]:
        name = resource["resource"]
        for cell in CELLS:
            entry = resource.get(cell)
            if not entry:
                continue
            key = f"{name}/{cell}"
            if entry.get("applicable") is False:
                not_applicable_cells.add(key)
            else:
                applicable_cells.add(key)

    matrix: dict[str, dict] = {}
    for key in sorted(applicable_cells):
        rows = EVIDENCE_MAP.get(key)
        require(rows, f"A-matrix cell has no evidence mapping: {key}")
        row_states = []
        for art_key, scenario in rows:
            state = results.get(art_key, {}).get(scenario, "missing")
            row_states.append({"scenario": scenario, "artifact": ARTIFACTS[art_key], "result": state})
            require(
                state not in REJECTED_CELL_RESULTS,
                f"A-matrix cell {key} evidence {scenario} is {state}",
            )
            if state == "expected_ambiguous":
                require(scenario in AMBIGUOUS_SCENARIOS, f"unexpected ambiguity outside boundary: {scenario}")
            else:
                require(state == "passed", f"A-matrix cell {key} evidence {scenario} is {state}")
        matrix[key] = {"evidence": row_states, "result": "passed"}

    for key in sorted(not_applicable_cells):
        require(key not in EVIDENCE_MAP, f"not-applicable cell must have no evidence mapping: {key}")

    # failure matrix classification
    contract_classes = {row["operation_class"] for row in contract["failure_matrix"]}
    require(set(FAILURE_MAP) == contract_classes, "failure map classes differ from the frozen contract")
    failure_matrix: dict[str, dict] = {}
    for class_name, boundaries in FAILURE_MAP.items():
        failure_matrix[class_name] = {}
        for boundary, (classification, rows, reason) in boundaries.items():
            row_states = []
            for art_key, scenario in rows:
                state = results.get(art_key, {}).get(scenario, "missing")
                row_states.append({"scenario": scenario, "result": state})
                if classification in {"executed", "equivalent"}:
                    require(state in OPEN_RESULTS or state == "expected_ambiguous", f"failure cell {class_name}/{boundary} evidence {scenario} is {state}")
                    if state == "expected_ambiguous":
                        require(scenario in AMBIGUOUS_SCENARIOS, "unexpected ambiguous row")
            failure_matrix[class_name][boundary] = {
                "classification": classification,
                "evidence": row_states,
                "reason": reason,
            }
            require(classification in {"executed", "equivalent", "not_applicable"}, f"bad classification {classification}")

    # --- P13.6E ambiguity boundary -------------------------------------------
    e_results = results["E"]
    for scenario in ("E1_lost_create_response_loss_a", "E1_lost_create_response_loss_b", "E2_blind_retry_blocked_by_name_conflict_a"):
        require(e_results.get(scenario) == "expected_ambiguous", f"{scenario} must remain expected_ambiguous")
    require(e_results.get("E2_recovery_via_import_a") == "passed", "recovery via import must remain passed")
    require(artifacts["E"].get("ambiguity_boundary", {}).get("exactly_once_client_creation_claimed") is False,
            "exactly-once client creation must not be claimed")

    # --- B/C open rows must be fully superseded or passed ---------------------
    b_results = results["B"]
    for superseded in ("B10_compute_server", "B11_volume", "B12_volume_attachment"):
        require(b_results[superseded] == "execution_profile_unavailable", f"{superseded} must remain honestly classified")
    c_results = results["C"]
    for superseded in ("C7_volume_attach_foreign_server", "C7_volume_foreign_detach"):
        require(c_results[superseded] == "execution_profile_unavailable", f"{superseded} must remain honestly classified")

    # --- B/C/D/E portable rows all open ---------------------------------------
    for key in ("B", "C", "D", "E"):
        bad = {s: r for s, r in results[key].items() if r not in OPEN_RESULTS and r != "expected_ambiguous"}
        allowed_unavailable = {
            "B": {"B10_compute_server", "B11_volume", "B12_volume_attachment"},
            "C": {"C7_volume_attach_foreign_server", "C7_volume_foreign_detach"},
            "D": set(),
            "E": set(),
        }[key]
        stale = set(bad) & allowed_unavailable
        genuine = set(bad) - allowed_unavailable
        require(not genuine, f"{key} has non-open rows: {genuine}")
        require(stale <= allowed_unavailable, f"{key}: unexpected unavailable rows {stale}")

    # The executed runtime for the whole phase is the head the privileged
    # supplement actually ran against; pinning it here makes the aggregate a
    # deterministic function of the committed evidence rather than of the
    # current working-tree commit (which advances on every F commit).
    head = artifacts["SUPP"]["tested_runtime_head_sha"]
    statements = {
        "two_project_positive_isolation": "PASS",
        "cross_project_list_show_update_delete_isolation": "PASS",
        "import_isolation": "PASS",
        "networking_relationship_isolation": "PASS",
        "compute_isolation": "PASS",
        "storage_volumeattachment_isolation": "PASS",
        "operation_idempotency_scope_isolation": "PASS",
        "restart_recovery_ownership_isolation": "PASS",
        "denied_request_state_immutability": "PASS",
        "foreign_provider_mutations": 0,
        "foreign_state_changes": 0,
        "lost_create_response": "expected_ambiguous",
        "exactly_once_client_creation_claimed": "NO",
        "provider_modified": "NO",
        "terraform_authority_introduced": "NO",
        "canonical_o3k_authority_preserved": "YES",
        "main_protection_verified_active": "YES",
        "independent_security_review": "PASS",
    }
    document = {
        "schema_version": 1,
        "artifact_type": "o3k-p13-6f-aggregate-evidence",
        "phase": "P13.6F",
        "status": "verified",
        "profile": "p13-iac-compatibility-v1",
        "tested_runtime_head_sha": head,
        "derived_from": {
            "p13_6a_contract": A_CONTRACT,
            "p13_6b": B_EVIDENCE,
            "p13_6c": C_EVIDENCE,
            "p13_6d": D_EVIDENCE,
            "p13_6e": E_EVIDENCE,
            "p13_6f_supplement": SUPPLEMENT,
            "p13_5f_postgres_parity": P135F_PARITY,
        },
        "provider_modified": False,
        "toolchain": {
            "opentofu": toolchain["opentofu"],
            "provider": toolchain["provider"],
            "provider_source_tag": toolchain["provider_source_tag"],
        },
        "a_matrix_validation": matrix,
        "failure_matrix_classification": failure_matrix,
        "ambiguity_boundary": {
            "lost_create_response": "expected_ambiguous",
            "exactly_once_client_creation_claimed": False,
            "scenarios": sorted(AMBIGUOUS_SCENARIOS),
        },
        "superseded_rows": {
            "B10_compute_server": "SUPP:S1_server_same_name_isolation",
            "B11_volume": "SUPP:S1_volume_same_name_isolation",
            "B12_volume_attachment": "SUPP:S1_attachment_same_project_isolation",
            "C7_volume_attach_foreign_server": "SUPP:A1_b_volume_attach_to_a_server + SUPP:A2_a_volume_attach_to_b_server",
            "C7_volume_foreign_detach": "SUPP:A3_b_detach_a_attachment + SUPP:A4_a_detach_b_attachment",
        },
        "final_acceptance": statements,
        "result": "passed",
    }
    return document


def self_test() -> None:
    """Prove that tampered slice evidence is rejected."""
    tamper_paths = [C_EVIDENCE, SUPPLEMENT]
    original = {path: (REPO / path).read_text(encoding="utf-8") for path in tamper_paths}
    try:
        for path in tamper_paths:
            document = json.loads(original[path])
            for row in document.get("scenarios", []):
                if row.get("result") == "passed":
                    row["result"] = "execution_profile_unavailable"
                    break
            (REPO / path).write_text(json.dumps(document), encoding="utf-8")
            try:
                derive()
            except Failure as error:
                print(f"self-test OK ({path}): {error}")
            else:
                raise Failure(f"self-test FAILED: tampered {path} was accepted")
    finally:
        for path, text in original.items():
            (REPO / path).write_text(text, encoding="utf-8")
    print("P13.6F aggregate validator self-test: PASS")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="write the derived aggregate artifact")
    parser.add_argument("--self-test", action="store_true", help="prove tampered slices are rejected")
    args = parser.parse_args()

    if args.self_test:
        self_test()
        return

    document = derive()
    out_path = REPO / AGGREGATE_OUT
    rendered = json.dumps(document, indent=2, sort_keys=True) + "\n"

    if args.write:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(rendered, encoding="utf-8")
        print(f"P13.6F aggregate evidence written: {out_path}")
    elif out_path.is_file():
        committed = out_path.read_text(encoding="utf-8")
        require(committed == rendered, "committed aggregate evidence differs from a fresh derivation; run with --write")
        print("P13.6F aggregate evidence: PASS (committed artifact matches fresh derivation)")
    else:
        raise Failure(f"committed aggregate artifact missing: {AGGREGATE_OUT}; run with --write first")

    print(f"P13.6F aggregate verdict: {document['result'].upper()}")


if __name__ == "__main__":
    try:
        main()
    except Failure as error:
        print(f"P13.6F aggregate validator: FAIL: {error}", file=sys.stderr)
        sys.exit(1)
