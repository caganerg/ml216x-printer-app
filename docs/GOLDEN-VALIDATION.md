# Golden Corpus Validation — 2026-09-05

**Historical validation below covers the original 12 pt corpus.** On
2026-09-06 the maintainer explicitly selected 12.5 pt and authorized P5.
The current corpus deliberately differs from `v1.x-final`; see the refresh
record at the end of this document.


The 15-case golden corpus in `goldens/` is the reference every later step of
the PAPPL migration compares against: ground rule 6 makes byte-for-byte
identity the acceptance criterion, so if these bytes are wrong, every "no
change in behaviour" claim built on them is wrong too. This document records
the validation the corpus was put through before any PAPPL code was written,
so the result does not have to be taken on trust.

## 1. Reproduction from the untouched pre-migration tree

**Result: all 15 cases reproduce exactly.**

The literal check — "regenerate the goldens from `v1.x-final`" — is not
possible, because the generator does not exist at that tag: `v1.x-final`
(`33d4ff2`) predates both `4a5552a` (which made the PJL service date
injectable) and `1cf2d0b` (which added `src/golden.rs`), and
`process_cups_raster_to_spl` there has no `service_date` parameter. The
equivalent check that *is* possible drives the untouched binary instead of the
untouched generator:

```sh
git worktree add /tmp/v1x v1.x-final && (cd /tmp/v1x && cargo build --release)
DUMP_GOLDEN_RASTER=/tmp/rasters cargo test --release golden
for r in /tmp/rasters/*.raster; do
    /tmp/v1x/target/release/rastertospl-rust 1 tester golden 1 '' "$r" > out.spl
done
```

Every one of the 15 streams produced by the `v1.x-final` binary is identical in
length to its golden and differs in exactly **two bytes** — the day and the
last digit of `@PJL DEFAULT SERVICEDATE=20260101`, which the corpus pins and
the 1.x binary reads from the clock. With that line normalised, the streams are
byte-for-byte identical. The raster inputs are generated deterministically by
`build_raster` in `crates/spl2-core/tests/golden.rs` (it was `src/golden.rs`
until 2026-09-10, when the harness moved so that it would outlive the 1.x
binary it measures); the filter's argv `num_copies` is
deliberately ignored (`src/main.rs:22`), so copies come from the raster header
in both paths.

The corpus therefore does **not** depend on the one encoder-adjacent change
that was made before it was captured: `4a5552a` injects the service date but
does not alter any other byte.

`goldens/SHA256SUMS` pins the validated bytes so this check has a fixed
reference next time; verify with `cd goldens && sha256sum -c SHA256SUMS`.

## 2. Proof that the harness can fail

A harness that never goes red is not a safety net. Each of the five migration
risks in `docs/MIGRATION-PLAN.md` §9 was injected as a deliberate minimal
defect, one at a time, into a scratch worktree, and the full suite was run:

| Risk | Injected defect | Suite | Caught by `golden::test_goldens_match` | Other tests that caught it |
|---|---|---|---|---|
| R-1 | `hard_margin_bytes` returns one byte too many | RED | yes | 5 |
| R-2 | band polarity inversion removed | RED | yes | 3 |
| R-3 | band height forced to 128 at 300 dpi | RED | yes | 3 |
| R-4 | resolution axes swapped in the 17-byte page header | RED | yes | 1 |
| R-5 | two job-level PJL lines reordered | RED | yes | **0** |

All five are caught, and the golden corpus catches all five on its own. R-5 is
caught by **nothing else**: reordering `@PJL SET ALTITUDE=LOW` and `@PJL SET
DENSITY=3` leaves all 106 other tests green. That is the case for keeping the
corpus in CI rather than relying on unit tests, and it is why `CONTRIBUTING.md`
makes refreshing a golden a reviewed act rather than a build step.

**That CI exists as of 2026-09-10**, which it did not when the sentence above
was written: `.github/workflows/checks.yml` runs the corpus through the `test`
group of `scripts/run-checks.sh` on every push, and the `goldens` group
verifies `goldens/SHA256SUMS` besides, so a blessed corpus cannot drift from
its recorded checksums. What CI still cannot do is measure paper; see
[`docs/CI.md`](CI.md).

**Re-run against the enlarged corpus (32 cases).** All five defects are still
caught, and the number of tests catching each is unchanged — 6, 4, 4, 2 and 1
respectively. That is expected rather than disappointing: all 32 cases live
inside the single `golden::test_goldens_match` test, so widening the corpus
deepens the evidence behind that one test (more geometries, more resolutions,
the positive `dst_offset` branch) without adding new test names to the failure
list.

## 3. Coverage, and what is not covered

As first captured, the 15 cases covered:

