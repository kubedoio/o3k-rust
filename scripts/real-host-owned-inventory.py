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
import stat as stat_module
import subprocess
import sys
import tempfile
from pathlib import Path

SAFE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]*$")
RESOURCES = ("server", "image", "network", "subnet", "flavor")
MAX_PROTECTED_FILE_BYTES = 64 * 1024 * 1024
MAX_PROTECTED_ENTRIES = 10_000
MAX_PROTECTED_TOTAL_BYTES = 256 * 1024 * 1024
RESOURCE_COMMANDS = {
    "server": ("server", "list", "--name", "o3k-testlab-server", "-f", "value", "-c", "ID"),
    "image": ("image", "list", "--name", "o3k-testlab-image", "-f", "value", "-c", "ID"),
    "network": ("network", "list", "--name", "o3k-testlab-network", "-f", "value", "-c", "ID"),
    "subnet": ("subnet", "list", "--name", "o3k-testlab-subnet", "-f", "value", "-c", "ID"),
    "flavor": ("flavor", "list", "--name", "o3k-testlab-flavor", "-f", "value", "-c", "ID"),
}
LAST_FAILURE_REASON = "inventory_collection_failed"


def command(args: tuple[str, ...], *, scrub_provider_config: bool = False) -> str | None:
    global LAST_FAILURE_REASON
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
            stderr=subprocess.PIPE,
            check=True,
            timeout=10,
            text=True,
        )
    except FileNotFoundError:
        LAST_FAILURE_REASON = "command_unavailable:" + ":".join(args[:3])
        return None
    except subprocess.TimeoutExpired:
        LAST_FAILURE_REASON = "command_timeout:" + ":".join(args[:3])
        return None
    except subprocess.CalledProcessError as error:
        stderr = error.stderr if isinstance(error.stderr, str) else ""
        status = next(
            (code for code in (401, 403, 404, 409, 500, 502, 503, 504)
             if str(code) in stderr),
            None,
        )
        suffix = f":http{status}" if status is not None else f":exit{error.returncode}"
        LAST_FAILURE_REASON = "command_failed:" + ":".join(args[:3]) + suffix
        return None
    except (OSError, UnicodeError, subprocess.SubprocessError):
        LAST_FAILURE_REASON = "command_error:" + ":".join(args[:3])
        return None
    return result.stdout


def digest(values: list[str]) -> str:
    payload = "\n".join(sorted(values)).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def protected_paths_digest() -> str | None:
    """Hash an explicit, redacted allowlist of host paths and their contents."""
    global LAST_FAILURE_REASON
    raw_allowlist = os.environ.get("O3K_REAL_HOST_PROTECTED_PATHS")
    if raw_allowlist is None:
        return None
    paths = []
    for raw in raw_allowlist.splitlines():
        value = raw.strip()
        if not value or value.startswith("#"):
            continue
        path = Path(value)
        if (not path.is_absolute() or "\x00" in value
                or ".." in path.parts):
            LAST_FAILURE_REASON = "protected_path_allowlist_invalid"
            return None
        paths.append(Path(os.path.abspath(path)))

    records: list[str] = []
    total_bytes = 0
    for root in sorted(set(paths), key=str):
        try:
            candidates = [root]
            if root.is_dir():
                for candidate in root.rglob("*"):
                    candidates.append(candidate)
                    if len(candidates) > MAX_PROTECTED_ENTRIES:
                        LAST_FAILURE_REASON = "protected_path_too_many_entries"
                        return None
            for candidate in candidates:
                stat = candidate.lstat()
                kind = ("dir" if stat_module.S_ISDIR(stat.st_mode)
                        else "file" if stat_module.S_ISREG(stat.st_mode)
                        else "symlink" if stat_module.S_ISLNK(stat.st_mode) else "other")
                content = ""
                if kind == "file":
                    if stat.st_size > MAX_PROTECTED_FILE_BYTES:
                        LAST_FAILURE_REASON = "protected_path_file_too_large"
                        return None
                    total_bytes += stat.st_size
                    if total_bytes > MAX_PROTECTED_TOTAL_BYTES:
                        LAST_FAILURE_REASON = "protected_path_total_too_large"
                        return None
                    hasher = hashlib.sha256()
                    with candidate.open("rb") as stream:
                        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                            hasher.update(chunk)
                    content = hasher.hexdigest()
                elif kind == "symlink":
                    content = hashlib.sha256(os.readlink(candidate).encode("utf-8")).hexdigest()
                relative = "." if candidate == root else str(candidate.relative_to(root))
                records.append(json.dumps({
                    "path_sha256": hashlib.sha256(str(root).encode("utf-8")).hexdigest(),
                    "relative_path_sha256": hashlib.sha256(relative.encode("utf-8")).hexdigest(),
                    "kind": kind,
                    "mode": stat.st_mode,
                    "size": stat.st_size,
                    "mtime_ns": stat.st_mtime_ns,
                    "content_sha256": content,
                }, sort_keys=True, separators=(",", ":")))
        except (OSError, UnicodeError, ValueError):
            LAST_FAILURE_REASON = "protected_path_unreadable"
            return None
    return digest(records)


def classify_link_lines(output: str) -> tuple[list[str], list[str]]:
    owned: list[str] = []
    foreign: list[str] = []
    for line in output.splitlines():
        value = line.strip()
        if not value:
            continue
        # `ip -o link` starts with an integer index and then the interface
        # name. Keep only non-O3K interfaces in the foreign-state digest.
        fields = value.split(":", 2)
        name = fields[1].strip().split("@", 1)[0] if len(fields) > 1 else ""
        if name.startswith("o3k-"):
            if SAFE_ID.fullmatch(name) is None:
                return [], []
            owned.append(name)
        else:
            foreign.append(value)
    return sorted(set(owned)), foreign


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

    openstack_requested = os.environ.get("O3K_REAL_HOST_OPENSTACK_INVENTORY", "false") == "true"
    openstack_status = "not_checked"
    resources: dict[str, list[str]] = {}
    if openstack_requested and not os.environ.get("OS_PASSWORD"):
        # A protected run that requests provider inventory must not silently
        # turn missing credentials into an empty, apparently clean snapshot.
        return None
    if openstack_requested:
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

    network_links, foreign_links = classify_link_lines(link_output)
    if not network_links and any(
        line.strip() and line.split(":", 2)[1].strip().split("@", 1)[0].startswith("o3k-")
        for line in link_output.splitlines()
        if len(line.split(":", 2)) > 1
    ):
        return None

    protected_paths = protected_paths_digest()
    if protected_paths is None:
        return None

    return {
        "schema_version": 2,
        "status": "available",
        "redacted": True,
        "domains": sorted(set(domains)),
        "network_links": network_links,
        "openstack": {"status": openstack_status, "resources": resources},
        "foreign_state": {
            "domains_sha256": digest(foreign_domains),
            "network_links_sha256": digest(foreign_links),
            "protected_paths_sha256": protected_paths,
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
            write_atomic(output, {"status": "unavailable", "reason": LAST_FAILURE_REASON, "redacted": True})
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
