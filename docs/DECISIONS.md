# Decision Log — PAPPL Migration

Decisions taken for the 2.0 line (CUPS filter → PAPPL Printer Application).
Each entry records the question, the decision, and the reasoning, so the state
can be reconstructed from the tree alone.

Questions are numbered as they were raised in `docs/MIGRATION-PLAN.md`.

**On the numbering.** Eleven questions were raised, Q-1 to Q-11, and none was
skipped. There are twelve entries below because Q-8 asked two unrelated things
in one paragraph — the licence for the FFI crates and the missing licence
header on the PPD — and was split into **Q-8a** and **Q-8b** when it was
answered. **Q-7 exists and is decided:** it asked whether the .deb should link
libpappl statically or dynamically, and it is answered under Q-7 below (and
folded into Q-1, which settled the same matter). Counting the decided entries
as ten and treating Q-7 as unaccounted for is the arithmetic slip this note
exists to prevent. **Q-12** and **Q-13** were added later, by review and by
implementation rather than by the migration plan, and are the only entries
outside the Q-1..Q-11 range.

---

## 2026-09-06 — Q-14 (OPEN): a normal-quality job runs at a resolution the document was never rendered at

Raised while analysing the state of the tree before P7, by driving the built
application over a loopback socket. It is the most serious thing found so far
on the PAPPL path, and it is the realisation of P5 finding 3 — "request
matching IPP and raster resolutions, PAPPL can otherwise construct a different
output header and pad/crop the incoming data".

**What was measured.** `scripts/pwg-probe-input.c` generated one A4 page at
600x600 dpi (4960 px wide, 620 bytes per line). It was submitted with
`ipptool` to a printer added at `socket://127.0.0.1:PORT`, with no
`printer-resolution` in the request. The job **completed successfully** and
45453 bytes of QPDL reached the socket. The driver's own log line for that job
reads `cupsWidth=9921, pageWidthPx=9920, bandWidthPx=9920, bandWidthB=1240,
hardMarginB=27` — that is A4 at **1200x600 dpi**, not the 600x600 the document
was rendered at. Repeating the same submission with
`ATTR resolution printer-resolution 600dpi` produced `cupsWidth=4960,
bandWidthB=620, hardMarginB=14` and 28759 bytes, which is correct.

So the printer's declared default (`x_default`/`y_default` = 600) is **not**
what an ordinary job gets. The hypothesis that fits the numbers is that PAPPL
picks a resolution from the list by print-quality — `normal` selecting the
middle entry of `[(300,300), (600,600), (1200,600), (1200,1200)]`, which is
`(1200,600)`. That hypothesis has not been checked against PAPPL's source and
must not be relied on until it is.

**Why this is more than a wrong page size.** `crates/pappl/src/application.rs`
builds the scanline slice as
`std::slice::from_raw_parts(line, raw.header.cupsBytesPerLine)` — 1241 bytes
in the measured job — from the **options** header. If PAPPL sizes the buffer it
passes to `rwriteline_cb` from the **document's** header (620 bytes here),
every scanline is a 621-byte out-of-bounds read reachable from an ordinary
print job. Which header sizes that buffer is not decidable from the installed
headers; `pappl/job-process.c` from `pappl 1.3.1-2.1` decides it. Until that
is read, this is an undetermined ABI/lifetime question and is treated as a real
exposure, not as "probably fine".

Candidate resolutions:

- **(a) Read `pappl/job-process.c` first, then fail the job on any mismatch.**
  Establish from the source how the line buffer is sized and whether PAPPL
  scales, pads or crops raster input; size the slice from whatever PAPPL
  actually guarantees; and refuse a job whose document header disagrees with
  the options header, with a specific error and log line, never a clamp.
- **(b) Honour the document header** and re-derive the geometry from it,
  ignoring the options header for raster jobs.
- **(c) Constrain what PAPPL can choose** — declare fewer resolutions, or map
  quality to resolution ourselves — so the options header cannot disagree.

**Expectation: (a) as the immediate step, and it is a prerequisite for
everything else in P7.** (b) may well be the right end state, but it cannot be
chosen before the source says what the buffer is; (c) alone hides the mismatch
instead of refusing it, and a page that looks fine until measured is worse than
a refused job. Note that failing every job that does not pin its resolution
would make the printer useless to ordinary clients, so (a) has to be paired
with whichever of (b) or (c) makes the common case correct — that pairing is
the decision this question asks for.

