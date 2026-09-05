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
exists to prevent.

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
