# Non-Goals for the 2.0 Printer Application

Things deliberately left out of the 2.0 release, with the reasoning that put
them here and — where it applies — the evidence that would unblock them.

This document exists so these exclusions are not re-litigated. If you are about
to implement one of these, read the "what would unblock it" note first; if the
evidence it asks for still does not exist, the exclusion still stands.

Decided 2026-09-05; see `docs/DECISIONS.md` for the full decision log.

---

## Duplex printing

**Excluded from 2.0.** The Printer Application advertises single-sided output
only.

The SPL2/QPDL side is already implemented and tested: `duplex_mode`
(`crates/spl2-core/src/geometry.rs`, `duplex_mode`) maps the CUPS `Duplex`/`Tumble` pair to `SplDuplex`,
`begin_job` emits `@PJL SET DUPLEX=MANUAL` plus `BINDING=LONGEDGE|SHORTEDGE`,
and `begin_page` writes the duplex and tumble bytes at page-header offsets
`0xB` and `0xC`. None of it is reachable today, because
`ppd/samsung-ml2160.ppd` has no `*OpenUI *Duplex` block, so CUPS never sets
`Duplex` in the raster header and every job is Simplex.

Three reasons it stays out:

1. **No hardware to validate it on.** The ML-2160 series has no automatic
   duplexer; every duplex job on this family is *manual* duplex, which cannot
   be verified without a person feeding paper back in.
2. **It requires two-pass page ordering that does not exist.** Manual duplex
   prints one side of the whole job, then the operator reloads and the other
   side prints. The current pipeline streams pages in arrival order in a single
   pass and never splits the page sequence into passes. Shipping duplex without
   this prints double-sided jobs in the wrong order.
3. **It requires a last-page tray override that cannot be computed while
   streaming.** SpliX's `qpdl.cpp renderPage` temporarily switches the paper
   source to `Multi` (3) on the front-side pass of every page except the last
   (`if (tumble && !lastPage) paperSource = 3`). In a streaming CUPS raster
   pipeline the next page header cannot be read until the current page's data
   is consumed, so "is this the last page?" is unknown when the page header is
   written. Doing it correctly means buffering a whole page in memory — up to
   roughly 22 MB at the largest accepted geometry.

Both gaps are documented in the code, in `crates/spl2-core/src/geometry.rs`
beside `duplex_mode` and in `crates/spl2-core/src/qpdl.rs`. (They used to be
cited by line number in `src/main.rs`, which P11 deleted.)

**Requirement for 2.0 despite the exclusion:** publish `sides-supported`
explicitly as one-sided only, rather than omitting the attribute. IPP clients
handle a present-but-limited attribute better than a missing one.

**What would unblock it:** access to the hardware for manual-duplex testing,
plus a decision to buffer pages (or to reorder at the job level) so the
two-pass sequence and the last-page tray override can both be implemented.

---

## Toner save / print density

**Excluded from 2.0.** No vendor option is exposed for density or economode.

`begin_job` currently emits a hardcoded `@PJL SET DENSITY=3`
(`src/spl.rs:888`), alongside several other hardcoded PJL settings
(`POWERSAVE=ON`, `POWERSAVETIME=5`, `JAMRECOVERY=OFF`, `ALTITUDE=LOW`,
`RET=NORMAL`). There is no PPD option for any of them and no code path that
varies them.

The reason it stayed out was that **we did not know the printer's real PJL
vocabulary for toner save**, and project rule 1 forbids inventing protocol
details. That premise was checked against the installed sibling PPDs and found
to hold at the time.

**Correction, 2026-09-05: the premise was wrong.** It was based on the PPDs
available then; the SpliX *source* package settles it. `ppd/ml1910.ppd` in
SpliX 2.0.1 does declare the option —

```
*OpenUI *EconoMode/Toner Save: PickOne
*DefaultEconoMode: 0
*EconoMode 0/Use Printer Default: ""
*EconoMode ON/Save: ""
```

— and the details are below. The exclusion stands as a scheduling decision
(Q-4: deferred, nothing invented, no vendor option in 2.0), but it no longer
rests on "there is no source for this". Whoever revisits it should know the
evidence exists rather than re-deriving that it does not.

**Required cleanup in 2.0 even though the feature is excluded:** replace the
hardcoded `3` with a named constant carrying a comment that cites where the
value came from, so a future capture can be wired in without archaeology.

**What would unblock it:** a capture of the traffic the Samsung/Windows driver
sends when toner save is toggled — enough to show the exact PJL line, its
accepted values, and whether `DENSITY` is the same knob or a separate one.
A packet or USB capture, or a vendor `.ppd`/`.ini` that names the option, would
all do. With that evidence the option can be added as a proper IPP vendor
attribute.

**Evidence found 2026-09-05, not yet acted on.** The SpliX source fetched to
settle the copyright attribution (`splix 2.0.1-1`) contains exactly what this
note asks for, from the same upstream the rest of the protocol is derived
from — so it satisfies project rule 1 without any guessing:

* `src/printer.cpp:212-215` emits `@PJL SET ECONOMODE=%s` from a PPD option,
  and `ppd/tonersave.defs` declares that option as
  `"EconoMode/Toner Save"` with the choices `0` (use printer default), `ON`
  and `OFF`.
* `src/printer.cpp:280-284` emits `@PJL SET DENSITY=%s` from a `TonerDensity`
  option, defaulting to `@PJL SET DENSITY=3` when it is absent — which is
  where this driver's hardcoded `3` comes from. `ppd/tonerdensity.defs`
  declares the choices as `1` (light), `3` (medium, default) and `5` (dark).
* Both defs are imported by the ML-2160 **and** the ML-2165 blocks of
  `ppd/samsung.drv.in`, so upstream believes this hardware accepts them.

This does not reopen the decision — it is still deferred, and nothing has been
implemented. What it does change is the required 2.0 cleanup above: the named
constant replacing the hardcoded `3` can now cite `printer.cpp:280-284` and
`tonerdensity.defs` as its source instead of citing nothing. Whether to expose
the options is a separate call for the maintainer, and printing a page with
each setting is the cheapest way to confirm the firmware honours them.