`apt-get source pappl` needs a network fetch from `deb.debian.org`, which is
why it is proposed here rather than already done.

---

## 2026-09-06 — Q-15 (OPEN): the persisted state file carries printers across driver modes

Raised in the same session. PAPPL's mainloop persists the system to
`$XDG_CONFIG_HOME/ml216x-printer-app.state` and reloads it at startup on its
own; nothing in this repository calls `papplSystemLoadState` or
`papplSystemSaveState`. That was observed directly: a `probe` printer created
by an earlier `--probe --probe-output` session was still in the user's state
file and was **re-created at startup by a plain `server` run**, which attaches
`Spl2Driver` rather than `GeometryProbe`.

The `--probe` guard is therefore not durable. `driver_cb` refuses a non-`file://`
URI while `app.probe` is set, but the two drivers share one driver name
(`samsung_ml216x`) and the state file records only that name, so a printer
created under one driver is silently re-created under the other. The
fail-closed direction (a socket printer reloaded in probe mode) is caught by
the URI guard; the fail-open direction — a printer named `probe`, reloaded
under the real SPL2 driver, pointed at whatever URI it was created with — is
not caught by anything.

Candidate resolutions:

- **(a) Give probe mode its own state file and its own driver name**, so the
  two modes cannot see each other's printers at all.
- **(b) Refuse to reload printers created in the other mode**, by recording the
  mode in the driver name or the device ID and rejecting a mismatch at load.
- **(c) Disable state persistence in probe mode** entirely, so a probe printer
  never outlives its run.

**Expectation: (a).** It is the only one of the three where the isolation does
not depend on a check being reached, and it costs one extra CLI flag. Whatever
is chosen, `scripts/p5-probe.py` already scopes `XDG_CONFIG_HOME` to a
temporary directory, and any manual run must do the same — a plain
`ml216x-printer-app server` writes into the user's real `~/.config`.

---

## 2026-09-06 — Q-16 (OPEN): the SPL2 driver advertises the geometry probe's format

Raised in the same session. `driver_cb` sets
`data.format = "application/x-pappl-geometry-probe"` unconditionally, so the
SPL2 driver inherits the P5 instrument's MIME type. It is not internal: PAPPL
publishes it, and the persisted state shows the printer's IEEE-1284 device ID
as `MFG:Samsung;MDL:ML-216x (P5 development);CMD:PWGRaster,URF,application/x-pappl-geometry-probe,JPEG,PNG;`,
while the server log says "Driver supports raw printing of
'application/x-pappl-geometry-probe' files". `printfile_cb` rejects such jobs,
so nothing is mis-printed today; what is wrong is what the printer claims to be.

The PPD's own answer for the classic queue is
`*1284DeviceID: "MFG:Samsung;MDL:ML-2160 Series;CMD:SPL,FWV,EXT;"`
(`ppd/samsung-ml2160.ppd:30`), which is the only device-ID evidence in the tree.

Candidate resolutions:

- **(a) Give the SPL2 driver its own format string** and leave the probe MIME
  to the probe driver; keep `printfile_cb` rejecting raw jobs, so the string is
  a label rather than an offer.
- **(b) Declare no printer-specific format at all** for the SPL2 driver, since
  raw printing is refused anyway.
- **(c) Accept raw SPL2 files** — advertise the format and implement
  `printfile_cb` as a pass-through.

**Expectation: (b) for 2.0, or (a) if a name is wanted in the device ID.** (c)
is a feature, not a fix, and it would let an unvalidated stream reach the
engine. What the `MDL` and `CMD` fields should finally say is part of the same
decision, and it is the maintainer's: the PPD says `SPL,FWV,EXT` for real
hardware, while the application currently announces itself as
"ML-216x (P5 development)".

---

## 2026-09-06 — Q-17 (OPEN): discovery, and whether the application may ever add a printer by itself

