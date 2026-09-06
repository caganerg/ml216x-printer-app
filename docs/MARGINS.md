# Hard Margins — 12.5 pt selected; hardware validation remains open

## Current decision and P5 experiment — 2026-09-06

The maintainer selected **12.5 pt on all edges** and authorized P5. The PPD,
transitional filter and PAPPL capabilities have been updated. This follows
SpliX's **ML-2165** setting; it is not a claim that the ML-2160 setting below
was also 12.5 pt, and it does not satisfy hardware gate G-1.

`src/media.rs` preserves fractional points. PAPPL/IPP publishes **441** in
0.01 mm units (12.5 pt = 440.9722...). The encoder's byte-aligned margin is
**7, 14, 27 bytes** at 300, 600 and 1200 dpi. The header's integer `Margins[]`
is no longer the driver's source of truth.

### What PAPPL actually supplies

Measured with the installed **PAPPL 1.3.1 / CUPS 2.4.10**, using real
`cupsRasterInitPWGHeader` / `cupsRasterWriteHeader2` PWG streams, `ipptool`
Print-Job, and the Rust callbacks. All **17 cases passed**: A4 and Letter at
all four supported resolution pairs, and the other nine media at 600 dpi.
The script checks job-state=completed, every scanline index, four sheet-edge
marks and four inset marks. Raw evidence: [`P5-MEASUREMENTS.json`](P5-MEASUREMENTS.json).

| Medium | DPI | Callback width × height | Bytes/line |
|---|---|---|---|
| A4 | 300×300 | 2480 × 3507 | 310 |
| A4 | 600×600 | 4960 × 7015 | 620 |
| A4 | 1200×600 | 9921 × 7015 | 1241 |
| A4 | 1200×1200 | 9921 × 14031 | 1241 |
| Letter | 300×300 | 2550 × 3300 | 319 |
| Letter | 600×600 | 5100 × 6600 | 638 |
| Letter | 1200×600 | 10200 × 6600 | 1275 |
| Letter | 1200×1200 | 10200 × 13200 | 1275 |

**Conclusion for this tested BLACK_1/PWG path: full media, not printable area.**
`Margins[]` is `[0,0]`; media-col margins remain 441. Sheet-edge marks arrive
unchanged, so PAPPL has not cropped away the hard margins. This does not yet
characterize PNG/JPEG conversion or other raster types (P9).

**Resolved in P6 (2026-09-06).** The encoder adapter now does this, and how it
does it matters more than that it does:

- *Horizontally, nothing new was needed.* `band_placement` centres the incoming
  line in the sheet-wide band and then subtracts the hard margin. When the line
  already spans the sheet, the centring term is zero and only the subtraction
  survives — which is the same sheet-to-engine mapping the classic path
  performs. So the guard against subtracting twice is to feed the real line
  width, not to special-case the PWG path. A test asserts the two paths place
  the sheet identically across all 11 media and all 4 resolutions.
- *Vertically there was no precedent*, because the classic path's centring and
  `hardMarginY` cancelled. The driver drops the top margin and cuts the page to
  the printable height, rounding to the nearest scanline. See **Q-13** in
  `docs/DECISIONS.md`: this is the one geometry value on the path that is an
  estimate rather than a transcription, and **G-1 must measure the vertical
  margin as well as the horizontal one.**

Consequences for the next encoder adapter:

- Distinguish PWG full-sheet input from the classic PPD printable-area input;
  do not subtract the hard margin twice or reuse the centring calculation blindly.
- Use canonical PWG millimetre dimensions when validating IPP options. For
  example A4 @600 is 7015 lines here, versus a 7017-line physical height derived
  from the legacy rounded 842 pt. Preserve the legacy QPDL contract deliberately.
- Request matching IPP and input raster resolutions. In a discovery run with
  no explicit resolution, PAPPL selected 1200×600 for a 600×600 input and padded
  the row to 1241 bytes. Internally consistent options alone do not prove that
  client raster geometry matches them. P9 must address this before real printing.
- Callback page numbering was **1-based** in these runs, despite the installed
  guide describing it as starting at 0. Use measured/source-confirmed behaviour.
