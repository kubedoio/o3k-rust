# O3K brand guide

Version 1.0.0 — 2026-09-09. This directory is the canonical source for the O3K visual identity: logo assets, infrastructure glyphs, illustrations, and machine-readable design tokens (`tokens.json`).

## Product idea

O3K is one Cloud Operating System from edge to datacenter: a Cloud Kernel built from small capability-bearing infrastructure building blocks on a structured substrate. It grows by adding blocks, not by replatforming.

```text
small kernel + atomic capability blocks + structured substrate
           = a cloud that grows from edge to datacenter
```

Do not visualize O3K as a generic cloud.

## Name treatment

- The product name is **O3K** in prose. It is a compact systems name; the characters do not formally expand to words.
- The graphic wordmark is uppercase **O3K** set in Inter Display Bold and shipped as vector paths.
- The round `O`, three active modules and `K`/Kernel relationship are visual rhyme, not an acronym claim.

## Logo concept — Bounded Growth

The mark is a 3×3 infrastructure grid with three active modules. Two remain inside the substrate; one crosses the top-right boundary while remaining on the same grid rhythm, leaving a dashed slot behind. The geometry expresses a small seed, composability, structured growth and scaling without replatforming.

See `concepts/logo-concepts.md` for evaluated alternatives and scoring.

### Canonical assets

| File | Use |
|---|---|
| `logo/o3k-mark.svg` | mark, light surfaces |
| `logo/o3k-mark-reversed.svg` | mark, dark surfaces |
| `logo/o3k-mark-mono.svg` | mark, single `currentColor` |
| `logo/o3k-wordmark.svg` | wordmark |
| `logo/o3k-wordmark-mono.svg` | monochrome wordmark |
| `logo/o3k-horizontal.svg` | primary light lockup |
| `logo/o3k-horizontal-reversed.svg` | primary dark lockup |
| `logo/o3k-horizontal-mono.svg` | monochrome lockup |
| `logo/o3k-stacked.svg` | stacked lockup |
| `logo/favicon.svg` | favicon/app tile |

Raster/social exports remain derivable release artifacts; the repository keeps the vector source of truth small and deterministic.

### Clear space and minimum size

- Clear space: one module cell on every side.
- Mark minimum: 16 px.
- Horizontal lockup minimum: 96 px wide.
- Stacked lockup minimum: 64 px wide.
- Below 24 px prefer the mark or `favicon.svg`.

### Color use

- Light: blue-600 `#2563EB`, cyan-500 `#06B6D4`, slate-300 `#CBD5E1`.
- Dark: blue-500 `#3B82F6`, cyan-400 `#22D3EE`, ink-700 `#33517A`.
- Maximum three colors in the logo.
- Cyan is a signal accent, never body text.
- Semantic status colors never appear in brand marks.

### Monochrome and dark backgrounds

Monochrome assets use `currentColor`; substrate outlines retain reduced opacity so the active/substrate hierarchy survives. Use reversed assets on ink-950/ink-900 surfaces.

### Misuse

Do not redraw, stretch, rotate or re-grid the mark; move the escaped module back into the panel; add shadows, gradients, glass effects or 3D; recolor modules with status colors; use the substrate as a generic apps icon; invent an O3K acronym expansion; or imitate third-party vendor marks.

## Typography

- Brand: **Inter Display Bold**, SIL OFL 1.1, vectorized in canonical logos.
- UI/docs: `Inter, ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif`.
- Code/terminal: `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`.

## Grid and icon language

All O3K visuals share one grammar: rounded modules on an orthogonal grid.

- filled module = active/owned capability;
- outlined module = substrate slot;
- dashed module = in-transit or vacated capacity;
- module outside a panel = growth beyond the original seed.

## Infrastructure glyph set

`icons/` contains the canonical 45-glyph, 24×24 `currentColor` family with 1.8px strokes, round joins and orthogonal geometry. It covers foundation, location/scale, compute, network, storage, platform and governance concepts including edge-site, datacenter, compute, server, network, fabric, dataplane, datapath, control-plane, storage, scheduler, reconciler, service catalog, IAM, quota, audit and usage.

Rules for new glyphs: 24×24 viewBox; stroke 1.8; round caps/joins; module corner radius ≈2; orthogonal first; no gradients, shadows or isometric 3D; readable at 16 px.

External technologies such as Kubernetes, Ceph and OpenStack remain external brands. Use official marks only when identifying those technologies and keep them separate from O3K-native glyphs.

## Illustrations

The canonical architectural visual set is:

1. `illustrations/o3k-edge-to-datacenter.svg` — same substrate across the scale continuum;
2. `illustrations/o3k-kernel-and-providers.svg` — authority in the Cloud Kernel and typed execution southbound;
3. `illustrations/o3k-building-block-expansion.svg` — deterministic block-join growth.

Illustrations are vector-first and architectural. Avoid literal cloud clipart, buildings, racks or decorative journeys.

## Accessibility

- Semantic token pairs in `tokens.json` target WCAG AA on intended surfaces.
- cyan-500 on white is decorative only.
- Icons inherit accessible context via `currentColor`.
- Web-facing logo SVGs carry accessible titles/descriptions.

## Relationship to Kubedo

Kubedo GmbH owns and develops O3K. No documented canonical Kubedo token palette was found when this identity was prepared, so this palette formalizes the infrastructure colors already present in O3K architecture visuals. If Kubedo later publishes canonical brand tokens, reconcile deliberately rather than silently replacing either system.

## Relationship to Araf

**O3K is the substrate/kernel identity. Araf is the human-interface identity built on the same visual grammar.**

Araf consumes the palette, module grammar and resource icon language while retaining its own neutral application surfaces and separate Tenant/Operator trust contexts.

## Files

```text
brand/
  README.md
  tokens.json
  concepts/logo-concepts.md
  logo/
  icons/
  illustrations/
```

Brand assets follow the project Apache-2.0 license. Inter is used under SIL OFL 1.1 and is not redistributed as a font file here.