Raised in the same session, because P7 is the step where it would be
implemented if it were wanted. `papplMainloop` is called with
`autoadd_cb = None` today, and the README already takes a position on the
matter for the 1.x queue: "Pick the device URI yourself rather than letting
anything auto-detect it. CUPS device discovery is unauthenticated — over the
network (mDNS/Bonjour/SNMP) and over USB (descriptor strings) alike — so any
device can advertise itself as a 'Samsung ML-216x' and be wired up as the print
destination" (`README.md`).

Candidate resolutions:

- **(a) Never wire `autoadd_cb`.** `devices` lists what is attached, the person
  reads it and passes `-v` to `add`, exactly as the README's note asks.
- **(b) Wire `autoadd_cb` but match strictly** on the IEEE-1284 device ID
  through `papplDeviceParseID`, accepting only `MFG:Samsung` with a known `MDL`.
- **(c) Wire it unconditionally**, matching any device the driver can drive.

**Expectation: (a)**, because it is what the README already promises and
because the threat it describes is exactly what (b) and (c) reopen: a device ID
is self-reported and unauthenticated. Recording it as a decision matters
because "we never got round to it" and "we decided not to" look identical in
the code.

---

## 2026-09-06 — Q-13 (OPEN): the vertical hard margin has no precedent in the tree

Raised while connecting the SPL2 callbacks (P6). It is the one number on the
new path that could not be derived from existing code, so it is written up
rather than buried in a commit.

**What is settled.** Horizontally, nothing new was needed. `band_placement`
centres the incoming line in the sheet-wide band and then subtracts the hard
margin; when the line already spans the sheet — which is what PAPPL delivers —
the centring term is zero and the subtraction alone survives. That is exactly
the sheet-to-engine mapping the classic path performs, so the same physical
byte column lands in the same band column on both paths. A test asserts this
across all 11 media and all 4 resolutions
(`full_media_and_printable_area_place_the_sheet_identically`), and it is what
stops the "subtract the margin twice" trap `docs/MARGINS.md` warns about.

**What is not settled.** Vertically the 1.x filter subtracts nothing at all,
because cups-filters handed it the printable area already centred on the sheet
and, as `docs/MARGINS.md` records, centring and `hardMarginY` "cancel exactly".
PAPPL delivers full media, so that cancellation is gone and the printer
application has to drop the top margin itself. There is no quoted SpliX line
and no existing call site for that number, which makes it the first geometry
value on this path that is an estimate rather than a transcription.

`hard_margin_lines` currently rounds to the nearest scanline: at 600 dpi
12.5 pt is 104.17 lines, so it drops 104 and the page is cut to
`height - 2 x 104`. Candidate resolutions:

- **(a) Nearest scanline (implemented).** 104 lines lands 0.17 lines low, and
  it is also what cups-filters' own centring implies for the classic path, so
  the two front ends stay within one scanline of each other on every medium —
  asserted in points, not lines, by `cropped_height_matches_the_printable_area`.
- **(b) `ceil`, mirroring `hard_margin_bytes`.** 105 lines, which lands 0.83
  lines high. Consistent-looking, but the `ceil` in the horizontal rule exists
  to reach a byte boundary and is free there because both sides of the
  placement use it; neither reason applies to a scanline.
- **(c) Do not crop at all** and send the full sheet, letting the last 12.5 pt
  fall past the printable area. Rejected without hardware: it declares a page
  taller than the engine can print, which is the class of mismatch `D-01`/`D-02`
  exist to refuse.

**Expectation: (a)**, and the difference between (a) and (b) is 0.04 mm, far
below what G-1's ruler will resolve. The reason to record it anyway is that
being wrong about the *rule* rather than the rounding — cropping the wrong
edge, or not cropping — is a 4.4 mm error, and nothing in the tree would
catch it. **G-1 must therefore measure the vertical margin on a PAPPL-printed
page, not only the horizontal one**, and `docs/GOLDEN-VALIDATION.md`'s gate
text should say so when Q-12 is settled.

---

## 2026-09-06 — Q-12 (OPEN): release gate G-1 still names the superseded 12 pt margin

