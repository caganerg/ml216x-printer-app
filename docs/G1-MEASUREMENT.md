# P12 — hardware bring-up, and release gate G-1

*Written 2026-09-07. Nothing in this document has been carried out on
hardware; it is the procedure and the record form, and every measured field
below is blank on purpose.*

P12 is the migration plan's hardware step: the printer is connected, the
packaged application is accepted on the maintainer's machine, and release gate
**G-1** — the physical margin measurement — is taken. Everything in P12 that
can be done without a printer has been done and is described here. What is
left needs a Samsung ML-216x, paper and a millimetre ruler, and none of it can
be inferred from a passing test suite. `docs/GOLDEN-VALIDATION.md` section 4
states why: the golden corpus derives every margin from the same 12.5 pt
`*ImageableArea`, so byte-for-byte agreement proves internal consistency and
says nothing about where toner lands.

## 1. What the software half already proves

`scripts/g1-probe.py` prints the measurement page through the real printer
application to a `file://` device, decodes the QPDL stream back into a bitmap
with the engine's own decompressor
(`crates/spl2-core/examples/qpdl-decode.rs`), and checks that every ruler
tick, every corner bracket and the calibration span sits on the pixel the
driver's geometry predicts.

```sh
cargo build -p ml216x-printer-app
python3 scripts/g1-probe.py --all
```

On 2026-09-07 that passed for **all 44 cases** — the 11 media the application
publishes across all 4 resolutions — and the harness has been shown to fail
three ways, each standing for one of the risks it claims to cover:

| Injection | The risk it stands for | What it reports |
|---|---|---|
| `--inject shift` | R-1, the page displaced sideways | 50 failures; the first is a 3 px error on the left ruler's 5 mm tick |
| `--inject crop` | Q-13, the wrong number of lines dropped | 3 failures; two ticks short on the top ruler |
| `--inject scale` | R-4, a resolution or scale error | 25 failures; tick spacing out by 2 px |

So the page that reaches the device is the page that was meant, on every
medium and resolution. That leaves **exactly one unknown**, and it is physical:
where the engine's first printable pixel actually sits on the sheet.

## 2. The measurement page

`scripts/g1-page.c` generates it as full-media PWG Raster. It replaces
`scripts/pwg-probe-input.c`'s single-pixel corner marks, which were right for
P5 — they proved the callbacks carry full media — but are 42 µm across at
600 dpi and no ruler resolves them.

The page carries four features, and each answers a different question:

* **Four corner brackets**, 15 mm arms, 1 mm thick, their outer corners on the
  predicted printable-area corners. The top-left bracket's outer corner is
  *the first pixel the driver emits at all*, so measuring where it lands
  measures the margin directly. The harness asserts that it is still pixel
  (0, 0) of the page; if that changes, the measurement below no longer means
  what this document says it means.
* **Four rulers**, one per paper edge, one tick per millimetre out to 25 mm,
  every fifth tick longer and every tenth longer still and numbered. Ticks are
  placed at whole millimetres of *predicted physical distance from the paper
  edge*, not at whole millimetres of sheet coordinate — so a ruler laid with
  its zero on the paper edge reads the error off directly.
* **A calibration cross** spanning exactly 100 mm on each axis (50 mm or
  25 mm on media too small for it). This separates a scale error from an
  offset: a swapped resolution axis makes the span the wrong *length*, which a
  margin error cannot do.
* **A caption** naming the medium, the resolution and the margin, because a
  stack of test prints is otherwise a stack of indistinguishable rulers.

Ticks that fall inside the margin are drawn too. They are predicted to be
clipped, so if one appears on paper the crop is *smaller* than modelled — that
is a positive signal, not a missing feature.

### Two predictions that are not the same number

The generator reports both, and the distinction matters when reading the sheet:

* `in_stream` — the tick survives the driver's own drops. The driver removes
  the left margin and crops top and bottom, but **nothing trims the right**, so
  a tick 1 mm from the right paper edge is encoded and simply never printed.
* `on_paper` — the tick also lands inside the printable area, which is where
  the engine can lay toner.

### The 0.33 mm the horizontal axis is expected to be out by

