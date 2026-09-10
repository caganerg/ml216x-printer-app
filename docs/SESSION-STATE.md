# Session State — PAPPL migration

*Updated 2026-09-07.*

**The application now runs entirely in user space (2.0.0~alpha-4,
decision Q-18).** The package installs a systemd *user* unit, enables it with
`systemctl --global enable`, and installs no system unit: no root process, no
setuid binary, state in each user's `$XDG_CONFIG_HOME`, spool in a 0700
`RuntimeDirectory` under `$XDG_RUNTIME_DIR`, and USB access through a
`uaccess` udev tag on the hardware-confirmed `04e8:330f` device rather than
through privilege. This closes S-3 in `docs/SECURITY-REVIEW.md`.

It also raised and then settled **Q-19**: libpappl creates the control socket
mode 0777, so in `/tmp` any local user could drive the server — true of the old
root service too, and worse there. The maintainer delegated the decision, and
candidate (c) is implemented: `crates/ml216x-printer-app/src/runtime.rs` points
`TMPDIR` at `$XDG_RUNTIME_DIR` when the caller has set none and that directory
is private, so the socket lands somewhere only its owner can reach. An explicit
`TMPDIR` still wins — the probe scripts rely on it — and a context with neither
variable gets a warning rather than a silent exposure. S-4 is contained, not
fixed: the socket's mode is still libpappl's, and the upstream half belongs
with the S-1/S-2 bug report.

The packaging change itself is **not yet installed or tested on a machine**;
`sh -n`, a package build, and running the binary by hand are all that has been
done.

**The Debian package is now `ml216x-printer-app` (2.0.0~alpha-5)**, renamed
with the repository, which moved to `github.com/caganerg/ml216x-printer-app`.
Only packaging metadata changed: the package name, its documentation directory,
the homepage and the `Documentation=` URLs. It declares
Conflicts/Replaces/Provides on `samsung-ml2160-rust` because both install
`/usr/bin/ml216x-printer-app`, so apt removes the old package — which is also
what stops and disables the root service of alpha-3 and earlier. `postinst`
keeps a backstop for a *dangling* enablement symlink, and only a dangling one:
a system unit someone installed by hand resolves to a real file and is left
alone. The five branches of that condition were exercised in a scratch
directory. The Rust crate names followed in a
separate change: the root crate is now `rastertospl-rust`, after the binary it
produces, because `ml216x-printer-app` was already taken by the application
crate.