Raised by a post-commit review of `eba7b09`, not by the maintainer. The 12.5 pt
refresh marked sections 1-3 of `GOLDEN-VALIDATION.md` as historical but did not
touch section 4, which therefore still reads as current while stating that the
corpus "derives its margin from the same 12 pt `*ImageableArea` value" and that
**G-1** measures against "`*ImageableArea` (12 pt = 4.23 mm on every medium)".
The PPD now declares 12.5 pt = 4.41 mm. G-1 is the condition on shipping 2.0,
so whoever performs it would measure against a value the tree no longer uses,
and the 0.5 pt (0.18 mm) difference is inside the range that measurement exists
to resolve.

A second, smaller instance of the same drift: each sidecar's
`cups_page_header.ImagingBoundingBox` now prints fractional points
(`[12.5, 12.5, ...]`) while the binary CUPS header the case actually builds
stores the truncated integer `12`, and the `Margins` field beside it in the same
block prints `12`. `goldens/README.md` states that every sidecar records the
driver margin "separately from truncated integer CUPS fields"; the
`ImagingBoundingBox` line sits inside the CUPS-header block and is not
truncated, so the block does not describe the header bytes it documents. No SPL
output depends on it: the filter no longer reads either field.

Candidate resolutions:

- **(a) Restate section 4 and G-1 at 12.5 pt (4.41 mm)**, and print
  `ImagingBoundingBox` in the sidecar exactly as the header stores it, leaving
  the fractional value in `ppd_source` and `derived_qpdl.hard_margin_pt`.
- **(b) Mark section 4 historical** the way sections 1-3 were marked, and write
  the gate afresh in a new dated section.
- **(c) Change nothing**, treating section 4 as a record of the 12 pt corpus and
  `MARGINS.md` as the current statement of the margin.

**Expectation: (a) for the gate.** G-1 is a release condition rather than a
record, so it should name the value that will actually be on paper; (c) leaves
the one document a person reads before measuring pointing at a superseded
number. Either (a) or (b) settles the sidecar question the same way.

Related evidence gathered in the same review: R-1 (`hard_margin_bytes` returning
one byte too many) was re-injected against the **refreshed** corpus and
`golden::test_goldens_match` still fails, so the harness is still proven to go
red for the risk this change touched. `GOLDEN-VALIDATION.md`'s note that the
mutation results were not rerun remains accurate for R-2 to R-5.

---

## 2026-09-06 — Q-2 follow-up: use 12.5 pt and proceed with P5

The maintainer explicitly selected **12.5 pt on every edge**, following the
SpliX ML-2165 override, and authorized the next migration step. This supersedes
the block below requiring a measurement before implementing capabilities.
It does **not** constitute a physical measurement for ML-2160/2165/2165W/2168:
release gate G-1 remains open, per model.

- The PPD and transitional filter now use 12.5 pt. `src/media.rs` holds the
  shared driver constant. Integer CUPS `Margins[]` must not truncate it.
- IPP uses hundredths of a millimetre: 12.5 pt is represented as **441**.
  SPL band placement uses the exact points, giving **7/14/27 bytes** at
  300/600/1200 dpi.
- This is an explicitly authorized change to the frozen filter's margin
  behaviour. The 30 production golden streams and all 32 JSON sidecars were
  refreshed together; the two synthetic streams retain their historical
  injected margins. No unrelated SPL2 protocol changes were made.
- P5 is implemented as a minimal mainloop with capabilities and an explicit
  file-only geometry probe. Real SPL2 printing remains unconnected and fails
  with a clear error. See `P5-MEASUREMENTS.json` and `MARGINS.md`.
- The experiment shows full-media BLACK_1 lines and zero header margins.
  Do not feed them unadapted into the classic printable-area band placement.

---

## 2026-09-05

### The two blocking answers

These were the decisions that unblocked the migration; everything else follows
from them.

**PAPPL 1.3.1 is the target, with a `>= 1.3, < 2.0` version guard.**
Build against the `libpappl-dev` Debian trixie actually ships (1.3.1-2.1+b2).
Do not build 1.4.x from source, do not vendor libpappl, do not link it
statically. Nothing 1.4 added is needed by a monochrome raster driver — the
raster callbacks, driver data, device API and mainloop have been stable since
1.0 — while vendoring a C library earns the lintian `embedded-library` tag,
removes the package from apt security updates, and makes us the CVE response
path. Linking the archive's library gives `${shlibs:Depends}` for free.