This is predicted, not a defect found on paper, and it should be read before
anyone concludes the margin is wrong.

`hard_margin_bytes` rounds the margin **up to a whole byte column**, because
the band buffer is byte addressed: 12.5 pt at 600 dpi is 104.17 px, which
becomes 112 px, or 4.74 mm. The engine's first printable column is at the
physical hard margin, 4.41 mm. So sheet content is predicted to sit
**0.33 mm further left than its nominal sheet position** at 600 dpi. The
generator reports this as `horizontal_alignment_shift_hmm`, and the harness
prints it on every pass. The 1.x filter shares the rule, so both front ends
share the shift — which is why no golden and no test in the tree can see it,
and why the rulers are placed in predicted-physical rather than sheet
coordinates.

The vertical axis has no such rounding: `hard_margin_lines` takes the nearest
scanline, 104 at 600 dpi, which is 0.04 mm short of 4.41 mm. Open question
**Q-13** in `docs/DECISIONS.md` records why that number is an estimate rather
than a transcription, and why G-1 must measure the vertical margin and not
only the horizontal one.

## 3. Printing the page on the real queue

Produce the files, then submit them to the maintainer's own IPP queue rather
than to the harness's `file://` device:

```sh
# Writes g1-<medium>-<x>x<y>.pwg, the SPL2 stream it produced, and the JSON
# describing every feature drawn.
python3 scripts/g1-probe.py --media iso_a4_210x297mm --resolution 600x600 \
        --keep ~/g1-pages

# Then, on the machine with the printer, through the queue that already works:
ipptool -tv ipp://127.0.0.1:8631/ipp/print/ML2160 /dev/stdin <<'TEST'
{
OPERATION Print-Job
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR naturalLanguage attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name g1
ATTR mimeMediaType document-format image/pwg-raster
GROUP job-attributes-tag
ATTR boolean ipp-attribute-fidelity true
ATTR keyword media iso_a4_210x297mm
ATTR resolution printer-resolution 600x600dpi
FILE /home/USER/g1-pages/g1-iso_a4_210x297mm-600x600.pwg
STATUS successful-ok
}
TEST
```

`ipp-attribute-fidelity` is set deliberately: the page is drawn for one exact
medium and resolution, and a server that silently substituted another would
produce a sheet that measures wrong for a reason that has nothing to do with
the margin.

**One tooling note, so it does not derail the session.** `ipptool -t` fails
every `Get-Printer-Attributes` against this printer, reporting
`document-format-supported`'s `application/octet-stream` as having "bad
characters (RFC 8011 section 5.1.10)". That report is wrong and it is not
this driver's bug: the server answers `successful-ok`, the value on the wire
is 24 clean bytes, and libcups' own `ippValidateAttributes` accepts the entire
response — all three checked on 2026-09-07 with CUPS 2.4.10-3+deb13u2.
`Print-Job` is unaffected, which is why every probe in `scripts/` uses it.
`g1-probe.py` asks for the capability list with a hand-built IPP request
instead.

## 4. Measuring the sheet

Use a steel rule with millimetre graduations, or a caliper for the bracket
corners. Measure at both ends of each edge, not once: a skew shows up as a
difference between the two, and a skew is not a margin error.

For each printed page, record:

1. **Bracket corners.** The distance from each paper edge to the *outer* edge
   of the nearer bracket arm, at all four corners. Predicted: 4.41 mm on the
   top and left, and within 0.06 mm of that on the right and bottom — the
   generator's `brackets[].predicted_hmm` gives the exact figure per case.
2. **Ruler zero.** Lay the rule with its zero on the paper edge and read the
   position of the tick numbered 10 and the tick numbered 20, on all four
   rulers. Predicted: 10.0 mm and 20.0 mm. A constant error on both is a
   margin error; a growing one is a scale error.
3. **The first tick that printed.** Predicted: the 5 mm tick, with 1 to 4 mm
   clipped. Anything lower means the crop is smaller than modelled.
4. **The calibration span**, end tick to end tick, on both axes. Predicted:
   exactly 100.0 mm (or the span the caption names).

### G-1 record form