| Axis | Covered by the original 15 |
|---|---|
| Media | A4, Letter, A5, EnvC5 |
| Resolution | 300×300, 600×600, 1200×600, 1200×1200 (A4 and Letter at all four) |
| Band height | 64 (the two 300 dpi cases) and 128 (all others) |
| Input slot | unset (→ Auto) and `MediaPosition` 2 (Manual) |
| Media type | unset and `ENV` |
| Copies | 1 and 3 |
| Pages | 1 and 3 |
| Input encoding | `RaS3` uncompressed and `RaS2` line-RLE (identical output asserted) |
| Content | four 1-pixel corner registration marks, and one blank page |

The corpus now holds **32 cases**. What was missing, and what was done about
it, follows.

### Gaps closed on 2026-09-05

The corpus was expanded from 15 to 32 cases; see `goldens/README.md` for the
full list.

* **`dst_offset > 0`** — two synthetic cases, at offsets 14 and 94. They
  fabricate only the imageable area and keep A4's real sheet, because
  `validate_page_header` rejects unknown paper sizes. This was the gap that
  mattered: the positive branch is the one that activates the first time PAPPL
  hands us a margin the PPD never produces, which is R-1.
* **All eleven PPD media sizes**, plus Legal and Folio — the two geometric
  extremes — at every supported resolution.
* **Multi-page combined with multi-copy**, including the `sanitize_copies`
  clamp (1000 requested, 999 written into both the page header and the
  footer): the R-5 "copies counted twice" scenario.
* **Non-ASCII job metadata**, freezing what `quote_untrusted` does with a UTF-8
  title today — it transliterates to ASCII — before PAPPL starts passing IPP
  job-names through unchanged.
* **The band-order ceiling** is now proved across the whole matrix rather than
  sampled: worst case Legal @1200x1200 at 129 bands of the 256 the field can
  address, with a compile-time assertion bounding it from the validator's own
  limits.

### Accepted gaps

These are deliberately left uncovered rather than filled:

* **Media types beyond `ENV` and unset.** Thirteen of the fifteen PPD media
  types map to a PJL `PAPERTYPE` string and nothing else; the mapping is
  covered by unit tests against the PPD, and a golden per type would freeze
  the same one-line difference fifteen times.
* **Duplex page-header bytes `0xB`/`0xC`.** Every case is Simplex. Duplex is a
  non-goal for 2.0 (`docs/NON-GOALS.md`) and cannot be validated without
  hardware anyway; if it is ever implemented, the corpus grows with it.

## 4. Physical measurement — NOT DONE

**R-1 remains unverified.** No printer is attached to this machine (`lpstat -p`
reports no destinations; no Samsung device on USB), so no registration-mark
golden has been printed and measured against the PPD's `*ImageableArea`.

This matters because the corpus can be self-consistently wrong: every case
derives its margin from the same 12.5 pt `*ImageableArea` value, so byte-for-byte
agreement proves internal consistency and nothing about where toner lands on
paper. Until one `*-marks` case is printed through the 1.x path and measured
with a ruler, **R-1 is validated only against itself**.

### Release gate G-1 — measure the margins on real hardware

This is a **release gate, not a documentation note**. It is written here as a
condition on shipping, so that a green test suite is never mistaken for a
verified margin.

> **G-1.** 2.0 does not ship until a registration-mark golden has been printed
> through the 1.x path and its margins measured against
> `ppd/samsung-ml2160.ppd`'s `*ImageableArea` (12.5 pt = 4.41 mm on every
> medium). The record must name **the exact model measured**, the medium, the
> resolution and the measured distance. If the measurement disagrees with the
> PPD, the hard-margin table is wrong and every golden that depends on it is
> re-captured before 2.0 is built.
>
> **Per model, not per family.** Upstream SpliX gives the ML-2160 and the
> ML-2165 *different* hard margins. This driver selects the ML-2165
> value of 12.5 pt (`docs/MARGINS.md`). Measuring an ML-2160 therefore does not clear the
> gate for an ML-2165, or the reverse. G-1 requires a measurement for **every
> model the package claims to support** — the README and the PPD currently
> name ML-2160, ML-2165, ML-2165W and ML-2168 — or the claim is narrowed to
> the models actually measured.

The gate lands naturally in **P12**, which needs the printer connected anyway:
the same session that prints the first PAPPL job can print the 1.x reference
page and measure both. Until G-1 is satisfied, R-1 is carried as an open risk
in every status report, and `docs/MARGINS.md` (the P5 printable-area
experiment) does not close it — that experiment establishes what PAPPL
delivers, not where the toner lands.

**The procedure for taking the measurement is
[`docs/G1-MEASUREMENT.md`](G1-MEASUREMENT.md)**, added 2026-09-07: a
ruler-bearing page a millimetre rule can actually read, a harness that proves
the page survives the driver unchanged on all 11 media and 4 resolutions, and
the record form to fill in per model. The single-pixel corner marks of the
`*-marks` corpus cases are not what to print for this; at 600 dpi they are
42 µm across.