**The hard margin is a driver constant derived from the PPD, with no zero
fallback.** It is never read from a raster header, in either the classic or the
PAPPL path; `Margins[]` arriving as zero is correct behaviour under PAPPL, not
a bug to work around. Per-media margins are derived from
`ppd/samsung-ml2160.ppd`'s `*ImageableArea` and `*PaperDimension`, held in one
committed table that cites the PPD lines it came from, declared in the PAPPL
driver data, and cross-checked at page start against that table. A mismatch, a
zero, or an absent margin **fails the job** with a specific error and a clear
log line. A page that looks fine until measured is worse than a refused job;
see the regression recorded at `src/main.rs:427`.

---

### Q-1 — Which PAPPL version to target
**Decision: Debian trixie's 1.3.1, dynamically linked. Guard `>= 1.3, < 2.0`.**

Reasoning as above. Debian has 1.3.1-2.1 in bookworm, trixie, forky *and* sid,
so there is no newer packaged version to move to; the tracker itself notes that
upstream 1.4.12 is available and unpackaged.

Consequences:
- `pappl-sys` binds only symbols present in the 1.3.1 headers. After bindings
  are generated, produce a table of every bound symbol against the PAPPL
  version that introduced it and assert none is newer than 1.3. If a 1.4-only
  symbol turns out to be genuinely necessary — stop and ask.
- `build.rs` uses pkg-config as the **primary and only** path: `libpappl-dev`
  installs `/usr/lib/x86_64-linux-gnu/pkgconfig/pappl.pc` and ships no
  `pappl-config` script.
- `packaging/debian/control` loses `cups-filters` and gains `libpappl1t64` via
  `${shlibs:Depends}`; the musl static build goes away.

#### Q-1 follow-up — the unpatched dependency
Trixie's 1.3.1 predates upstream's two 2026 overflow fixes
(`4587888f50`, dithering in `pappl/job-process.c`; `44327aaac3`, ready-media in
`pappl/printer-ipp.c`; both 2026-08-04, both two-line bounds clamps). Upstream
released them in 1.4.12 with placeholder CVE IDs (`CVE-2026-NNNNN`), so no CVE
has been published and Debian's security tracker has no pappl entry at all.

Agreed actions, in order:
1. **Read the 1.3.1 source first** and confirm the vulnerable lines are present
   before filing anything. A Debian bug asserting an unconfirmed CVE gets
   closed; one citing the two upstream commits and the corresponding lines in
   1.3.1 does not. Report findings, then file.
2. **Determine whether we are exposed to the dithering issue at all.** If the
   driver declares only 1-bit black raster and never accepts 8-bit grayscale,
   PAPPL's dithering path may be unreachable for us. Answer this as part of the
   P9 raster-type decision and record the conclusion in
   `docs/SECURITY-REVIEW.md`. If declaring only `BLACK_1` removes the exposure
   that is an argument for doing so, but it must not override what the hardware
   and the existing engine actually need.
3. **Default the systemd unit to loopback only.** Network exposure is a
   deliberate opt-in via configuration, documented in the README.
4. **Record the unpatched dependency as a known issue** in
   `docs/SECURITY-REVIEW.md` and in the README, with the Debian bug number once
   filed.

### Q-2 — Where the hard margin comes from
**Decision: a driver constant derived from the PPD; no zero fallback.**

Reasoning as above. P5 measured full-media BLACK_1/PWG scanlines on
2026-09-06; see `docs/MARGINS.md`. The selected 12.5 pt margin is preserved
separately from the integer/zero raster-header fields.

#### Q-2 follow-up — historical open question (superseded 2026-09-06)

**Historical status on 2026-09-05: blocked pending a hardware
measurement; see [`docs/MARGINS.md`](MARGINS.md) for the full trace and the
three candidate resolutions.** The margin table has not been changed.

Found on 2026-09-05 while fetching the SpliX source to settle the copyright
attribution, and recorded because it bears directly on R-1 and on which model
gate G-1 must be measured against.