One thing dpkg settled by rejecting it: `#` comment lines are not allowed in a
binary package's control file (`dpkg-deb: error: parsing file … near line 12:
field name '#' must be followed by colon`). Reasoning that would sit next to
those fields lives in the changelog and in `scripts/build-deb.sh` instead.

**P6 is implemented: `spl2-core` is extracted and the SPL2 callbacks are
connected.** The printer application now emits real SPL2/QPDL through PAPPL.
The maintainer reports successful hardware printing and CUPS network sharing,
but USB reconnect / power cycling creates a duplicate desktop queue. Removing
that extra queue restores use of the existing IPP queue without reinstalling. See
[hardware test notes](HARDWARE-TESTING.md). Release gate G-1 is still open per
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
* **USB hardware feedback is now available.** The maintainer reports working
  printing on their system, with a duplicate desktop queue after reconnect or
  power cycling; deleting that extra queue restores use of the existing IPP queue.
  No Samsung device is attached to the development host; model, USB IDs and
  before/after queue details are still needed. See `docs/HARDWARE-TESTING.md`.
* **The state file is PAPPL's, not ours.** The mainloop persists printers to
  `$XDG_CONFIG_HOME/ml216x-printer-app.state` and reloads them at startup
  without this repository calling either state function. A manual run that does
  not scope `XDG_CONFIG_HOME` writes into the user's real `~/.config`; only
  `scripts/p5-probe.py` scopes it today.
* **Nothing in this project's code needs root** (measured 2026-09-06, and the
  basis of decision Q-18). Run as uid 1000, the server binds the loopback port
  — verified with `ss -ltnp`, because libpappl's own log line prints the port
  with its last digit missing — puts its control socket at
  `$TMPDIR/ml216x-printer-app<uid>.sock`, its state at
  `$XDG_CONFIG_HOME/ml216x-printer-app.state`, and answers `devices`, `add` and
  `status` with no `sudo`. The root requirement was entirely in the packaging.

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

## P12 so far — the measurement, prepared but not taken

Everything P12 can do without a printer is done, and it is collected in
[`docs/G1-MEASUREMENT.md`](G1-MEASUREMENT.md): the runbook, the record form
per model, and what each measured outcome means.

* **`scripts/g1-page.c`** generates the measurement page as full-media PWG
  Raster: four corner brackets on the predicted printable-area corners, four
  numbered millimetre rulers (one per paper edge, out to 25 mm), a 100 mm
  calibration cross, and a caption naming the medium, resolution and margin.
  Ticks are placed at whole millimetres of *predicted physical distance from
  the paper edge*, so a rule laid with its zero on the edge reads the error
  off directly. It replaces `scripts/pwg-probe-input.c` for this purpose;
  that generator's single-pixel marks were right for P5 and are 42 µm across.
  The generator also *declares* every position it drew, in JSON, and the
  harness re-derives none of them.
* **`crates/spl2-core/examples/qpdl-decode.rs`** reads an SPL2 stream back
  into page bitmaps using the engine's own `Algo0x11::decompress` — behind
  `golden-replay`, for the reason Q-6 gives. Checked against
  `goldens/a4-600-marks.spl`, where it recovers exactly the two right-hand
  corner marks the corpus documents, the left pair having been dropped.
* **`scripts/g1-probe.py`** prints the page through the real printer
  application to a `file://` device, decodes it, and asserts that every tick,
  bracket and span landed on the pixel the driver's geometry predicts.
  `--all` sweeps every medium and resolution the application publishes,
  asking it over a hand-built IPP request rather than a second copy of the
  capability table. **44 of 44 cases pass** (11 media × 4 resolutions), and
  the harness has been shown to fail three ways: `--inject shift` (R-1
  displacement), `--inject crop` (Q-13's vertical rule) and `--inject scale`
  (R-4). It also asserts the precondition the whole mapping rests on —
  `band_placement`'s centring term being zero — instead of assuming it.

What is left is physical and cannot be done here: no Samsung device is
attached, so the sheet has not been printed or measured. **G-1 is still
open, per model.**

Two things the preparation turned up, both written up as open questions
rather than acted on:

* **Q-21** — `hard_margin_bytes` rounds the margin up to a whole byte column,
  so sheet content is placed 0.33 mm left of its nominal position at 600 dpi.
  The 1.x filter does the same, which is why no golden and no test can see it;
  the P6 equivalence test asserts the two front ends agree with each other,
  not with the sheet. Expectation: keep it.
* **Q-20** — G-1's text asks for a page printed "through the 1.x path", which
  the 2.0 package no longer installs. The runbook measures the shipping path
  and says so; the gate's wording is untouched.

One tooling note worth not rediscovering: **`ipptool -t` fails every
`Get-Printer-Attributes` against this printer**, reporting
`document-format-supported`'s `application/octet-stream` as having "bad
characters (RFC 8011 section 5.1.10)". The report is wrong and it is not this
driver's bug — the server answers `successful-ok`, the value on the wire is 24
clean bytes, and libcups' own `ippValidateAttributes` accepts the whole
response, all three checked on 2026-09-07 against CUPS 2.4.10-3+deb13u2.
`Print-Job` is unaffected, which is why every probe in `scripts/` uses it.

## Next work — the rest of P7

1. **USB.** Everything that can be established without a printer has been:
   `ml216x-printer-app devices` runs and lists nothing on this machine, which
   agrees with `docs/GOLDEN-VALIDATION.md` §4 — no Samsung device is attached,
   and `/dev/bus/usb` holds no node this user could open anyway. What is left
   needs hardware: diagnosing duplicate desktop queue creation, recording the
   successful setup, and collecting the IEEE-1284 device ID.

   **Corrected 2026-09-06:** this item used to say the udev rules could not be
   written because no real vendor and product IDs were available. They are
   available — the maintainer supplied `04e8:330f` — and two device-scoped
   rules now ship against it: the duplicate-queue suppression of alpha-3 and
   the `uaccess` tag of alpha-4. What is still missing is not the IDs but the
   acceptance run on the maintainer's machine, and the IEEE-1284 device ID,
   which the PPD does not carry either (it records
   `MFG:Samsung;MDL:ML-2160 Series;CMD:SPL,FWV,EXT;` and nothing about USB).
   Socket transport, by contrast, is proven end to end.
2. ~~**Packaging for the printer application**~~ — **done**, and the USB
   permission story is now answered rather than pending: the maintainer
   supplied `04e8:330f`, so the packaged udev rule tags that one device
   `uaccess` and the logged-in user can open its `/dev/bus/usb` node without
   being root and without group `lp` (Q-18). It needs the same hardware
   acceptance run as the duplicate-queue rule. The `.deb`
   installs `/usr/bin/ml216x-printer-app` and a systemd unit and ships neither
   filter nor PPD (Q-5's clean break for what we ship); dependencies are stated
   explicitly, because the hand-built package path never substitutes
   `${shlibs:Depends}` — the old control would have shipped that literal string
   to users. `scripts/build-deb.sh` builds it and checks the libpappl range
   first. The service no longer runs as root at all; the unit that
   did is kept in the tree, marked not installed, as the record. The maintainer
   scripts pass `sh -n` but have not been executed: doing so installs a service
   on the development machine.
3. ~~**P9's raster-type and dithering review**~~ — **done.** `BLACK_1` stays
   (the engine is 1-bit only), and the dithering path is reachable rather than
   avoidable: forcing `BLACK_1` selects it. The review confirmed the two
   unpatched libpappl overflows the Q-1 follow-up flagged and reproduced both
   to a server crash — an 8-bit raster wider than the page, and an oversized
   `media-ready` list. Neither is fixable in this tree; the loopback-only bind
   limits network exposure; a demonstrated crash does not bound their impact. See `docs/SECURITY-REVIEW.md`, the P9 entry in
   `docs/DECISIONS.md`, and `scripts/security-probe.py`. The one action left is
   filing the Debian bug, which needs a bug-tracker submission (maintainer).

The diagnostic and commentary translation pass landed in `d3f6677`, and the
comment pass that finished it — the whole test module of `src/main.rs` — landed
after it; Q-11's known deviation is closed, with the three deliberate Turkish
fragments listed in that decision entry.
The default build now selects the Printer Application; the legacy filter is
an explicitly selected reference package. README installation and option
examples use the IPP application. Q-12's stale margin gate and sidecar metadata
are corrected without changing SPL output.

**The log-truncation question, settled by measurement on 2026-09-06.** This
paragraph has now said both things, so here is the evidence rather than a third
opinion. An early note claimed PAPPL's log lines drop the last character of
formatted numbers; a later correction said that was a capture artifact and the
format strings were fine. Both halves were half right.

The format strings *are* correct. `libpappl.so.1` contains, verbatim:

```
Starting log, system up %ld second(s), %d printer(s), listening for connections on '%s:%d' from up to %d clients.
```

What comes out of it does not match. The server was started three times, at
`--listen-port 8639`, `18631` and `12345`, and logged `debian.local:863`,
`debian.local:1863` and `debian.local:1234`; the constant client limit printed
as `3276` (32767) every time, and the two zero-valued counts printed as nothing
at all — `system up  second(s),  printer(s)`. Every numeric conversion loses
its last character. String conversions do not: the hostname above is intact,
and so is the full socket path in `Listening for connections on '%s'`.

The bind itself is correct, which is the part that matters here: with the log
saying `1863`, `ss -ltnp` showed the process listening on `127.0.0.1:18631`.
So this is a display defect in libpappl's own log writer — it costs nothing but
the readability of a diagnostic, and the mechanism was not located from
outside the library. **Never read a number out of a PAPPL log line without
confirming it another way**; that is how the "the resolution is wrong" scare
started. If the S-1/S-2 report to Debian is filed, this is worth a sentence in
it, but it is cosmetic and is the maintainer's call whether to include.

Keep the original filter in-tree until P11 passes; keep the PPD permanently.
Hardware G-1 and open question Q-13 remain outstanding; Q-12 and Q-14 to Q-17
are decided and implemented. Hardware printing and CUPS sharing are now
reported working by the maintainer; a device-scoped udev fix for confirmed USB ID `04e8:330f` is packaged in
alpha-3; its reconnect/power-cycle hardware acceptance test remains open.

Checks: **`./scripts/run-checks.sh`**, which is the whole list in one place and
is what CI runs — `fmt`, `clippy` with `-D warnings`, `cargo test --workspace`,
the `golden-replay` feature both ways, the golden checksums, all three
`p5-probe.py` modes with the default mode's output diffed against
`docs/P5-MEASUREMENTS.json`, `transport-probe.py` plain and with both
injections, `g1-probe.py --all` plus its three injections, `security-probe.py`,
and the `.deb` build. `--list` explains the groups, `--self-test` proves the
runner still stops at the first failure. Every probe scopes `XDG_CONFIG_HOME`
and `TMPDIR` to a temporary directory; run them no other way, or a probe run
leaves printers in the user's own PAPPL state.

## CI, added 2026-09-10

Until this point there was no CI at all, while `CONTRIBUTING.md` and
`docs/GOLDEN-VALIDATION.md` both spoke of "keeping the corpus in CI" and of CI
building `golden-replay` both ways. The gap had already cost something: three
clippy lints that did not exist when the engine was written turn the documented
`-D warnings` run red on rustc 1.95, and that was found by typing the command,
not by a machine. The lints are fixed in `fb7aab7` with no output bytes moved.

`scripts/run-checks.sh` now holds the check list, and
`.github/workflows/checks.yml` runs that same script in a `debian:trixie`
container — trixie's packaged rustc 1.85.0 against libpappl 1.3.1-2.1+b2, the
combination decision Q-1/D-1 targets. Three jobs gate (`build-and-test`,
`harnesses`, `package`); two deliberately do not (`security-signal`, because
its failure most likely means libpappl was *fixed*, and `future-toolchain`,
because a newer stable's new lint is not a regression in this tree). It also
runs weekly, since both of those signals arrive without a commit. Full local
run from an empty `target/`: 2 m 52 s, all eight groups green.

Two things the work turned up:

* **The runner shipped with a defect that a mutation caught.** Calling each
  group as `run_$g || fail $g` loses `set -e` inside the function body, so a
  failure in the middle of a group was ignored. A mutated
  `docs/P5-MEASUREMENTS.json` passed the run. Groups are now called plainly and
  `--self-test` exercises the loop with a group that fails in the middle; the
  self-test itself was shown red against the old runner and green against the
  new one. `docs/CI.md` records the whole sequence.
* **Q-22 is open.** Three crates declare `rust-version = "1.77"` and nothing
  has ever compiled against it. Written up in `docs/DECISIONS.md` with
  resolutions and my expectation (raise it to the archive's 1.85), not acted
  on.

What CI cannot cover is listed in [`docs/CI.md`](CI.md); the short list is
G-1's physical measurement, everything USB, executing the maintainer scripts,
and installing or upgrading the `.deb`.

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
[`docs/MARGINS.md`](MARGINS.md), `docs/DECISIONS.md` and — before touching
anything about margins — [`docs/G1-MEASUREMENT.md`](G1-MEASUREMENT.md), then
`docs/GOLDEN-VALIDATION.md` and `CONTRIBUTING.md` for how output is verified
and what blessing a golden requires, then `docs/MIGRATION-PLAN.md` §7 and §9
for the target layout and the six corruption risks. The byte-for-byte
behaviour itself lives in `crates/spl2-core/src/geometry.rs` around `compute_page_width_pixels`,
`hard_margin_bytes` and `band_placement`, and in `crates/spl2-core/src/qpdl.rs` around
`begin_job`, `begin_page`, `write_compressed_band`, `end_page` and `end_job`.
The `v1.x-final` recovery anchor is an annotated tag: object `7c0cf2c`,
commit `33d4ff2`, both present on `origin`. **It is now the only ref that names
the 1.x line.** The `legacy/cups-filter-1.x` branch was deleted on 2026-09-06,
locally and on `origin`, because the project supports the 2.0 line only. That
was checked before it was done rather than assumed: the branch tip `1ebcafa`
was an ancestor of both the tag and `main`, and `git rev-list --count
main..legacy/cups-filter-1.x` was 0, so no commit existed only there. Deleting
the branch is not P11 and does not touch the 1.x filter, which stays in the
tree as the root crate until that gate passes (decision Q-5).