- PAPPL 1.3.1 ignores `rwriteline_cb`'s boolean return in `job-process.c`.
  The wrapper retains a failure marker and returns false from `rendpage` and
  `rendjob`. A real `/dev/full` run ended with IPP job-state=aborted.
- PAPPL increments PWG impressions itself; the probe does not increment again.

Reproduce (no printer required):

```sh
cargo build -p ml216x-printer-app
python3 scripts/p5-probe.py --output /tmp/p5-measurements.json
python3 scripts/p5-probe.py --device-failure --output /tmp/p5-failure.json
```

The tests bind loopback only and stop their temporary server on success or
failure. Inputs and JSONL output stay in a temporary directory. They require
`cc`, `libcups2-dev`, `libpappl-dev` and `ipptool` (`cups-ipp-utils`).

## Historical investigation — 2026-09-05

The text below records the evidence before the maintainer selected 12.5 pt.
References to “today”, “not changed” and “open” below describe that baseline.


*Written 2026-09-05, ahead of P5. **The margin table has not been changed.***

The driver's hard margin is a driver constant derived from
`ppd/samsung-ml2160.ppd`, which declares `*ImageableArea … "12 12 …"` — 12 pt
on every edge of every medium (decision Q-2). While fetching the SpliX source
to settle the copyright attribution, it turned out that **upstream SpliX does
not use 12 pt for either of the two models this driver claims to support**, and
that it uses two different values for them.

This document records what upstream says, what it would cost to be wrong, and
the three ways the question can be resolved. It is deliberately open: the
person reading it has the hardware, and a ruler settles in one page what no
amount of source reading can.

## What upstream actually says

From the Debian source package `splix 2.0.1-1` (`http://splix.ap2c.org/`),
which is the source this driver's protocol is derived from.

`ppd/samsung.drv.in` puts the ML-1915 and ML-2165 in a block of their own, with
an explicit override and a comment saying why:

```
266  }
267  }
268
269  //
270  // ML-1915/ML-2165 printers (different margins than the other monochrome
271  // printers)
272  //
273  {
274      HWMargins 12.5 12.5 12.5 12.5
275      #import "spl2basic.defs"
…
302                  ModelName "ML-2165"
303                  PCFileName "ml2165.ppd"
```

The ML-2160 is not in that block. It sits at line 241, inside the file's
outermost group, which imports `spl2.defs` at line 19 and never overrides the
margins:

```
19   #import "spl2.defs"
…
241                  ModelName "ML-2160"
242                  PCFileName "ml2160.ppd"
```

and `ppd/spl2.defs` sets:

```
 9  // Supported paper format
10  HWMargins 10.75 15 10.75 15
```

No other file in the import chain sets `HWMargins` — `spl2basic.defs` and
`monochrome-v2.defs` do not, and the only other definition in the tree,
`ppd/spl2bandedjbig.defs` line 12, belongs to the banded-JBIG colour printers
imported at `samsung.drv.in:453`, not to the ML-216x at all:

```
10  // For banded jbig printers, all hardware margins seems to be 12pt.
11  // HWMargins left bottom right top
12  HWMargins 12 12 12 12
```

So, per upstream:

| Model | Upstream `HWMargins` (left bottom right top) | Source |
|---|---|---|
| **ML-2160** | `10.75 15 10.75 15` | `spl2.defs:10`, via `samsung.drv.in:19` |
| **ML-2165** (and ML-1915) | `12.5 12.5 12.5 12.5` | `samsung.drv.in:274`, explicit override |
| *(banded-JBIG colour models)* | `12 12 12 12` | `spl2bandedjbig.defs:12` — **not** these printers |
| **This driver's PPD** | `12 12 12 12` | `ppd/samsung-ml2160.ppd` |

Two things follow that were not understood before. Our 12 pt matches **neither**
model upstream describes; the file it does match covers different hardware.
And the ML-2160's upstream margin is **asymmetric** — 10.75 pt left/right,
15 pt top/bottom — whereas every value in our PPD is 12.

Upstream's confidence is worth noting too. The comment attached to the 12 pt
definition says "all hardware margins **seems to be** 12pt". Upstream was
estimating in at least that file, so "upstream says X" is evidence, not proof.

## What it costs to be wrong

`hard_margin_bytes` (`src/main.rs`) converts the margin to pixels and rounds
**up to a whole 8-pixel column**, because the band buffer is byte-addressed.
That rounding is what turns a fraction of a point into a visible error:

| Left margin | @300 dpi | @600 dpi | @1200 dpi |
|---|---|---|---|
| 10.75 pt (upstream ML-2160) | 6 B | 12 B | 23 B |
| **12 pt (this driver today)** | **7 B** | **13 B** | **25 B** |
| 12.5 pt (upstream ML-2165) | 7 B | 14 B | 27 B |

At 300 dpi the 12 and 12.5 pt cases collapse to the same 7 bytes, so the
question is invisible there. Everywhere else it is a whole number of byte
columns:

* **12 vs 12.5 pt** — 1 byte at 600 dpi, 2 bytes at 1200 dpi: 8 and 16 pixels
  respectively, both **≈ 0.34 mm**.
* **12 vs 10.75 pt** — 1 byte at 600 dpi, 2 bytes at 1200 dpi, the same
  ≈ 0.34 mm, in the other direction.

If the true hard margin is larger than the table says, the whole raster lands
that far to the right of where the engine expects, and the rightmost byte
column is pushed towards — or past — the edge of the printable area. This is
the R-1 failure mode, and it is the same class of bug as the D-06 regression
recorded at `src/main.rs:427`, which was 13 bytes and about 4 mm. A third of a
millimetre will not be noticed by eye; it will be measured.

The vertical axis carries the same question. The driver relies on the
horizontal and vertical origins agreeing — `band_placement`'s documentation
works through the 600 dpi case, where centring (100 lines) and `hardMarginY`
(12 pt = 100 lines) cancel exactly. If the ML-2160's true top margin is 15 pt
(125 lines at 600 dpi) rather than 12, that cancellation is wrong by 25 lines
and the page shifts vertically as well.

## The three resolutions

To be picked after measurement, not before.

**(a) Upstream is right and one value serves everything.** The table becomes a
single upstream-derived value for all models. Note this now has two sub-cases
rather than one: 12.5 pt (if the ML-2165 override describes the family) or
10.75/15 pt (if `spl2.defs` does). "12.5 for all models" is only half of
option (a) as originally framed, because our 12 pt was never upstream's value
for the ML-2160 either.

**(b) The models genuinely differ, and the driver-capability table carries
per-model margins.** Upstream separated the ML-2165 deliberately and said so in
a comment, which is a model-specific claim rather than an accident. PAPPL's
driver-capability table can express per-model margins without difficulty; the
single classic PPD cannot, which is an argument for the migration rather than
against it.

**(c) Upstream's `.drv` is wrong for our hardware and 12 pt stays**, with the
comment at the margin table replaced by a citation of the measurement that
proved it — the model, the resolution, the medium, the measured distance.

**What I expect: (b)**, and with our current 12 pt wrong for both models. The
reasoning is that upstream's separation of the ML-2165 is deliberate and
documented, and that our 12 pt traces to a defs file written for different
hardware — which looks like how it got here, rather than a measurement anyone
made on an ML-216x. But (c) is entirely live: upstream hedges in the very file
our value seems to come from, and no source reading beats one printed page.

## How to settle it

Print a `*-marks` golden through the 1.x path and measure. The corpus contains
one-pixel registration marks at the exact printable-area corners on A4 and
Letter at every supported resolution, so the distance from the sheet edge to
the first printed dot **is** the hard margin, and 600 or 1200 dpi will show a
0.34 mm error where 300 dpi will not.

This is release gate **G-1** (`docs/GOLDEN-VALIDATION.md` §4). Two constraints
it now carries:

* **Record the exact model.** Measuring an ML-2160 says nothing about the
  ML-2165 and vice versa — that is the whole question.
* **Measure every model we claim.** The README and PPD advertise ML-2160,
  ML-2165, ML-2165W and ML-2168. If the models differ, G-1 needs a measurement
  per model, or the claim narrows to what has been measured.

## If the table changes

Changing the margin changes the bytes that reach the printer, so every one of
the 32 goldens moves. That is a deliberate behaviour change and follows the
bless discipline in `CONTRIBUTING.md`: refresh in the same commit, cite the
measurement in the commit message, and include the diff in review. The
registration-mark cases exist precisely so that this shows up as a byte
difference rather than a ruler difference.