SpliX's `ppd/samsung.drv.in` puts the ML-2165 (and ML-1915) in their own block
with an explicit override, `HWMargins 12.5 12.5 12.5 12.5`
(`samsung.drv.in:274`), commented "different margins than the other monochrome
printers". The ML-2160 is not in that block: it inherits
`HWMargins 10.75 15 10.75 15` from `ppd/spl2.defs:10`. Our
`ppd/samsung-ml2160.ppd` declares 12 pt on every edge, which matches
`spl2bandedjbig.defs` — a file that belongs to the banded-JBIG **colour**
printers, not to the ML-216x.

So our value matches neither model upstream describes, and upstream's ML-2160
margin is asymmetric where ours is square.

Half a point sounds negligible and is not: `hard_margin_bytes` rounds up to a
whole 8-pixel column, so at 600 dpi 10.75 pt gives 12 bytes, 12 pt gives 13 and
12.5 pt gives 14 — and at 1200 dpi 23, 25 and 27. Each step is 8 or 16 pixels,
both ≈ 0.34 mm. That is exactly the R-1 failure mode, and it would look like a
correctly printed page. At 300 dpi the 12 and 12.5 pt cases collapse to the
same 7 bytes, so the question is invisible at that resolution.

**Not changed, deliberately.** Changing the margin table changes the bytes sent
to the printer and moves every golden; and upstream's `.drv` is evidence about
the hardware, not proof — SpliX's own PPDs for this family were derived without
access to every model either. The decision this needs is the user's, informed by
a measurement. Two consequences follow now:

* **Gate G-1 must record which model was measured.** Measuring an ML-2160 says
  nothing about the ML-2165's margin, and vice versa.
* **If the two models really differ, one PPD-derived margin table cannot serve
  both.** The PAPPL driver-capability table would need per-model margins, which
  is straightforward there but impossible in the single classic PPD — an
  argument in favour of the migration, not against it.

### Q-3 — Duplex
**Decision: out of scope for 2.0, but advertise `sides-supported` explicitly as
one-sided only rather than omitting the attribute.**

IPP clients handle a present-but-limited attribute better than a missing one.
The exclusion and its reasoning are recorded in `docs/NON-GOALS.md` so it is
not re-litigated.

### Q-4 — Toner save / density
**Decision: deferred. No invented PJL values.**

Replace the hardcoded `@PJL SET DENSITY=3` with a named constant carrying a
comment citing where the value came from, so a future capture can be wired in
without archaeology. Expose no vendor option for it now. Recorded in
`docs/NON-GOALS.md` with a note on exactly what evidence would unblock it.

### Q-5 — Clean break: what we ship vs what we keep
**Decision: approved for what we SHIP, not approved for what we DELETE.**

- The 1.x filter code **stays in the tree** until P11 passes green, because the
  golden corpus can only be regenerated while it runs. Freeze it: exclude it
  from the built artifact and mark it clearly as frozen. Do not remove it.
- `ppd/samsung-ml2160.ppd` **stays permanently**. Under Q-2 it is the source of
  truth for the hard-margin table, so it is now project *data*, not a shipped
  artifact. It must not be installed by the .deb, but it must remain in the
  source package and be listed in `debian/copyright`.
- The tree is tagged `v1.x-final` before migration work so the working 1.x
  driver stays trivially recoverable.
- A concrete list of files to **stop shipping** versus **stop keeping** must be
  written out and approved separately before anything is deleted.

### Q-6 — Does `spl2-core` keep the CUPS raster parser
**Decision: keep `raster.rs`, behind a non-default Cargo feature named
`golden-replay` — not `#[cfg(test)]`.**

`#[cfg(test)]` items are compiled only for their own crate's unit tests and are
invisible to an integration test in another crate's `tests/` directory, which
is where the golden harness will live once the workspace is split. The harness
enables the feature explicitly, and CI builds both with and without it.

### Q-7 — Static or dynamic linking for the .deb
**Decision: dynamic, against the archive's `libpappl1t64`.**

**Consequence, recorded so it is not rediscovered as a surprise: the artifact
stops being portable across distributions, and the target release becomes a
hard constraint rather than a preference.**

The 1.x package is a static musl binary with no libc dependency: it runs on
any Linux with a compatible kernel, whatever the distribution. The 2.0 package
does not. It links dynamically against glibc and against `libpappl1t64`, so it
runs only where both are present at compatible versions, and `${shlibs:Depends}`
will encode exactly that.

