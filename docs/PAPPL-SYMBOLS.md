# PAPPL Symbol Table — nothing bound is newer than 1.3

Decision Q-1 targets Debian trixie's PAPPL **1.3.1-2.1+b2** and requires a
table asserting that no symbol `pappl-sys` binds is newer than 1.3. This is
that table, plus the argument behind it.

## Why the table looks like this

The obvious form would be a column giving the PAPPL version that introduced
each symbol. That column cannot be filled honestly from what is installed
here: the 1.3.1 headers contain exactly **one** `@since` annotation in total —
`PAPPL_SOPTIONS_NO_TLS`, `@since PAPPL 1.1@` in `system.h` — so an
"introduced in" column would be invented for the other 48 entries, which
project rule 1 forbids in spirit and rule 2 in letter.

What can be established mechanically is stronger for the purpose the
requirement serves. The requirement exists so the binary never references
something 1.3 lacks. Two checks together guarantee exactly that:

1. **`tests/symbols.rs` asserts that every declared symbol is exported by the
   installed `libpappl.so.1`**, which *is* 1.3.1. A symbol introduced after
   1.3 would not be there, and the test would fail. This is a direct
   observation of the target library, not a claim about release history.
2. **`build.rs` refuses to build outside `>= 1.3, < 2.0`**, so the library the
   test observes is always in range.

Every declaration is additionally transcribed from a header shipped by
`libpappl-dev` 1.3.1-2.1+b2 — a symbol newer than 1.3 could not have been
copied from them in the first place.

If a 1.4-only symbol ever turns out to be genuinely necessary, decision Q-1
says stop and ask rather than raising the floor.

## The table

Verified on 2026-09-05, and again on 2026-09-12 for the three symbols decision
Q-24 added, against `libpappl-dev` / `libpappl1t64` 1.3.1-2.1+b2,
`pkg-config --modversion pappl` = 1.3.1. All 49 symbols are exported by the
installed library; `cargo test -p pappl-sys` re-checks this on every run.

| Symbol | Declared in | Exported by installed 1.3.1 |
|---|---|---|
| `papplMainloop` | `mainloop.h` | yes |
| `papplMainloopShutdown` | `mainloop.h` | yes |
| `papplSystemCreate` | `system.h` | yes |
| `papplSystemDelete` | `system.h` | yes |
| `papplSystemRun` | `system.h` | yes |
| `papplSystemShutdown` | `system.h` | yes |
| `papplSystemIsRunning` | `system.h` | yes |
| `papplSystemAddListeners` | `system.h` | yes |
| `papplSystemSetPrinterDrivers` | `system.h` | yes |
| `papplSystemSetLogLevel` | `system.h` | yes |
| `papplSystemGetLogLevel` | `system.h` | yes |
| `papplSystemLoadState` | `system.h` | yes |
| `papplSystemSaveState` | `system.h` | yes |
| `papplSystemSetSaveCallback` | `system.h` | yes |
| `papplSystemIteratePrinters` | `system.h` | yes |
| `papplSystemSetDNSSDName` | `system.h` | yes |
| `papplPrinterCreate` | `printer.h` | yes |
| `papplPrinterDelete` | `printer.h` | yes |
| `papplPrinterSetDNSSDName` | `printer.h` | yes |
| `papplPrinterSetDriverData` | `printer.h` | yes |
| `papplPrinterGetDriverData` | `printer.h` | yes |
| `papplPrinterSetReadyMedia` | `printer.h` | yes |
| `papplPrinterGetName` | `printer.h` | yes |
| `papplPrinterGetID` | `printer.h` | yes |
| `papplPrinterOpenDevice` | `printer.h` | yes |
| `papplPrinterCloseDevice` | `printer.h` | yes |
| `papplPrinterGetReasons` | `printer.h` | yes |
| `papplPrinterSetReasons` | `printer.h` | yes |
| `papplJobGetName` | `job.h` | yes |
| `papplJobGetUsername` | `job.h` | yes |
| `papplJobGetID` | `job.h` | yes |
| `papplJobGetFilename` | `job.h` | yes |
| `papplJobGetFormat` | `job.h` | yes |
| `papplJobGetImpressions` | `job.h` | yes |
| `papplJobSetImpressionsCompleted` | `job.h` | yes |
| `papplJobIsCanceled` | `job.h` | yes |
| `papplJobGetData` | `job.h` | yes |
| `papplJobSetData` | `job.h` | yes |
| `papplJobSetReasons` | `job.h` | yes |
| `papplJobGetPrinter` | `job.h` | yes |
| `papplDeviceWrite` | `device.h` | yes |
| `papplDevicePuts` | `device.h` | yes |
| `papplDeviceFlush` | `device.h` | yes |
| `papplDeviceRead` | `device.h` | yes |
| `papplDeviceGetStatus` | `device.h` | yes |
| `papplLog` | `log.h` | yes |
| `papplLogJob` | `log.h` | yes |
| `papplLogPrinter` | `log.h` | yes |
| `papplCopyString` | `base.h` | yes |

