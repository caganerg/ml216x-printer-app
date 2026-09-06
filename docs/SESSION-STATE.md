# Session State — PAPPL migration

*Updated 2026-09-06.*

**P6 is implemented: `spl2-core` is extracted and the SPL2 callbacks are
connected.** The printer application now emits real SPL2/QPDL through PAPPL.
No hardware print has been performed and release gate G-1 is still open, per
model; 12.5 pt remains the maintainer's selection, not a measurement.
Do not ask again for permission to use 12.5 pt.

## Current code

- `crates/spl2-core`: the protocol engine, `#![forbid(unsafe_code)]` and
  dependency free. `qpdl` (was `src/spl.rs`), `geometry` (the pure half of
  `src/main.rs`), `engine` (the shared page/band seam), `media`, `log`, and
  `raster` behind the non-default `golden-replay` feature (Q-6). The frozen
  filter and the printer application drive the same code, so they cannot
  drift apart without a golden turning red.
- `src/main.rs`: now only the CUPS filter front end — argv, stdin, stderr and
  the page loop. All 32 goldens are byte identical across the split, and
  `goldens/SHA256SUMS` verifies unchanged.
- `crates/pappl`: owns the C boundary only. `RasterDriver` is the seam; the
  crate stays `Apache-2.0 OR MIT` because the GPL engine is linked by the
  binary, not by it (Q-8a). `RasterOptions` is validated at the boundary, so a
  driver cannot read an unchecked field.
- `crates/ml216x-printer-app`: `Spl2Driver` turns PAPPL raster jobs into QPDL;
  `media_table` holds the capability table with its PPD cross-checks. Job state
  is keyed by job id and dropped by `abandon_job`, so a failed job cannot leave
  a half-written stream for the next one. `--probe` still selects the P5
  geometry probe unchanged.
- `scripts/p5-probe.py`: three modes now. The default reproduces
  `docs/P5-MEASUREMENTS.json` byte for byte, `--spl` runs the same 17-case
  matrix through the SPL2 driver and checks the QPDL page headers, and
  `--device-failure` still requires job-state=aborted.

## The geometry adaptation, in one paragraph

PAPPL delivers full media; cups-filters delivers the printable area. The
horizontal axis needs no new rule — `band_placement`'s centring term is zero
for a sheet-wide line, leaving exactly the hard-margin subtraction the classic
path performs, and a test asserts the two paths place the sheet identically
across all 11 media and 4 resolutions. The vertical axis has no precedent in
the tree: the top margin is dropped and the page cut to the printable height.
That is open question **Q-13**, and G-1 must measure it.

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

## What the transport actually does today

Corrects an earlier statement here that the application "can only reach a
`file://` destination". Measured on 2026-09-06 by driving the built binary:

* **A `socket://` destination already works end to end.** With the server
  running, `add -d NAME -m samsung_ml216x -v socket://127.0.0.1:PORT` succeeds
  and an `ipptool` `Print-Job` of a PWG raster page delivers real QPDL to a
  loopback TCP sink — 28759 bytes, opening with the UEL and the PJL envelope
  and closing with `\t` plus the UEL. Nothing in the code had to change for
  this; `papplSystemSetPrinterDrivers` is called only on the probe path, and
  `add` works regardless.
* **USB is unexercised, not unimplemented.** No Samsung device is attached to
  this machine, so `usb://` has never been opened. PAPPL's built-in USB scheme
  is what would carry it.
* **The state file is PAPPL's, not ours.** The mainloop persists printers to
  `$XDG_CONFIG_HOME/ml216x-printer-app.state` and reloads them at startup
  without this repository calling either state function. A manual run that does
  not scope `XDG_CONFIG_HOME` writes into the user's real `~/.config`; only
  `scripts/p5-probe.py` scopes it today.

## Next work — P7, device transport and the job-geometry contract

**P7 opens with a defect, not with a feature.** A job submitted without an
explicit `printer-resolution` ran at 1200x600 while the document was rendered
at 600x600, and completed rather than failing: `cupsWidth=9921`,
`bandWidthB=1240`, `hardMarginB=27` for a document 4960 px wide with 620-byte
lines. That is open question **Q-14**, and it carries a possible out-of-bounds
read, because the scanline slice is sized from the options header while the
buffer may be sized from the document's. Order of work:

1. **Read `pappl/job-process.c` from `pappl 1.3.1-2.1`** and settle how the
   `rwriteline_cb` buffer is sized and whether PAPPL scales, pads or crops
   raster input. Everything else in P7 depends on the answer. Needs a network
   fetch (`apt-get source pappl`), so ask first.
2. **Make the geometry contract explicit**: size the slice from what PAPPL
   guarantees, and fail a job whose document header disagrees with the options
   header — a specific error and log line, never a clamp. Regression test: a
   600 dpi document into a 1200x600 job must end `job-state=aborted`.
3. **A transport harness** (`scripts/transport-probe.py`, in the shape of
   `p5-probe.py`): loopback TCP sink, printer added over `socket://`, and the
   received bytes compared **byte for byte** with the same job run to
   `file://`. It only counts once it has been shown to go red — truncate the
   sink's stream and watch it fail.
4. **USB**, as far as it goes without hardware: a `devices` listing check,
   `papplDeviceIsSupported` on a `usb://` URI, and the permission story written
   down — access to `/dev/bus/usb` is a packaging matter, not a code one.
5. **Q-15's state-file isolation**, so a probe printer cannot come back under
   the SPL2 driver.
6. **Q-16's format string and device ID**, so the printer stops advertising the
   geometry probe's MIME type.
7. **Q-17 recorded as a decision** — no `autoadd_cb`, matching the README.

Two smaller items found alongside, neither blocking:

* `spl2-core` still emits Turkish diagnostics, and they now surface in PAPPL's
  job log ("Hesaplanan bant genişliği (1240 B) ..."). Q-11's known-deviation
  list named only `src/golden.rs` and `goldens/README.md`; it is wider.
* PAPPL 1.3.1's own log lines drop the last character of formatted numbers —
  "Device write metrics: 4545 bytes" for 45453 bytes actually written,
  "60x60dpi" for `600x600dpi` as stored in the state file, "3276 clients" for
  32768. Our own log lines are formatted in Rust and are unaffected. Worth
  confirming in the source before it is reported anywhere, and worth knowing
  before anyone debugs from those numbers.

Then P9's raster-type and dithering review — the probe measured BLACK_1/PWG
only, and nothing characterises what PAPPL's PNG or JPEG conversion produces.
Keep the original filter in-tree until P11 passes; keep the PPD permanently.
Hardware G-1 and open questions Q-12 to Q-17 remain outstanding. No hardware
print was performed.

Checks: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`, golden checksums, and all three `p5-probe.py` modes
(default, `--spl`, `--device-failure`).

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
| P6 | `spl2-core` extraction and the SPL2 raster callbacks | — |
| P7 | Device transport and the job-geometry contract (Q-10, Q-14 to Q-17) | — |
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