**The .deb targets Debian 13 (trixie)**, which ships `libpappl-dev` /
`libpappl1t64` 1.3.1-2.1+b2 — the version this project is developed and tested
against. Forky and sid carry the same 1.3.1-2.1, so they are expected to work
without change. Older releases are out of scope: the runtime package name
`libpappl1t64` comes from the 64-bit `time_t` transition, so a build for a
release predating that transition would need its own dependency name and its
own verification, and is not something this package claims. Building for a
different release means rebuilding there, not copying the .deb.

**The 1.x static binary remains the only build that runs anywhere**, which is
one more reason Q-5 keeps the 1.x filter in the tree and tagged `v1.x-final`
rather than deleting it: it is the fallback for any system the 2.0 package
cannot target.

This is the right trade for a package intended for the Debian archive — the
archive builds each release against its own libraries, and `${shlibs:Depends}`
is how that is expressed — but it is a real capability loss compared with 1.x
and it is stated here deliberately.

Answered together with Q-1; it is not an open question. Statically linking
libpappl would put Apache-2.0 code inside a GPL-2.0-only binary and rest the
whole package on PAPPL's linking exception, on top of the packaging costs
listed under Q-1. The 1.x musl static build does not carry over to 2.0.

### Q-8a — Licence for the `pappl` safe wrapper
**Decision (2026-09-05): both FFI crates are licensed `Apache-2.0 OR MIT`.**
`pappl-sys` and `pappl` both carry the standard Rust dual licence;
`spl2-core` and `ml216x-printer-app` stay `GPL-2.0-only`.

The MIT arm is what makes the arrangement work: plain Apache-2.0 on either
crate would create the same internal incompatibility, because an Apache-2.0
crate linked into a GPL-2.0-only binary imposes the patent-termination and
notice terms that GPLv2 section 6 treats as "further restrictions", and
PAPPL's linking exception covers PAPPL's own code, not ours. A GPL-2.0-only
consumer — this project's binary — takes the MIT arm and the conflict
disappears; the Apache-2.0 arm preserves the "match upstream" intent for
anyone reusing the bindings elsewhere. Dual-licensing is also what the wider
Rust ecosystem expects of a `-sys` crate, so it costs nothing in reusability.

This is a practical licensing convention, not legal advice.

**The MIT arm is necessary, not merely convenient.** The obvious escape from
the incompatibility would be to relicense this repository as
`GPL-2.0-or-later`, since Apache-2.0 is compatible with GPLv3 — but that
escape is not available to us unilaterally. `src/spl.rs` is derived from
OpenPrinting SpliX, which is GPLv2-**only**, and `src/main.rs` and the PPD
carry transcribed SpliX values as well (see the SpliX stanza in
`packaging/debian/copyright`). A derived work cannot be relicensed under terms
its upstream did not offer, and SpliX offers no "or later" clause. Only the
SpliX copyright holders could grant that. So the project is GPL-2.0-only for
as long as it contains SpliX-derived code, and dual-licensing the FFI crates
is the only way to keep them linkable. If this is ever revisited, the question
to answer first is not "should we relicense" but "can we", and today the
answer is no.

**Confirmed and applied 2026-09-05, after `pappl-sys` was written.** The
alternative — making the FFI crates `GPL-2.0-only` like the rest — was
considered and rejected. Beyond the reuse argument, it sits badly with what
`pappl-sys` actually contains: its declarations are transcribed from
Apache-2.0 licensed PAPPL headers, and stamping a GPL-2.0-only notice on a
file whose substance is a transcription of someone else's Apache-2.0 header is
not a claim worth making. The dual licence keeps the MIT arm that a
GPL-2.0-only binary needs and the Apache-2.0 arm that matches where the
material came from.

Applied: `LICENSE-APACHE` and `LICENSE-MIT` at the repository root, the
per-crate split in `packaging/debian/copyright`,
`license = "Apache-2.0 OR MIT"` in `crates/pappl-sys/Cargo.toml`, and an
`SPDX-License-Identifier` header on every file of that crate — `src/lib.rs`,
`build.rs`, `probe/layout_probe.c`, `tests/layout.rs`, `tests/symbols.rs` and
the manifest itself. The `pappl` wrapper crate carries the same headers from
its first commit.