The gate text above is left exactly as written, including "through the 1.x
path". The runbook measures the *shipping* path instead, because that is what
the package installs, and whether the gate's wording should follow is open
question **Q-20** in `docs/DECISIONS.md` rather than something the runbook
decided.


## 2026-09-06 — authorized 12.5 pt refresh

Reason: maintainer instruction to adopt SpliX ML-2165's 12.5 pt margin and
proceed with P5. This is **not** a newly measured hardware value; G-1 remains
open per model. `src/media.rs`, the PPD and the golden media table change
in the same working-tree change.

The harness now preserves fractional points and records `hard_margin_pt` in
all 32 sidecars. CUPS' integer header margins remain truncated values; they
no longer determine the production filter's placement. Production golden
cases inject the same 12.5 pt constant; only two synthetic cases retain their
historical 6/12 pt test geometry. The older cupsfilter height measurements
remain tests of the legacy rounding rule, not measurements of the new PPD.

**30 SPL streams changed; the two synthetic streams did not.** PJL and the
compression algorithm were unchanged. The first changed byte is the page
height at offset 315, caused by reducing the printable height by one point.
Band payloads/placement also change, and Legal @1200 needs 128 bands instead
of 129. `SERVICEDATE` stays pinned. All golden comparisons pass after refresh.
The original mutation results above remain historical evidence; they were
not rerun for this margin change.

| Case | Previous bytes | Updated bytes | First difference |
|---|---:|---:|---:|
| a4-1200-marks | 89600 | 89604 | 315 |
| a4-1200x600-marks | 45451 | 45455 | 315 |
| a4-300-marks | 16167 | 16167 | 315 |
| a4-600-blank | 28687 | 28687 | 315 |
| a4-600-marks-2pages-1000copies | 57321 | 57197 | 315 |
| a4-600-marks-3copies | 28819 | 28757 | 315 |
| a4-600-marks-3pages | 85823 | 85637 | 315 |
| a4-600-marks-utf8-title | 28838 | 28776 | 334 |
| a4-600-marks-v2rle | 28819 | 28757 | 315 |
| a4-600-marks | 28819 | 28757 | 315 |
| a5-600-marks | 16927 | 16923 | 315 |
| a6-600-marks | 10115 | 10113 | 315 |
| b5-600-marks | 22691 | 22687 | 315 |
| env10-600-marks | 16422 | 16420 | 315 |
| envc5-600-marks-manual-env | 19124 | 19124 | 315 |
| envc5-600-marks | 19124 | 19124 | 315 |
| envdl-600-marks | 15549 | 15172 | 315 |
| executive-600-marks | 23845 | 23845 | 315 |
| folio-1200-marks | 99596 | 99600 | 315 |
| folio-1200x600-marks | 50449 | 50453 | 315 |
| folio-300-marks | 17925 | 17925 | 315 |
| folio-600-marks | 31969 | 31907 | 315 |
| legal-1200-marks | 110248 | 109399 | 314 |
| legal-1200x600-marks | 55784 | 54935 | 315 |
| legal-300-marks | 19520 | 19225 | 315 |
| legal-600-marks | 35114 | 34581 | 315 |
| letter-1200-marks | 85571 | 85571 | 314 |
| letter-1200x600-marks | 43021 | 43021 | 314 |
| letter-300-marks | 15095 | 15095 | 315 |
| letter-600-marks | 27121 | 27119 | 314 |

## 2026-09-06 — P6: the corpus survives the crate split unchanged

The SPL2 engine moved out of the filter binary into `crates/spl2-core`, and the
filter now drives it through the same `engine::PageSetup` / `engine::BandEncoder`
seam the printer application uses. **All 32 streams are byte identical and
`SHA256SUMS` is unchanged**, which is what the corpus was captured for: a
refactor of this size is exactly the case where "no change in behaviour" cannot
be taken on trust.

The band loop changed shape in the move — it takes pushed scanlines instead of
pulling them from a `CupsRasterReader`, because PAPPL pushes — so R-1 was
re-injected against the extracted code (`hard_margin_bytes` returning one byte
too many, now in `crates/spl2-core/src/geometry.rs`). `golden::test_goldens_match`
still fails on it, so the harness still covers the code after it moved. R-2 to
R-5 were not re-run; they were not re-run for the 12.5 pt refresh either.

The printer application's own geometry is covered separately, and not by this
corpus: it produces different page heights by design, because PAPPL delivers
full media. Its checks are
`full_media_and_printable_area_place_the_sheet_identically` and
`cropped_height_matches_the_printable_area` in
`crates/ml216x-printer-app/src/driver.rs`, plus the 17-case
`scripts/p5-probe.py --spl` run against a real IPP server.
