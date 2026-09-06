# Golden Files

**Baseline update, 2026-09-06:** the maintainer selected 12.5 pt margins.
The corpus now freezes that intentional change; it is no longer expected to
match the untouched 12 pt `v1.x-final` binary. Thirty production SPL streams
changed; the two synthetic SPL streams remain byte-identical. Every sidecar
records the exact driver margin separately from truncated integer CUPS fields.
See `docs/GOLDEN-VALIDATION.md` for the audit of this refresh.


**2026-09-06, P6:** the SPL2 engine moved to `crates/spl2-core` and the frozen
filter now drives it through `spl2_core::engine`. All 32 streams are byte
identical across that move and `SHA256SUMS` is unchanged, which is the whole
point of capturing them before the split.

This directory freezes **the SPL2/QPDL bytes the 1.x CUPS filter produces**
before the move to a PAPPL Printer Application. The acceptance criterion is
byte-for-byte identity (project rule 6), and that criterion needs a reference
to compare against.

The code that produces and compares them: [`../src/golden.rs`](../src/golden.rs).

## Files

| File | Content |
|---|---|
| `<case>.spl` | The raw SPL2/QPDL stream the filter produced |
| `<case>.json` | The classic CUPS page header that produced that stream, plus the QPDL placement values the filter derived from it |
| `SHA256SUMS` | Checksums of every file above, so a validated corpus has a fixed reference |

The raster **inputs** are not kept in the repository; `build_raster` in
`src/golden.rs` generates them deterministically (the A4 @1200 DPI input alone
is ~16 MB).

## Usage

```sh
# Compare (the default; on every `cargo test --workspace` run)
cargo test -p rastertospl-rust golden

# Refresh AFTER a deliberate behaviour change
UPDATE_GOLDENS=1 cargo test -p rastertospl-rust golden

# Verify the committed bytes (from the repository root)
(cd goldens && sha256sum -c SHA256SUMS)

# Export the inputs (for manual inspection / comparison with the installed binary)
DUMP_GOLDEN_RASTER=/tmp/r cargo test -p rastertospl-rust golden
```

Every diff in a `.spl` file means **the bytes going to the printer changed**.
Refresh only together with a deliberate change, and include the diff in review.

## Corpus coverage

32 cases. Every case whose name starts with `synth-` is **synthetic** — see
below.

* Registration-mark (`*-marks`) cases for A4 and Letter at **every supported
  resolution** (300x300, 600x600, 1200x600, 1200x1200).
* **Every one of the eleven PPD media sizes** has at least one case, at 600
  DPI: A4, Letter, Legal, Executive, A5, A6, B5, Env10, EnvDL, EnvC5, Folio.
  The hard-margin table is per-size, so an uncovered size would be an
  uncovered margin.
* **Legal and Folio at every resolution**, as the two geometric extremes: the
  tallest sheets produce the most lines, the most bands and the widest hard
  margins. Legal @1200x1200 is the worst case in the whole matrix at 129 bands
  against the 256-band ceiling of the QPDL band-order field.
* `a4-300-marks` / `letter-300-marks` / `legal-300-marks` / `folio-300-marks`
  cover the 64-line band rule; everything else produces 128-line bands.
* `a4-600-marks-3copies` covers the copy field, `a4-600-marks-3pages` covers
  multiple pages, and `a4-600-marks-2pages-1000copies` covers both at once
  together with the `sanitize_copies` clamp (1000 requested, 999 sent).
* `envc5-600-marks-manual-env` covers the tray + media-type mapping.
* `a4-600-marks-utf8-title` covers non-ASCII job metadata. PAPPL passes the
  IPP job-name through as UTF-8, which the classic filter path rarely saw;
  this freezes what `quote_untrusted` currently does with it.
* `a4-600-marks-v2rle` uses a line-RLE (`RaS2`) input; it must come out
  byte-for-byte identical to `a4-600-marks`.
* `a4-600-blank` is a completely blank page.

### The synthetic cases, and why they are not cheating

`synth-a4-600-dst14` and `synth-a4-600-dst94` are **fabricated geometries: no
real printer reports these imageable areas.** They exist because
`band_placement` positions the CUPS line at `centred - hard_margin`, and every
medium in this PPD has a 12.5 pt left margin — which at every supported
resolution makes the hard margin at least as large as the centring offset. So
`dst_offset` is 0 in every realistic case, only `src_skip` varies, and the
positive branch has no golden at all.

That branch is not dead code: it is exactly the one that activates the first
time PAPPL hands us a different margin, which is risk R-1 in
`docs/MIGRATION-PLAN.md`. Freezing it now, while the 1.x encoder still runs,
is the only chance to capture what it should produce.

What the two cases fabricate is only the **imageable area**. Both keep A4's
real 595 x 842 pt sheet, because `validate_page_header` rejects any `PageSize`
that is not an exact known QPDL paper size — so they vary precisely the two
header fields (`Margins[0]` and `cupsWidth`) that PAPPL could plausibly
deliver differently, and nothing else. `test_golden_synthetic_media_use_a_real_sheet`
enforces that, and the sidecars carry `"synthetic": true`.

| Case | Left margin | Imageable width | Hard margin | Centring | `dst_offset` |
|---|---|---|---|---|---|
| `synth-a4-600-dst14` | 6 pt | 554 pt | 7 B | 21 B | 14 |
| `synth-a4-600-dst94` | 12 pt | 388 pt | 13 B | 107 B | 94 |

### Accepted gaps

Two gaps are deliberately left open rather than filled; see
`docs/GOLDEN-VALIDATION.md` §3 for the reasoning.

* **Media types beyond `ENV` and unset.** Thirteen of the fifteen PPD media
  types are covered by unit tests against the PPD, not by golden bytes.
* **Duplex page-header bytes.** Every case is Simplex, so bytes `0xB`/`0xC`
  are frozen in one state. Duplex is a non-goal for 2.0
  (`docs/NON-GOALS.md`).
