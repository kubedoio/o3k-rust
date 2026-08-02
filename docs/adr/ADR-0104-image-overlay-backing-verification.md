# ADR-0104 — Verify image overlays before publication

## Status

Accepted

## Context

`qemu-img create` can exit successfully while the resulting temporary image
does not have the format or backing chain that the image cache expects. The
cache previously published that output after checking only the process exit
status, allowing a malformed or foreign-backed overlay to become a managed
artifact.

## Decision

Create overlays with an explicit format for both the new image and its
backing image:

```text
qemu-img create -f qcow2 -b <managed-base> -F qcow2 <temporary-overlay>
```

After `qemu-img create`, inspect the temporary overlay with
`qemu-img info --output=json`. Publication is allowed only when the reported
overlay format and `backing-filename-format` are exactly `qcow2`, at least one
backing filename is present, and every reported `backing-filename` or
`full-backing-filename` resolves to the exact canonical managed base path.
Missing, malformed, failed, or mismatched metadata fails closed and removes
the temporary output before the final atomic rename.

The executable path remains `qemu-img` by default; the private constructor
injection used by unit tests does not change the production command contract.

## Consequences

Malformed, raw, backing-less, and foreign-backed outputs are never published
as managed overlays. This adds one metadata query per newly-created overlay.
Existing final overlays retain the existing idempotent behavior and are not
revalidated by this bounded change.

The change is repository-side only. It does not claim Glance integration,
compute-agent image realization, or trusted real-host qemu-img evidence.

## Public sources

- QEMU [`qemu-img create`](https://qemu.readthedocs.io/en/v9.2.4/tools/qemu-img.html#create)
  and [`qemu-img info`](https://qemu.readthedocs.io/en/v9.2.4/tools/qemu-img.html#info)
  public command behavior and JSON information output, verified through the
  executable contract represented by the deterministic fake-qemu regression
  test (accessed 2026-08-02).
- OpenStack Glance's [disk and container format
  conventions](https://docs.openstack.org/glance/latest/user/formats.html)
  and the [OpenStack image guide's QEMU/KVM image
  guidance](https://docs.openstack.org/image-guide/obtain-images.html)
  (accessed 2026-08-02).
- Rust standard library `std::fs::canonicalize`, accessed 2026-07-31.
