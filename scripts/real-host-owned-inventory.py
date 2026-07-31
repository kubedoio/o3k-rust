#!/usr/bin/env python3
"""Collect a redacted, stable host inventory for the real-host guard.

The collector deliberately records only O3K-owned identities. Foreign host
state is represented by hashes so the verifier can detect mutation without
publishing unrelated domain, interface, or provider identities.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]*$")
RESOURCES = ("server", "image", "network", "subnet", "flavor")
RESOURCE_COMMANDS = {
    "server": ("server", "list", "--name", "o3k-testlab-server", "-f", "value", "-c", "ID"),
    "image": ("image", "list", "--name", "o3k-testlab-image", "-f", "value", "-c", "ID"),
    "network": ("network", "list", "--name", "o3k-testlab-network", "-f", "value", "-c", "ID"),
    "subnet": ("subnet", "list", "--name", "o3k-testlab-subnet", "-f", "value", "-c", "ID"),
    "flavor": ("flavor", "list", "--name", "o3k-testlab-flavor", "-f", "value", "-c", "ID"),
}


def command(args: tuple[str, ...], *, scrub_provider_config: bool = False) -> str | None:
    environment = os.environ.copy()
    if scrub_provider_config:
        environment.pop("OS_CLOUD", None)
        environment.pop("OS_CLIENT_CONFIG_FILE", None)
    try:
        result = subprocess.run(
            args,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=True,
            timeout=10,
            text=True,
        )
    except (OSError, UnicodeError, subprocess.SubprocessError):
        return None
    return result.stdout


def digest(values: list[str]) -> str:
    payload = "\n".join(sorted(values)).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def foreign_link_lines(output: str) -> list[str]:
    lines = []
    for line in output.splitlines():
        value = line.strip()
        if not value:
            continue
        # `ip -o link` starts with an integer index and then the interface
        # name. Keep only non-O3K interfaces in the foreign-state digest.
        fields = value.split(":", 2)
        name = fields[1].strip().split("@", 1)[0] if len(fields) > 1 else ""
        if not name.startswith("o3k-"):
            lines.append(value)
    return lines


def snapshot() -> dict[str, object] | None:
    domain_output = command(("virsh", "-c", "qemu:///system", "list", "--all", "--name"))
    link_output = command(("ip", "-o", "link", "show"))
    if domain_output is None or link_output is None:
        return None

    domains: list[str] = []
    foreign_domains: list[str] = []
    for value in (line.strip() for line in domain_output.splitlines() if line.strip()):
        if value.startswith("o3k-"):
            if SAFE_ID.fullmatch(value) is None:
                return None
            domains.append(value)
        else:
            foreign_domains.append(value)

    openstack_status = "not_checked"
    resources: dict[str, list[str]] = {}
    if os.environ.get("O3K_REAL_HOST_OPENSTACK_INVENTORY", "false") == "true" and os.environ.get("OS_PASSWORD"):
        openstack_status = "available"
        for resource in RESOURCES:
            output = command(("openstack", *RESOURCE_COMMANDS[resource]), scrub_provider_config=True)
            if output is None:
                return None
            values = []
            for value in (line.strip() for line in output.splitlines() if line.strip()):
                if SAFE_ID.fullmatch(value) is None:
                    return None
                values.append(value)
            resources[resource] = sorted(set(values))
    else:
        resources = {resource: [] for resource in RESOURCES}

    return {
        "schema_version": 2,
        "status": "available",
        "redacted": True,
        "domains": sorted(set(domains)),
        "openstack": {"status": openstack_status, "resources": resources},
        "foreign_state": {
            "domains_sha256": digest(foreign_domains),
            "network_links_sha256": digest(
                foreign_link_lines(link_output)
            ),
        },
    }


def write_atomic(path: Path, document: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent, text=True)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(document, output, indent=2, sort_keys=True)
            output.write("\n")
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} OUTPUT_JSON", file=sys.stderr)
        return 2
    output = Path(sys.argv[1])
    previous: str | None = None
    for _attempt in range(3):
        current = snapshot()
        if current is None:
            write_atomic(output, {"status": "unavailable", "reason": "inventory_collection_failed", "redacted": True})
            return 1
        canonical = json.dumps(current, sort_keys=True, separators=(",", ":"))
        if previous == canonical:
            write_atomic(output, current)
            return 0
        previous = canonical
    write_atomic(output, {"status": "unavailable", "reason": "inventory_not_stable", "redacted": True})
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