Every other file in the tree stays `GPL-2.0-only`. The SPDX-header gap on the
GPL files recorded in the audit table below is unchanged and still open: those
headers are a separate, mechanical pass over `src/*.rs` and the PPD (Q-8b),
not part of this decision.

The audit that led here — the repository's licence was checked before
anything was assigned:

| Source | States | Agrees? |
|---|---|---|
| `Cargo.toml` `license =` | `GPL-2.0-only` | yes |
| `src/spl.rs:8` header | "GPLv2 (v2 only — same as the SpliX source)" | yes |
| `packaging/debian/copyright` | `License: GPL-2` (DEP-5 short name for v2-only; the or-later form would be `GPL-2+`) | yes |
| `LICENSE` | stock GPLv2 text only | neutral |
| `src/main.rs`, `src/raster.rs`, `src/golden.rs`, `ppd/*.ppd` | **no licence header at all** | gap |
| SPDX identifiers | **none anywhere in the tree** | gap |

The three declarations that speak all agree: **`GPL-2.0-only`**. The `LICENSE`
file is the stock GPLv2 document; its "either version 2 … or (at your option)
any later version" wording appears only inside the FSF's *"How to Apply These
Terms to Your New Programs"* appendix, which is boilerplate, not a statement
about this project. The v2-only choice is also substantively forced: `spl.rs`
is derived from SpliX, which is GPLv2-only, so the project cannot be
or-later.

**Therefore Apache-2.0 for our own wrapper would create a real internal
incompatibility** — Apache-2.0's patent-termination and notice clauses are
"further restrictions" under GPLv2 §6, and PAPPL's linking exception covers
PAPPL's code, not code we write.

Recommendation: **MIT for the `pappl` wrapper.** It is GPLv2-compatible, so no
internal conflict; it is permissive, so the wrapper stays reusable, which was
the entire point of not making it GPL; and it avoids the Apache-2.0 patent
clause that causes the incompatibility. `GPL-2.0-or-later` would also link
cleanly but would defeat the reuse goal.

The wrinkle that this recommendation missed, and that the decision above
resolves: `pappl-sys` at plain Apache-2.0 had the *same* problem as the
wrapper, for the same reason — it too would be linked into the GPL-2.0-only
binary. The earlier approval of "pappl-sys stays Apache-2.0" was taken before
that was noticed and is superseded; both crates are now `Apache-2.0 OR MIT`.

### Q-8b — Missing licence header on the PPD
**Decision: add a GPL-2 header to `ppd/samsung-ml2160.ppd`.**

More important under Q-5, not less: the PPD is now permanent project data and
the source of truth for the hard-margin table.

### Q-9 — Golden-file harness
**Decision: approved, with JSON sidecars and a margin-specific corpus case.**

Each golden `.spl` gets a sidecar recording the classic CUPS page-header values
that produced it (at minimum `Margins[]`, `ImagingBoundingBox`, `cupsWidth`,
`cupsHeight`, `cupsBytesPerLine`, `HWResolution`, `cupsBitsPerPixel`,
`cupsColorSpace`, `PageSize`, media name). These sidecars are the reference the
PAPPL-side option mapping is validated against and can only be captured while
the 1.x code runs. The corpus includes a page with 1-pixel registration marks
at the exact printable-area corners, on A4 and Letter, at every supported
resolution, so a margin regression fails at byte level rather than at ruler
level.

**Status: implemented.** See `src/golden.rs`, `goldens/`, and
`goldens/README.md`.

### Q-10 — Device transport and discovery
**Decision: USB first, socket second. Do not defer socket past 2.0 without
asking.**

Socket support is largely built into PAPPL, so once USB works the incremental
cost should be small; if it turns out not to be, raise it before doing the
work. The PPD advertises ML-2165W, and a wireless-only user gets nothing from a
USB-only release.

### Q-11 — Language for new code
**Decision: English for all code, comments, identifiers, commit messages,
documentation and packaging metadata.**

This is a GPL open-source project and contributors will not read Turkish. Any
Turkish user-facing text would be a separate translation layer added
deliberately later, never inlined into source.

**Known deviation:** `src/golden.rs` and `goldens/README.md` were written in
Turkish before this decision was taken and need converting.
