# O3K brand guide

Version 1.0.0 — 2026-09-09. This directory is the canonical source for the O3K
visual identity: logo assets, the infrastructure glyph set, illustrations, and
machine-readable design tokens (`tokens.json`).

## Product idea

O3K is one Cloud Operating System from edge to datacenter: a Cloud Kernel built
from small capability-bearing infrastructure building blocks on a structured
substrate. It grows by adding blocks, not by replatforming. The identity exists
to express exactly that:

```
small kernel  +  atomic capability blocks  +  structured substrate
            =  a cloud that grows from edge to datacenter
```

Do not visualize O3K as a generic cloud.

## Name treatment

- The product name is **O3K** in prose. It is a compact systems name; the
  characters do not formally expand to words, and marketing copy must not
  invent an acronym expansion.
- The graphic wordmark is uppercase **O3K** set in Inter Display Bold. Both
  `O3K` and `o3k` were evaluated; uppercase won on technical gravity, exact
  match with the prose name, and small-size rhythm.
- The letterforms may conceptually echo the system: the round "O" rhymes with
  module corner radii, "3" with the three active modules of the mark, "K" with
  Kernel. This is visual rhyme, not an acronym claim.

## Logo concept — "Bounded Growth"

The mark is a 3×3 infrastructure grid drawn as light cell outlines. Three cells
are active (filled). Two sit inside the grid; the third has crossed the
top-right boundary and sits outside it on the same module rhythm, leaving its
dashed slot visible.

It encodes: a small seed, composable blocks, structured growth, and expansion
beyond the original panel without breaking the grid. See
`concepts/logo-concepts.md` for the evaluated alternatives and scoring.

### Assets

| File | Use |
|---|---|
| `logo/o3k-mark.svg` | mark only, light surfaces |
| `logo/o3k-mark-reversed.svg` | mark only, dark surfaces |
| `logo/o3k-mark-mono.svg` | mark only, single `currentColor` |
| `logo/o3k-wordmark.svg` | wordmark only (ink) |
| `logo/o3k-wordmark-mono.svg` | wordmark only, `currentColor` |
| `logo/o3k-horizontal.svg` | primary lockup, light surfaces |
| `logo/o3k-horizontal-reversed.svg` | primary lockup, dark surfaces |
| `logo/o3k-horizontal-mono.svg` | lockup, single `currentColor` |
| `logo/o3k-stacked.svg` | stacked lockup, light surfaces |
| `logo/favicon.svg` | favicon / app tile (ink-950 tile + reversed mark) |
| `logo/o3k-social-preview.svg` | GitHub/social preview, 1280×640 |
| `logo/png/` | pre-rendered PNGs of the above |

### Clear space and minimum size

- Clear space: one module cell (19 viewBox units ≈ 19% of mark width) on all
  sides. Nothing else enters this zone.
- Minimum sizes: mark 16 px; horizontal lockup 96 px wide; stacked lockup
  64 px wide. Below 24 px always prefer the mark alone or `favicon.svg`.

### Color use

- Default logo colors: blue-600 `#2563EB` modules + cyan-500 `#06B6D4` escape
  module + slate-300 `#CBD5E1` substrate on light; blue-500 `#3B82F6` +
  cyan-400 `#22D3EE` + ink-700 `#33517A` on dark.
- The logo never uses more than three colors.
- Cyan is the signal accent; it marks the module that leaves the panel. It
  must never dominate a composition and never be used for body text.
- Status/semantic colors never appear in brand marks.

### Monochrome

Mono assets use `currentColor` for every element, with substrate outlines at
45% stroke opacity so the active/substrate hierarchy survives in one color.
Set `color: #000` for pure black, `color: #fff` for pure white. Monochrome is
fully acceptable for print, CLI, and single-color contexts; the mark's
silhouette is designed to survive it.

### Dark backgrounds

Use the `-reversed` assets (or `favicon.svg`) on ink-950/ink-900 surfaces. On
dark, module fills step from blue-600 to blue-500 and cyan-500 to cyan-400 to
preserve contrast.

### Misuse

Do not:

- redraw, stretch, rotate, or re-grid the mark;
- move the escaped module back inside the panel;
- add shadows, gradients, glass effects, or 3D;
- place the mark on low-contrast or busy backgrounds;
- recolor modules with status colors;
- use the substrate grid as a generic "apps" icon elsewhere;
- invent an O3K acronym expansion in copy;
- imitate third-party vendor marks in O3K style.

