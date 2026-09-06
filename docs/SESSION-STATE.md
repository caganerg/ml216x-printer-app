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

## P7 so far — the job-geometry contract, closed

Q-14 to Q-17 were raised, then decided by delegation and implemented in the
same session; `docs/DECISIONS.md` carries the reasoning and what the PAPPL
source actually said. In short:

* **Q-14.** With no `printer-resolution` in the request, PAPPL selects by
  print-quality from the *position* of the entry in the driver's resolution
  list and never reads `x_default`/`y_default`, so normal quality — every
  ordinary job — ran at 1200x600 while the document was rendered at 600x600.
  The list is reordered so the middle entry is the default, and
  `Application::run` refuses to start if that stops being true. A 600 dpi
  document with no requested resolution now yields a stream byte identical to
  the pinned-resolution one.
* **The suspected out-of-bounds read does not exist.** `job-process.c`
  allocates the line buffer at the larger of the two headers'
  `cupsBytesPerLine` and pre-fills the padding, so the slice built from the
  options header is always inside the allocation. Equally, a mismatch **cannot**
  be detected from inside the driver: for 1-bit output PAPPL never adopts the
  document's header, pads short lines with white and appends blank ones.
  Refusing such a job is not implementable in 1.3.1; ordering the list is what
  prevents it.
* **Q-15.** A probe run now persists nothing (`papplSystemSetSaveCallback` with
  a callback that writes nowhere), and the two drivers no longer share a name.
* **Q-16.** The SPL2 driver's format is `application/octet-stream`; declaring
  none crashes the server on the first raster job, which is recorded with the
  source line that does it. The device ID is now
  `MFG:Samsung;MDL:ML-216x Series;CMD:PWGRaster,URF,JPEG,PNG;`.
* **Q-17.** `autoadd_cb` stays null, deliberately and now visibly.

`scripts/transport-probe.py` is the new harness: it prints the same job to a
`file://` destination and to a loopback `socket://` device and requires the two
streams to be byte identical, and it checks each print-quality against PAPPL's
resolution rule. It has been shown to go red three ways — a truncated socket
stream, a single flipped bit, and the old resolution order.

## Next work — the rest of P7

1. **USB.** Everything that can be established without a printer has been:
   `ml216x-printer-app devices` runs and lists nothing on this machine, which
   agrees with `docs/GOLDEN-VALIDATION.md` §4 — no Samsung device is attached,
   and `/dev/bus/usb` holds no node this user could open anyway. What is left
   genuinely needs hardware: opening a `usb://` URI, and the IEEE-1284 device
   ID the printer reports. **The udev rule cannot be written yet**, because a
   rule needs the real vendor and product IDs and the only ones available now
   would be recalled rather than read off a device; the PPD records
   `MFG:Samsung;MDL:ML-2160 Series;CMD:SPL,FWV,EXT;` and says nothing about
   USB IDs. Take them from `lsusb` at bring-up (P12) and write the rule then.
   Socket transport, by contrast, is proven end to end.
2. ~~**Packaging for the printer application**~~ — **done**, except the USB
   permission story, which waits on the same missing IDs as item 1. The `.deb`
   installs `/usr/bin/ml216x-printer-app` and a systemd unit and ships neither
   filter nor PPD (Q-5's clean break for what we ship); dependencies are stated
   explicitly, because the hand-built package path never substitutes
   `${shlibs:Depends}` — the old control would have shipped that literal string
   to users. `scripts/build-deb.sh` builds it and checks the libpappl range
   first. The service runs as root for now, with the reason written in the unit
   file. The maintainer scripts pass `sh -n` but have not been executed: doing
   so installs a system service on the development machine.
3. **P9's raster-type and dithering review.** Note what the source reading
   turned up for it: with `force_raster_type = BLACK_1`, PAPPL selects a dither
   matrix by quality and content, and `image/jpeg` and `image/png` reach the
   driver through PAPPL's own filters — so the dithering path is *not*
   unreachable for us, and P9 has to characterise it rather than assume 1-bit
   input everywhere.

Two smaller items found alongside, neither blocking:

* ~~`spl2-core` still emits Turkish diagnostics~~ — **done.** Every diagnostic
  string in `spl2-core` is English, and the goldens are byte identical across
  the change. The Turkish **comments** in `src/main.rs`, `src/golden.rs`, the
  four `spl2-core` modules and `goldens/README.md` remain, and are left to a
  dedicated pass.
* PAPPL 1.3.1's own log lines drop the last character of formatted numbers —
  "Device write metrics: 4545 bytes" for 45453 bytes actually written,
  "60x60dpi" for `600x600dpi` as stored in the state file, "3276 clients" for
  32768. Our own log lines are formatted in Rust and are unaffected. Worth
  confirming in the source before it is reported anywhere, and worth knowing
  before anyone debugs from those numbers.

Keep the original filter in-tree until P11 passes; keep the PPD permanently.
Hardware G-1 and open questions Q-12 and Q-13 remain outstanding; Q-14 to Q-17
are decided and implemented. No hardware print was performed.

Checks: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`, golden checksums, all three `p5-probe.py` modes
(default, `--spl`, `--device-failure`), and `transport-probe.py` both plain and
with `--inject truncate` / `--inject flip`. Every script scopes
`XDG_CONFIG_HOME` to a temporary directory; run them no other way, or a probe
run leaves printers in the user's own PAPPL state.

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