G-1 requires a measurement **per model**, not per family: upstream SpliX gives
the ML-2160 and the ML-2165 different hard margins and this driver uses the
ML-2165's 12.5 pt (`docs/MARGINS.md`). The README and the PPD claim ML-2160,
ML-2165, ML-2165W and ML-2168, so either all four are measured or the claim is
narrowed to those that were.

| Field | Value |
|---|---|
| Date | |
| Model, exactly as the label reads | |
| Firmware / device ID reported | |
| Package version | |
| Medium | |
| Resolution requested | |
| Top margin, left end / right end | |
| Bottom margin, left end / right end | |
| Left margin, top end / bottom end | |
| Right margin, top end / bottom end | |
| Ruler 10 mm tick, top / bottom / left / right | |
| Ruler 20 mm tick, top / bottom / left / right | |
| Lowest tick printed, per ruler | |
| Calibration span, horizontal / vertical | |
| Paper stock and weight | |
| Measuring instrument | |

Copy the table per model. One A4 page at 600 dpi clears the gate for a model;
a second at 300 dpi is worth taking because the 300 dpi band height is halved
(`band_height_for`) and nothing else in P12 exercises that on paper.

### What each outcome means

| Measured | Reading |
|---|---|
| Within ±0.5 mm of prediction on all four edges | G-1 passes for that model. Record it and close R-1 for it. |
| A constant offset on both horizontal edges | The horizontal margin constant is wrong. `hard_margin_bytes`, the PPD's `*ImageableArea` and every golden that depends on it are re-captured. |
| A constant offset on both vertical edges only | Q-13's rule is wrong — the crop, not the rounding. Candidates (b) and (c) in that entry become live. |
| Content shifted 4–5 mm on one edge and clipped on the opposite one | The margin is being applied twice, or not at all, on that axis. `docs/MARGINS.md` names this trap. |
| The calibration span the wrong length | A scale or resolution-axis fault (R-4), not a margin fault. Fix that before reading any margin off the page. |
| Ticks below 5 mm printed | The crop is smaller than modelled; re-derive `hard_margin_lines` from the measurement. |

A disagreement is a finding to write up, not a number to adjust the constant
to until the page looks right: the constant is derived from the PPD, so a
measurement that contradicts it contradicts the PPD too, and both change
together or neither does.

## 5. The rest of P12 — the acceptance run

These are the checks `docs/HARDWARE-TESTING.md` has been carrying as pending,
gathered here so one session at the printer covers them. They are unchanged;
only their location is new.

1. **`uaccess` on the USB node.** With the printer plugged in and the
   maintainer logged in locally, `getfacl /dev/bus/usb/<bus>/<dev>` shows a
   `user:<name>:rw-` entry. Over SSH it will not, and that is expected — there
   is no active local session for the ACL to attach to; the README's group `lp`
   fallback is the answer for a headless machine.
2. **No root anywhere.** `ml216x-printer-app devices` lists the printer without
   `sudo`; `systemctl --user status ml216x-printer-app` is active; and
   `ps -o user= -C ml216x-printer-app` shows the maintainer.
3. **The duplicate queue.** Upgrade, delete only the `ML-2160-Series` raw
   queue, then reconnect the cable and power-cycle the printer. Only the
   working IPP queue may remain, with no driver-required notification, and
   printing must still work without deleting anything by hand.
4. **The IEEE-1284 device ID**, which is still unrecorded. The PPD does not
   carry it either — it records `MFG:Samsung;MDL:ML-2160 Series;CMD:SPL,FWV,EXT;`
   and nothing about USB. `lsusb -v` for `04e8:330f`, or the queue's own
   `printer-device-id`, supplies it.

Do not mark any of these passed from a syntax check or a package build.

## 6. What P12 does not do

P12 does not remove the 1.x filter. That is **P11**, it needs the delete list
approved separately (decision Q-5), and the PPD stays permanently as project
data regardless.

G-1's wording in `docs/GOLDEN-VALIDATION.md` predates the printer
application: it asks for a page printed "through the 1.x path". The page in
this document goes through the *shipping* path instead, which is the one whose
margins users will meet. Whether the gate's wording should follow is a
decision the maintainer owns; it is written up as an open question in
`docs/DECISIONS.md` rather than settled here.