## Typography

- Brand/wordmark: **Inter Display Bold** (SIL OFL 1.1). Canonical logo SVGs
  contain vector paths — no font installation is required to render them.
- UI and documentation: Inter, falling back to system sans
  (`ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif`).
- Code/terminal: `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`.
- Eyebrows/labels in illustrations: uppercase, letter-spacing 2.2px at 14px.

## Grid and icon language

All O3K visuals share one grammar: rounded-rect modules on an orthogonal grid.

- filled module = active / owned capability;
- outlined module = substrate slot (inactive capacity);
- dashed module = in-transit or vacated capacity (join, drain, escape);
- a module outside the panel boundary = growth beyond the original seed.

## Infrastructure glyph set

`icons/` contains the canonical 24×24 glyph family (`currentColor`, stroke
1.8, round joins, orthogonal geometry, readable at 16 px). These are one
coherent icon family for Araf, docs, and diagrams — not per-service logos.

Catalog (45 glyphs): account, audit, authorization, availability-zone,
block-storage, building-block, capability, cluster, compute, control-plane,
datacenter, datapath, dataplane, edge-site, fabric, failure-domain, gateway,
host, identity, image, network, network-policy, o3k-cloud-kernel, operation,
organization, project, provider, public-ip, quota, rack, reconciler, region,
relationship, resource, role, router, scheduler, server, service,
service-catalog, site, storage, subnet, usage, volume.

Rules for new glyphs: 24×24 viewBox; stroke 1.8; round caps/joins; module
corner radius ≈ 2; orthogonal first, diagonals only for arrows/connectors; no
gradients/shadows/3D; must read at 16 px; never imitate external vendor marks.

External technologies (Kubernetes, Ceph, OpenStack, …) are external brand
marks. Use their official marks only to identify that technology, keep them
visually separate from O3K-native glyphs, and respect their licenses and
trademark rules. Never draw a fake Kubernetes wheel inside the O3K family.

## Illustration rules

`illustrations/` holds the small canonical set:

1. `o3k-edge-to-datacenter.svg` — the scale continuum in module language;
2. `o3k-kernel-and-providers.svg` — Cloud Kernel + execution providers;
3. `o3k-building-block-expansion.svg` — deterministic block-join growth.

Illustrations are vector-first and architectural: ink-950 surface, card
surfaces `#111C2F`/`#0C1728`, module grammar for anything structural, cyan
only for eyebrows/signals. Never literal buildings, clouds, racks, or journeys.

## Accessibility

- All semantic (status) token pairs pass WCAG AA ≥ 4.5:1 on their intended
  surfaces; exact ratios are recorded in `tokens.json`.
- cyan-500 `#06B6D4` on white is decorative-only (2.4:1) — never text.
- Icons are `currentColor` and inherit the surrounding accessible color.
- Logo SVGs used on web pages carry `<title>`/`<desc>`; keep them when
  embedding.

## Relationship to Kubedo

Kubedo GmbH owns and develops O3K. No documented canonical Kubedo brand
palette was found at the time of writing (kubedo.io uses ad-hoc site styling;
no public Kubedo brand repository exists). This palette is therefore a newly
proposed O3K/Kubedo infrastructure-family palette, consistent with the colors
already used in `docs/architecture/*.svg`, and does not replace any existing
documented Kubedo identity. If Kubedo later publishes a canonical brand, this
system should be reconciled with it deliberately, not silently.

## Relationship to Araf

**O3K is the substrate/kernel identity. Araf is the human-interface identity
built on the same visual grammar.**

Araf (the tenant/operator console) consumes the glyph set, tokens, and module
grammar from this directory while keeping its own Cloudscape-derived neutral
UI surfaces. Product consoles should feel calm and operational; brand color is
concentrated in the O3K mark, eyebrows, and signal accents.

## Files

```
brand/
  README.md                 this guide
  tokens.json               machine-readable token source
  concepts/logo-concepts.md design review: alternatives, scoring, decision
  logo/                     canonical SVG assets + rendered PNGs
  icons/                    24x24 infrastructure glyph set
  illustrations/            canonical diagrammatic illustrations
```

All brand assets are Apache-2.0, same as the project. The wordmark is set in
Inter Display (SIL OFL 1.1) and shipped as vector paths.
