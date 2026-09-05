# Session State — PAPPL migration

*Updated 2026-09-06.*

**P5 is implemented and exercised.** The maintainer selected **12.5 pt on
all edges** and authorized the next step. This follows SpliX's ML-2165
setting; it is provisional for the family, not a hardware measurement.
G-1 remains open per model. Do not ask again for permission to use 12.5 pt.

## Current code

- `src/media.rs`: shared 12.5 pt driver constant. The transitional filter
  preserves fractional points and no longer derives hard margins from integer
  CUPS `Margins[]`. This is the maintainer-authorized exception to freezing 1.x.
- `goldens/`: 32 cases refreshed with this intentional behaviour change.
  30 SPL streams changed; two synthetic SPL streams stayed identical. All
  sidecars now record `hard_margin_pt`. Original 12 pt evidence is historical.
- `pappl-sys`: native CUPS page header fields now bound and checked against
  the C probe (49 additional fields); four additional CUPS/IPP constants.
- `pappl::application`: minimal mainloop, loopback system, driver capability
  registration, file-only geometry callbacks and strict R-6/H option checks.
  Driver descriptors live through mainloop teardown: PAPPL stores their pointer.
- `ml216x-printer-app`: new unsafe-free binary, eleven media, four resolution
  pairs, two sources, fourteen media types, BLACK_1 and explicit one-sided.
  Real SPL2 output is **not connected**; normal jobs fail with an explanation.
- `scripts/p5-probe.py`: real IPP/PWG integration matrix and `/dev/full`
  failure test. Uses temporary spool/config/output and stops its server.

## P5 findings that the next step must use

1. BLACK_1 PWG callbacks contain **full media** and zero `Margins[]`, while
   media-col retains 441 (0.01 mm). All 17 media/resolution cases passed,
   including exact line counts and eight corner/inset marks. Evidence:
   `docs/P5-MEASUREMENTS.json`; interpretation: `docs/MARGINS.md`.
2. Canonical PWG dimensions differ from the rounded legacy PPD points. A4 at
   600 dpi is 4960×7015, not the legacy full height of 7017.
3. Request matching IPP and raster resolutions. PAPPL can otherwise construct
   a different output header and pad/crop the incoming data. Options sanity
   alone does not validate the original client header. P9 must resolve this.
4. PAPPL's PWG page callback numbering is 1-based in source and measurement.
5. PAPPL 1.3.1 ignores a false `rwriteline_cb` return. The wrapper records the
   failure and refuses `rendpage`/`rendjob`; `/dev/full` produced job-state=aborted.
6. PAPPL increments PWG impressions itself. Do not double count.

## Next work

Extract `spl2-core`, adapt the full-media raster path, and connect SPL2 job,
page and band callbacks while preserving the new golden baseline. Keep the
original filter in-tree until P11 passes; keep the PPD permanently. USB then
socket remain required for the final application. Hardware G-1 and the P9
raster/dithering review remain outstanding. No hardware print was performed.

Checks: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`, golden checksums and both P5 integration modes.

## Step numbering

Two numbering schemes have been in use: the plan's own, and the one in the
prompt series the work is driven from. **The plan's numbering is authoritative
from here on**, in the documents and in every report. The mapping below covers
the steps where the two are known to differ or to coincide; blanks are steps
the prompt series has not named, and are left blank rather than guessed.

| Plan | What it covers | Prompt series |
|---|---|---|
| P1 | Repository audit and migration plan (`docs/MIGRATION-PLAN.md`) | — |
| P2 | Golden-file harness, and its validation | P2 |
| P3 | `pappl-sys`: hand-written FFI **and** the size/offset/enum layout harness | P3 + P6 |
| P4 | `pappl`: the safe wrapper and the `catch_unwind` callback shim | P7 |
| P5 | Minimal PAPPL app; the printable-area vs full-media experiment (`docs/MARGINS.md`) | — |
| P9 | Raster-type decision, and the dithering-exposure question | — |
| P11 | The gate after which the frozen 1.x filter may be removed (Q-5) | — |
| P12 | Hardware bring-up; release gate G-1, the physical margin measurement | P12 |

The prompt series splits P3 into the bindings (its P3) and their layout tests
(its P6); the plan keeps them in one step because the harness had to exist
before the first declaration. Where older documents in this repository say
P11 or P12, they mean the rows above.

**Reading order for a fresh agent:** this file, then
[`docs/MARGINS.md`](MARGINS.md) and `docs/DECISIONS.md`, then
`docs/GOLDEN-VALIDATION.md` and `CONTRIBUTING.md` for how output is verified
and what blessing a golden requires, then `docs/MIGRATION-PLAN.md` §7 and §9
for the target layout and the six corruption risks. The byte-for-byte
behaviour itself lives in `src/main.rs` around `compute_page_width_pixels`,
`hard_margin_bytes` and `band_placement`, and in `src/spl.rs` around
`begin_job`, `begin_page`, `write_compressed_band`, `end_page` and `end_job`.
The `v1.x-final` recovery anchor is an annotated tag: object `7c0cf2c`,
commit `33d4ff2`, both present on `origin`.