## The libcups ABI underneath PAPPL

Recorded on 2026-09-05 because PAPPL's ABI is not entirely PAPPL's own.

| What | Value |
|---|---|
| Installed PAPPL | `libpappl-dev` / `libpappl1t64` 1.3.1-2.1+b2, soname `libpappl.so.1` |
| libcups it links | `libcups.so.2` (from its `NEEDED` entries), provided by `libcups2t64` 2.4.10-3+deb13u2 |
| Headers the probe measured | `libcups2-dev` / `libcupsimage2-dev` 2.4.10-3+deb13u2 — `CUPS_VERSION_MAJOR` 2, `CUPS_VERSION_MINOR` 4, `CUPS_VERSION_PATCH` 10 |
| `sizeof(cups_page_header2_t)` measured | **1796 bytes**, 4-byte aligned |

`pappl_pr_options_t` embeds that struct **by value as its first member**, so
the CUPS raster header's layout is part of the ABI this crate compiles
against. `pappl-sys` pins it as `CUPS_PAGE_HEADER2_SIZE`, `CUPS_ABI_MAJOR` and
`CUPS_ABI_MINOR`, and `tests/layout.rs` checks all three against the probe on
every build.

Three observations decide how much protection the dependency chain gives, and
the answer is: less than it looks.

1. **The binary records no libcups dependency.** We use the struct's layout but
   call no libcups function, so the linker's `--as-needed` drops `-lcups`.
   Verified by linking a binary against this crate and reading its `NEEDED`
   entries: `libpappl.so.1`, `libgcc_s.so.1`, `libc.so.6` — no `libcups.so.2`.
   `${shlibs:Depends}` therefore names no libcups package at all, which is why
   `packaging/debian/control` states the dependency by hand.
2. **Debian's `libpappl1t64` ships a symbols file**, whose entries are of the
   form `_papplClientCreate@Base 1.0.1`. Symbols files version *symbols*; a
   struct layout change with unchanged symbols moves no entry and bumps no
   dependency.
3. **Whether `libpappl.so.1`'s soname would necessarily change** if its public
   struct layout changed underneath it could not be established from anything
   installed here. CUPS itself does treat this struct as major-version ABI —
   PAPPL's own `printer.h` selects `cups_page_header2_t` or
   `cups_page_header_t` on `#if CUPS_VERSION_MAJOR < 3`, and CUPS 3 moves to
   `libcups.so.3` — but that is CUPS's soname, not PAPPL's.

Undetermined is treated as exposed, so this is risk **R-6** in
`docs/MIGRATION-PLAN.md` rather than a documentation note. No runtime check is
possible: neither library exports its own `sizeof`, and libcups exports no
runtime version accessor — `httpGetVersion` reports the HTTP protocol version,
not the library's. The runtime backstop is the driver's own geometry
validation, which refuses a job whose option fields are implausible instead of
printing from them.

## Types and constants

The same discipline covers data, where the risk is worse: a wrong field offset
corrupts memory silently instead of failing to link. `probe/layout_probe.c`
prints the size and alignment of all 8 types this crate declares, the offset
and size of all 128 fields, the value of all 69 constants and limits, and the
CUPS version that defined the raster header; `tests/layout.rs` checks every one
of those 208 records against the Rust declarations and fails if any record is
left unchecked.

The `cups_page_header2_t` embedded in `pappl_pr_options_t` is held as opaque
storage of the probed size (1796 bytes) for now. Its individual raster fields
are a CUPS header rather than a PAPPL one and get the same treatment —
transcription plus probe entries — when the raster callbacks need them.


## P5 additions — 2026-09-06

No additional library functions were bound. The installed
`/usr/include/cups/raster.h` `cups_page_header2_t` declaration is now represented
field-by-field (49 fields), and every offset/field size is checked by the C
probe. `CUPS_CSPACE_K`, `CUPS_ORDER_CHUNKED`, `IPP_ORIENT_NONE` and
`IPP_QUALITY_NORMAL` are also checked against installed headers.

Runtime ownership and callback behaviour were cross-checked in the official
1.3.1 sources as well as exercised by `scripts/p5-probe.py`:

- [system-accessors.c](https://github.com/michaelrsweet/pappl/blob/v1.3.1/pappl/system-accessors.c):
  `papplSystemSetPrinterDrivers` retains the descriptor-array pointer. The
  wrapper now keeps that array alive throughout mainloop and system teardown.
- [job-process.c](https://github.com/michaelrsweet/pappl/blob/v1.3.1/pappl/job-process.c):
  PWG page numbering starts at 1; PAPPL counts impressions; `rwriteline_cb`
  return values are ignored while `rendpage_cb`/`rendjob_cb` failures abort.
- [mainloop-subcommands.c](https://github.com/michaelrsweet/pappl/blob/v1.3.1/pappl/mainloop-subcommands.c):
  mainloop deletes the system and reads `XDG_CONFIG_HOME` for its state path.
  The integration test scopes that directory to its temporary subprocess.
