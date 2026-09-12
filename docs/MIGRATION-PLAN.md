# ML-216x: CUPS Filter → PAPPL Printer Application — Repository Audit & Migration Plan

Current progress (2026-09-06): **P1–P6 implemented**. `spl2-core` is extracted
and the SPL2 raster callbacks are connected; the application emits real QPDL to
a `file://` destination, and no hardware transport exists yet. The maintainer
selected 12.5 pt and authorized the golden refresh. See `SESSION-STATE.md`,
`DECISIONS.md` and `MARGINS.md` for current state. The baseline audit and its
original recommendations below are historical.

Two deviations from §7's proposed layout, both deliberate: `src/spl.rs` moved
to a single `qpdl` module rather than being split into seven files, because the
split is cosmetic and the move had to be byte-for-byte safe; and the band loop
takes pushed scanlines rather than a line-source trait, because PAPPL pushes
and the filter can drive a push interface in a loop, while the reverse is not
true.

Original status: **audit only, no code changed.**
Baseline commit: `33d4ff2` (`main`, clean tree).
Baseline test run: `cargo test` — **101 passed, 0 failed**.

This document is the input to step 2. It records what exists today, what must
move where, and what is most likely to silently break. Every protocol claim
below is a citation of code already in this repository — nothing here is
inferred about the SPL2 wire format.

---

## 1. Current crate/module layout

One binary crate, `rastertospl-rust` (`Cargo.toml`; called `samsung-2160-rust`
when this audit was written), **zero third-party
dependencies** (`Cargo.lock` contains only the crate itself). Three modules,
5652 lines total, of which roughly 2947 are non-test.

| File | Total / non-test lines | Role | Purity |
|---|---|---|---|
| `src/main.rs` | 2912 / ~1174 | Binary entry, CUPS filter argv handling, page-header validation, SpliX geometry math, resource budgets, page/band pipeline, diagnostics | **Mixed.** Contains the only argv/stdin/stdout/`process::exit` code in the tree, *and* most of the pure geometry math. |
| `src/raster.rs` | 967 / ~686 | Classic CUPS Raster stream parser (v1/v2/v3, both endians), `cups_page_header{,2}_t` decode, v2 line-RLE decoder | **Pure + generic I/O.** Reads only through a `R: Read` type parameter. No env, no argv, no filesystem, no stdout, and (uniquely) **zero `eprintln!`**. |
| `src/spl.rs` | 1773 / ~1087 | SPL2 / QPDL v3 emitter: PJL envelope, 17-byte page header, Algo 0x11 RLE, band records, checksums, PJL field sanitiser | **Pure + generic I/O**, with one ambient dependency: `current_service_date()` (`spl.rs:281`) calls `SystemTime::now()`. |

Per-module detail:

### `src/raster.rs` — pure computation, CUPS-format-specific
- `CupsRasterVersion` (`:19`), `CupsColorSpace` (`:88`), `CupsColorOrder` (`:127`) — pure.
- `PageHeader` (`:152`) + `PageHeader::parse` (`:248`) — pure byte-slice decode against fixed offsets. **Two `expect()` calls** at `:227` and `:239` (`read_u32`/`read_f32`, "Hatalı dilim uzunluğu") — see §4.
- `CupsRasterReader` (`:399`) — generic over `R: Read`; `new` (`:409`), `next_page_header` (`:466`), `read_line` (`:519`).
- `CupsLineDecoder` (`:543`) — v2/PWG line-RLE, pure state machine over `R: Read`.
- **CUPS-specific, not PAPPL-ready:** the sync-word match at `:420` accepts exactly six magics — `RaSt`, `tSaR`, `RaS2`, `2SaR`, `RaS3`, `3SaR`. `PwgR` is **not** accepted, even though the v2 body encoding is documented in-file as identical to PWG Raster.

### `src/spl.rs` — pure computation + generic `Write`
- Protocol constants (`:14`–`:26`), enums `SplPaperSize` (`:31`), `SplPaperSource` (`:98`), `SplResolution` (`:137`), `SplDuplex` (`:200`), `SplCompression` (`:212`) — pure.
- `Algo0x11` (`:382`) — pure compressor; `lookup_best_offsets` (`:385`) already heap-allocates its 32 KB table specifically so the code is safe off the main thread (comment at `:385`).
- `sanitize_pjl_field` (`:766`) + `ascii_fold` (`:637`) — pure.
- `SplStreamWriter<W: Write>` (`:797`) — generic over the sink; the only impurity is `Drop` (`:1077`) writing a closing UEL.
- `current_service_date()` (`:281`) — **clock access**, the sole ambient dependency in the module.

### `src/main.rs` — mixed
Pure and directly reusable: `validate_page_header` (`:184`), `compute_page_width_pixels` (`:367`), `compute_page_height_lines` (`:383`), `hard_margin_bytes` (`:406`), `band_placement` (`:449`), `duplex_mode` (`:516`), `pjl_paper_type_for` (`:538`, modulo one `eprintln!`), `band_height_for` (`:587`), `JobBudget` (`:667`), `sanitize_copies` (`:740`), `quote_untrusted` (`:155`).

I/O and process-bound: `main` (`:60`), `CupsFilterArgs::parse` (`:38`), `process_cups_raster_to_spl` (`:749`), `stream_page_bands` (`:1043`), `print_header_info` (`:1115`).

---

## 2. Exact entry point of the current CUPS filter

**Binary:** `rastertospl-rust`, `path = "src/main.rs"`, installed to
`/usr/lib/cups/filter/rastertospl-rust`.

**How CUPS reaches it:** `ppd/samsung-ml2160.ppd` declares

```
*cupsFilter2: "application/vnd.cups-raster application/octet-stream 0 rastertospl-rust"
*cupsFilter:  "application/vnd.cups-raster 0 rastertospl-rust"
```

So the driver sits at the *end* of the chain: an upstream `cups-filters` stage
(`gstoraster`/`pdftoraster`) has already interpreted the PPD and rasterised to
CUPS Raster. The filter's output MIME type is `application/octet-stream` —
raw device data straight to the backend.

**Input format:** classic **CUPS Raster**, not PDF and not PWG-by-magic.
`CupsRasterReader::new` (`raster.rs:409`) reads a 4-byte sync word and accepts
only the six CUPS magics listed above. v1 uses a 420-byte
`cups_page_header_t`; v2/v3 use a 1796-byte `cups_page_header2_t`
(`raster.rs:57`). v2 bodies are line-RLE and are transparently decoded by
`CupsLineDecoder`; v1/v3 bodies are uncompressed and `read_line` degenerates to
`read_exact` (`raster.rs:519`).

**How it reads input:** `main` (`main.rs:60`) builds a `Box<dyn Read>`:
- if `argv[6]` (the optional `filename`) is present → `File::open` (`main.rs:115`), wrapped in `BufReader`;
- otherwise → `io::stdin()` (`main.rs:129`), wrapped in `BufReader`.

Argv is read with `env::args_os()` + lossy UTF-8 (`main.rs:68`) rather than
`env::args()`, deliberately, so a job title with invalid UTF-8 cannot panic the
filter.

**How it parses ppd/options: it does not.** This is the single most important
fact for the migration. `CupsFilterArgs` (`main.rs:28`) captures all six CUPS
filter arguments, but the doc comment at `main.rs:15–35` states that
`num_copies` (argv[4]) and `options` (argv[5]) are **intentionally not read**,
and `#[allow(dead_code)]` enforces that they stay unread. The PPD is never
opened at runtime — the only code that reads `ppd/samsung-ml2160.ppd` is the
test helper `ppd_text()` (`main.rs:1188`), which uses `CARGO_MANIFEST_DIR` to
assert that the filter's constants still cover every PPD option.

**Every printer option therefore comes from the CUPS Raster page header**, on
the stated rationale that the page *pixels* were already generated to match
that header, so the header is binding when it disagrees with argv.

**What it writes to stdout:** the raw SPL2/QPDL byte stream, via
`process_cups_raster_to_spl(&args, input_reader, io::stdout())`
(`main.rs:138`). Structure, in emission order:

1. `begin_job` (`spl.rs:820`) — UEL + PJL envelope + `@PJL ENTER LANGUAGE = QPDL`
2. per page: `begin_page` (17 bytes) → N × `write_compressed_band` → `end_page` (3 bytes)
3. `end_job` (`spl.rs:1051`) — `PJL_END` = `\t` + UEL

**Diagnostics** go to stderr — 35 `eprintln!` in `main.rs`, using the CUPS
log-level prefixes `DEBUG:`, `INFO:`, `WARNING:`, `ERROR:` and the page-progress
line `PAGE: <n> <copies>` (`main.rs:825`).

**Exit:** `process::exit(1)` on usage error (`:89`), unopenable file (`:123`),
or conversion error (`:140`). The ordering at `:138` is load-bearing and
documented: the `SplStreamWriter` lives *inside*
`process_cups_raster_to_spl`, so it is dropped — and its `Drop` writes the
closing UEL — before `main` calls `process::exit`, which does not run drops.

---

## 3. SPL2 engine surface — output-producing inventory

Everything below is in `src/spl.rs` unless noted.

### Constants (wire values)
| Item | Line | Emits |
|---|---|---|
| `PJL_UEL` = `\x1b%-12345X` | `:14` | Job-opening Universal Exit Language |
| `PJL_END` = `\t\x1b%-12345X` | `:15` | Job-closing UEL (note the leading TAB) |
| `SUBHEADER_SIG_LE` = `EF CD AB 09` | `:18` | Band sub-header signature (0x09ABCDEF, little-endian) |
| `COMPRESS_SAMPLE_RATE` 0x800, `TABLE_PTR_SIZE` 0x40, `MAX_UNCOMPRESSED_BYTES` 0x80, `MIN_COMPRESSED_BYTES` 2, `MAX_COMPRESSED_BYTES` 0x1FF+3, `COMPRESSION_FLAG` 0x80 | `:21`–`:26` | Algo 0x11 RLE encoding parameters |
| `PJL_PAPER_TYPES` (14 entries, `OFF` first) | `:229` | Vocabulary for `@PJL SET PAPERTYPE=` |
| `QPDL_BAND_HEIGHT` = 128 (`main.rs:565`) | — | Base band height in scan lines |

### Enums → single header bytes
| Item | Line | Emits |
|---|---|---|
| `SplPaperSize` (`repr(u8)`, A4=2 … Oficio=28) | `:31` | Page-header byte `0x4` |
| `SplPaperSize::from_dimensions_pt_exact` | `:66` | Maps pt geometry → paper code; returns `None` rather than falling back to A4 |
| `SplPaperSource` (Auto=1 … Lower=5) | `:98` | Page-header byte `0x9` |
| `SplPaperSource::from_media_position` | `:123` | `MediaPosition` → tray code; `0` and `1` both → `Auto` |
| `SplResolution` (300/600/1200) | `:137` | Page-header bytes `0x1` (Y) and `0x10` (X), each as `dpi/100` |
| `SplResolution::from_dpi_exact` / `pair_is_supported` | `:158` / `:171` | Exact-match only; supported pairs are 300×300, 600×600, 1200×600, 1200×1200 |
| `SplDuplex` (Simplex, Long/ShortEdge, ManualLong/ShortEdge) | `:200` | PJL `DUPLEX=`/`BINDING=` **and** page-header bytes `0xB` (duplex) + `0xC` (tumble) |
| `SplCompression` (None=0x00, Rle=0x11) | `:212` | Band-header byte `0x6`; only `Rle` is ever written |

### Config structs
- `JobConfig` (`:291`) — `job_name`, `user_name`, `service_date`, `duplex`, `paper_type: &'static str`. The `&'static str` type on `paper_type` is a deliberate injection barrier: only a `PJL_PAPER_TYPES` constant can reach the PJL line.
- `PageConfig` (`:318`) — `paper_size`, `paper_source`, `resolution_x`, `resolution_y`, `duplex`, `page_number` (**1-based**, used only for the tumble parity byte), `copies`, `width_pixels: u16`, `height_pixels: u16`, `qpdl_version` (3).

### Emitters — `SplStreamWriter<W: Write>` (`:797`)
| Function | Line | Emits |
|---|---|---|
| `new` | `:811` | nothing; sets `current_band = 0`, `job_active = false` |
| `begin_job` | `:820` | **Job header / PJL wrapper.** UEL, then `@PJL DEFAULT SERVICEDATE=`, `SET USERNAME="…"`, `SET JOBNAME="…"`, `DEFAULT POWERSAVE=ON`, `DEFAULT POWERSAVETIME=5`, `SET JAMRECOVERY=OFF`, `SET DUPLEX=OFF|ON|MANUAL` (+ `SET BINDING=LONGEDGE|SHORTEDGE` when duplex), `SET PAPERTYPE=<type>`, `SET ALTITUDE=LOW`, `SET DENSITY=3`, `SET RET=NORMAL`, `@PJL ENTER LANGUAGE = QPDL`. Flushes, sets `job_active = true`. |
| `begin_page` | `:902` | **17-byte QPDL page header.** `0x0`=00 sig, `0x1`=Y dpi/100, `0x2..0x4`=copies BE, `0x4`=paper size, `0x5..0x7`=width px BE, `0x7..0x9`=height px BE, `0x9`=paper source, `0xA`=00, `0xB`=duplex byte, `0xC`=tumble byte (`page_number % 2`), `0xD`=00, `0xE`=QPDL version 3, `0xF`=01 colorplanes, `0x10`=X dpi/100. Also resets `current_band = 0`. |
| `write_compressed_band` | `:950` | **Band data + compression.** 11-byte record `0x0C`: `[0x0]`=0x0C, `[0x1]`=band index (u8), `[0x2..0x4]`=band width px BE, `[0x4..0x6]`=band height lines BE, `[0x6]`=0x11, `[0x7..0xB]`=total data size BE (payload+8). Then the 4-byte sub-header signature, the Algo 0x11 payload, and a 4-byte BE checksum over signature+payload. |
| `end_page` | `:1032` | **End-of-page.** 3 bytes: `[0x01, copies_msb, copies_lsb]`, then flush. |
| `end_job` | `:1051` | **End-of-job.** `PJL_END`, then flush. Idempotent — no-op if `!job_active`. |
| `Drop` | `:1077` | Safety net: calls `end_job()` if the job is still active, so an aborted job cannot leave the printer stuck in QPDL. |

### Compression
| Function | Line | Role |
|---|---|---|
| `Algo0x11::lookup_best_offsets` | `:385` | Builds the 64-entry back-reference offset table written at payload bytes 4..132 |
| `Algo0x11::compress` | `:431` | Full Algo 0x11 payload: 4-byte LE initial-literal size, 64 × u16 LE offsets, initial literals, then literal-run / match records |
| `Algo0x11::calculate_checksum` | `:550` | Wrapping u32 byte sum |
| `Algo0x11::decompress` | `:561` | **Test-only** (`#[cfg(test)]`) inverse, for round-trip assertions |

### Support
- `pjl_paper_type` (`:251`) — free-text media type → `&'static str` from the table, ASCII-case-insensitive.
- `sanitize_pjl_field` (`:766`) — printable-ASCII whitelist, no double quotes, Latin-1/Turkish folding, hard cap `MAX_PJL_FIELD_BYTES` = 128 **bytes** (`:615`).
- `service_date_from_unix_days` (`:262`) / `current_service_date` (`:281`) — `YYYYMMDD` for `@PJL DEFAULT SERVICEDATE`.

### Geometry that shapes the output (in `main.rs`)
- `compute_page_width_pixels` (`:367`) — SpliX `((ceil(pt*dpi/72)) + 7) & ~7`
- `compute_page_height_lines` (`:383`) — vertical equivalent, **no 8-alignment**, uses `hw_resolution[1]`
- `hard_margin_bytes` (now `crates/spl2-core/src/geometry.rs`) — SpliX `hardMarginXInB`; 12.5 pt @ 600 dpi → 105 px → 112 aligned → 14 bytes
- `band_placement` (`:449`) — centring minus hard margin, as a signed offset split into `dst_offset`/`src_skip`
- `band_height_for` (`:587`) — 128, halved to 64 **only** when both axes are 300 dpi
- `stream_page_bands` (`:1043`) — column-major (transposed) band fill `band[col * band_height + y]`, then **unconditional bitwise inversion** of every byte (CUPS K 1=black vs Samsung 0=black)

---

## 4. Hidden global/mutable state and per-process assumptions

**Good news first.** A grep for `static mut`, `unsafe`, `lazy_static`,
`OnceLock`, `thread_local`, `Atomic*`, and `Mutex` across `src/` returns
**nothing**. There is no global mutable state at all. All mutable state is
instance-local:

- `CupsRasterReader { page_count, decoder }` — per stream, and `decoder` is correctly reset per page (`raster.rs:495–500`).
- `CupsLineDecoder { repeat_remaining, last_line }` — per page; the reset is explicitly justified so v2 repeat counts cannot leak across pages.
- `SplStreamWriter { current_band, job_active }` — per job; `current_band` is reset in `begin_page`.
- `JobBudget { pages, raster_bytes, impressions }` — per job, constructed fresh in `process_cups_raster_to_spl`.

So the engine is already structurally re-entrant. What breaks in a long-lived,
multi-threaded daemon is everything *around* it:

| # | Assumption | Where | Why it breaks under PAPPL |
|---|---|---|---|
| G-1 | **`process::exit` is an acceptable error path** | `main.rs:89, 123, 140` | In `pappld` this kills the daemon and every concurrent job. Must become a `bool`/status return from the print callback. |
| G-2 | **`panic!` is survivable** — two `expect()` calls | `raster.rs:227, 239` | These are the only panic sites reachable from the conversion path. They are unreachable today (`PageHeader::parse` bounds-checks `buf.len() >= expected_size` at `raster.rs:252` before any read), but ground rule 5 forbids them on a C-callback path, and unwinding out of `extern "C"` is UB. Must become `io::Error`, plus a `catch_unwind` belt at the FFI boundary. |
| G-3 | **stderr is the log, and prefixes carry the level** | 35 `eprintln!` in `main.rs`, `spl.rs:1701`/`:1752` (tests only) | A daemon has one stderr shared by all jobs; `DEBUG:`/`PAGE:` prefixes mean nothing to PAPPL. Every line must become `papplLogJob(job, PAPPL_LOGLEVEL_*, …)` so output is correlated per job. `PAGE: n copies` specifically must become PAPPL's page-progress call, or job accounting silently reports zero pages. |
| G-4 | **`Drop` runs before the process ends** | `spl.rs:1077`, contract documented at `main.rs:130–137` | Under PAPPL there is no `exit`, so the drop always runs — this actually gets *safer*. But if a panic ever unwinds, `Drop` runs during unwind and writes to a `W` that may itself be an FFI device handle. The unwind must be caught *inside* Rust, below the `extern "C"` frame. |
| G-5 | **stdout is the device** | `main.rs:138` | Must become a PAPPL device handle. `SplStreamWriter` is already generic over `W: Write`, so this is an adapter, not a rewrite — but flush semantics differ (`end_page`/`end_job` flush; a PAPPL device may need an explicit write-through). |
| G-6 | **The clock is free to read per job** | `spl.rs:281` | Thread-safe and re-entrant, so no correctness problem, but it makes `begin_job` non-deterministic. Golden-file testing (see §9) requires an injectable date. |
| G-7 | **Per-job budgets are the only resource ceiling** | `main.rs:611, 644, 658` | `MAX_PAGES_PER_JOB` = 1000, `MAX_JOB_RASTER_BYTES` = 8 GiB, `MAX_JOB_IMPRESSIONS` = 10000 are correctly per-`JobBudget`. But a daemon runs N jobs at once, so peak memory is N × per-job peak. `stream_page_bands` allocates `band_data = bw_bytes * band_height` — up to roughly 22 MB at the largest accepted geometry — plus a `line_buffer`, per concurrent job. There is no daemon-wide cap today because there was no daemon. |
| G-8 | **Stack size is the main thread's 8 MB** | already mitigated at `spl.rs:385` | The 32 KB offset table was deliberately moved to the heap for exactly this reason. Worth keeping in mind for any new large locals; PAPPL worker threads get the default 2 MB. |
| G-9 | **argv carries `title`/`user`** | `main.rs:38`, `:749` | These feed `@PJL SET JOBNAME`/`USERNAME`. Under PAPPL they come from IPP job attributes instead; the sanitiser stays, the source changes. |
| G-10 | **Job-level options can be peeked from page 1** | `main.rs:~770–790` | Duplex and paper type are read from the *first page header* because PJL needs them before any page. PAPPL delivers job attributes up front, so this peek can and should go away — but see risk R-5. |

---

## 5. Every place that touches env, argv, stdin/stdout, /tmp, or the filesystem

**Environment variables: none.** No `env::var` anywhere in the tree.
**`/tmp`: never touched.** No temp files, no temp dirs, at any point.

| Kind | Location | Detail |
|---|---|---|
| argv | `main.rs:68` | `env::args_os()` → lossy UTF-8 |
| argv | `main.rs:38` | `CupsFilterArgs::parse` — 6-arg CUPS form, or a single bare filename ("direct file mode") |
| argv | `main.rs:85–89` | usage message + `exit(1)` when argc ≤ 1 |
| filesystem (read) | `main.rs:115` | `File::open(argv[6])` when a filename is given |
| stdin | `main.rs:129` | `io::stdin()` when no filename |
| stdout | `main.rs:138` | `io::stdout()` passed as the `W` of `SplStreamWriter` |
| stderr | `main.rs` ×35 | diagnostics; `raster.rs` has **zero** |
| process | `main.rs:89, 123, 140` | `process::exit(1)` |
| clock | `spl.rs:281` | `SystemTime::now()` in `current_service_date()` |
| filesystem (test only) | `main.rs:1189` | `ppd_text()` reads `$CARGO_MANIFEST_DIR/ppd/samsung-ml2160.ppd` to assert constants match the PPD |
| filesystem (test only) | `spl.rs:1698` | `test_algo0x11_roundtrip_real_band0` opens `target/test_output/test.raster`, **skipping cleanly** if absent |
| stderr (test only) | `spl.rs:1701, 1752` | skip/diagnostic notices |

Net: after the binary's `main` is deleted, **`raster.rs` and `spl.rs` have no
ambient dependencies except the clock at `spl.rs:281`.** That is a very clean
starting point.

---

## 6. Printer options supported today, and where each is read

Every one is read from the CUPS Raster page header. The PPD column shows the
option the user actually picks; the offset column is the byte offset decoded by
`PageHeader::parse`.

| Option | PPD option | Raster field (offset) | Consumed by | Effect on output |
|---|---|---|---|---|
| **Resolution** | `*Resolution` — 300dpi, 600dpi (default), 1200x600dpi, 1200dpi | `HWResolution[0]` @276, `[1]` @280 | `validate_page_header` (`main.rs:203`), `SplResolution::from_dpi_exact` | Page-header `0x1` = Y/100, `0x10` = X/100. Also drives band width, hard margin, and band height. |
| **Media size** | `*PageSize` / `*PaperDimension` — A4 (default), Letter, Legal, Executive, A5, A6, B5, Env10, EnvDL, EnvC5, Folio | `PageSize[0]` @352, `[1]` @356 | `SplPaperSize::from_dimensions_pt_exact` (`spl.rs:66`) | Page-header `0x4`. Unknown geometry is **rejected**, never coerced to A4. |
| **Media type** | `*MediaType` — OFF (default), NORMAL, THICK, THIN, BOND, OHP, CARD, LABEL, USED, COLOR, ENV, COTTON, RECYCLED, ARCHIVE | `MediaType` C-string @128..192 | `pjl_paper_type_for` (`main.rs:538`) → `pjl_paper_type` (`spl.rs:251`) | `@PJL SET PAPERTYPE=<key>`. Read from **page 1 only** — it is a job-level PJL setting. Unknown → `OFF` + a warning. |
| **Paper source** | `*InputSlot` — Auto (default), Manual | `MediaPosition` @324 | `SplPaperSource::from_media_position` (`spl.rs:123`) | Page-header `0x9`. `0`/`1` → Auto; unknown → Auto + warning. |
| **Copies** | (no PPD option; comes from the job) | `NumCopies` @340 | `sanitize_copies` (`main.rs:740`) | Page-header `0x2..0x4` **and** the end-of-page footer. Clamped to `1..=999` (`MAX_REALISTIC_COPIES`). |
| **Duplex / binding edge** | **no `*OpenUI *Duplex` block in the PPD** | `Duplex` @272, `Tumble` @368 | `duplex_mode` (`main.rs:516`) | `@PJL SET DUPLEX=OFF\|ON\|MANUAL` + `BINDING=`, page-header `0xB`/`0xC`. **Currently unreachable** — with no PPD option, `Duplex` is never set, so every job is Simplex. The mapping is kept ready; `main.rs:509` and `main.rs:968` document two unfinished pieces (manual-duplex tray override needs last-page knowledge; two-pass page ordering is not implemented). |
| **Hard margin** | the driver constant `spl2_core::media::HARD_MARGIN_PT` (12.5 pt since 2026-09-06; `*ImageableArea` mirrors it) | not read from the header any more | `hard_margin_bytes`, `hard_margin_lines`, `band_placement` (`crates/spl2-core/src/geometry.rs`) | Placement of content within the band, on both axes. Not user-selectable, but user-visible if wrong. |
| **Colour space / depth** | `*ColorDevice: False` | `cupsColorSpace` @400, `BitsPerColor` @384, `BitsPerPixel` @388, `cupsColorOrder` @396 | `validate_page_header` | Not an option — anything other than 1-bit K is **rejected**. |
| `cupsCompression` | — | @404 | ignored (`main.rs:839`) | Deliberately ignored with a one-shot warning; band compression is always Algo 0x11. |

**Not supported today — flagged because a Printer Application UI will expect them:**
- **Toner save / economode** — no PPD option, no code path. `@PJL SET DENSITY=3` is hardcoded (`spl.rs:888`).
- Also hardcoded in `begin_job`, with no way to change them: `POWERSAVE=ON`, `POWERSAVETIME=5`, `JAMRECOVERY=OFF`, `ALTITUDE=LOW`, `RET=NORMAL`.
- Hardcoded in `begin_page`: `qpdl_version = 3` (`main.rs:996`), colorplanes = `0x01`, `unknownByte1`/`unknownByte2` = 0.
- Collate (`@264`), orientation (`@344`), `output_face_up` (`@348`), `mirror_print`, `negative_print` — parsed into `PageHeader` but never used.
- PPD driver attributes `*QPDL PacketSize: "512"` and `*General DocHeaderValues: "<0><0><1>"` are declared but **not read by any code**.

---

## 7. Proposed target workspace layout

```
ml216x-printer-app/             # virtual workspace root
├── Cargo.toml                  # [workspace] members = [...]
├── crates/
│   ├── spl2-core/              # GPL-2.0-only  — no C, no I/O, no deps
│   ├── pappl-sys/              # raw FFI to libpappl
│   ├── pappl/                  # safe wrapper over pappl-sys
│   └── ml216x-printer-app/     # GPL-2.0-only  — the binary
├── ppd/                        # kept for the frozen 1.x queue; not used by 2.0
├── packaging/debian/
└── docs/
```

### `spl2-core` — the protocol engine, zero I/O
**Moves here, essentially unchanged (this is the byte-for-byte-critical code):**
- All of `src/spl.rs` → `spl2-core/src/qpdl/` (suggested split: `consts.rs`, `paper.rs`, `resolution.rs`, `duplex.rs`, `pjl.rs`, `compress.rs`, `writer.rs`).
- All of `src/raster.rs` → `spl2-core/src/raster.rs`, **kept as-is** for now. Even though PAPPL will hand us PWG Raster, this parser already implements the exact v2 line-RLE that PWG uses; keeping it lets the golden-file harness replay 1.x inputs. Whether it stays in the shipping path is open question Q-6.
- From `src/main.rs`, the pure half: `validate_page_header`, `compute_page_width_pixels`, `compute_page_height_lines`, `hard_margin_bytes`, `BandPlacement`/`band_placement`, `duplex_mode`, `pjl_paper_type_for`, `band_height_for`, `sanitize_copies`, `JobBudget`, the `MAX_*` constants, `QPDL_BAND_HEIGHT`, and the band loop from `stream_page_bands` — the last refactored to take a line-source trait instead of a `CupsRasterReader`, so PAPPL's raster callbacks can feed it.

**Two changes are unavoidable here:**
1. `current_service_date()` becomes injectable (a `service_date: String` supplied by the caller, with the current function kept as the default), so golden files are reproducible.
2. Every `eprintln!` that survives into this crate becomes a diagnostic **returned or emitted through a `Log` trait**, never written to a global stream. Cleanest: `spl2-core` takes a `&dyn Fn(Level, &str)` or a small `Diagnostics` trait object.

**Constraint:** `spl2-core` must stay `#![forbid(unsafe_code)]` and dependency-free. It must never depend on `pappl-sys`.

### `pappl-sys` — raw FFI
- **Nothing moves here.** All new code.
- Hand-written `extern "C"` declarations, each one quoting the real signature from the installed `pappl/pappl.h` (ground rule 2). Bindgen is an option but adds a build-time dependency and a licensing question (see §8) — recommendation is hand-written and reviewable, given how small the needed surface is (`papplMainloop`/`papplSystemCreate`, `papplPrinterCreate`, driver-callback registration, `papplJob*`, `papplDevice*`, `papplLog*`, and the raster callback struct).
- `build.rs` links `pappl` via pkg-config.
- No safety, no abstraction — declarations and `#[repr(C)]` types only.

### `pappl` — the safe wrapper
- **Nothing moves here.** All new code.
- Owns the entire `unsafe` surface: RAII handles, `&CStr` conversions, and — critically — **the `catch_unwind` shim that every `extern "C"` callback goes through**, so no panic can cross into C (ground rule 5).
- Converts Rust `Result` into whatever return convention each PAPPL callback uses.

### `ml216x-printer-app` — the binary
- **Moves here:** the orchestration half of `src/main.rs` — the job/page loop from `process_cups_raster_to_spl` (rewritten against PAPPL's callbacks), `print_header_info` (rewritten as `papplLogJob` calls), and `quote_untrusted`.
- **Deleted:** `CupsFilterArgs` and its parse, `main`'s argv/stdin/stdout plumbing, and all three `process::exit` calls.
- **New:** the driver-capability table that replaces the PPD (media sizes, resolutions, input slots, media types — mechanically derivable from `ppd/samsung-ml2160.ppd`, which is why the existing PPD-vs-constants tests are worth porting), device discovery, and the `papplMainloop` entry.
- **New, and required before the raster callbacks are considered done:** the
  option-struct sanity check that R-6 calls for. `rstartjob`/`rstartpage`
  receive `pappl_pr_options_t`, whose first member is a CUPS-owned struct that
  no dependency in the package pins; the fields we read after it — `copies`,
  `printer_resolution`, `media.size_width`/`size_length`, `media.size_name` —
  must be validated on every job and an implausible value must **fail the job
  with a specific error, never be clamped or defaulted**. See requirement
  R-6/H in §9. This is also where the per-page geometry validation from the
  1.x `validate_page_header` lands, so the two checks are one piece of work.
- **Implemented provisionally in P5:** the hard-margin table uses the
  maintainer-selected 12.5 pt (441 IPP units). Physical model-specific
  validation remains required — see `docs/MARGINS.md` and release gate G-1. PAPPL's
  driver data can express per-model margins; the classic PPD could not, so if
  the models differ this step is where that is expressed.

### Test strategy across the split
The 101 existing tests move with their code. The PPD-reading tests
(`test_limits_cover_every_ppd_option`, `test_every_ppd_paper_size_maps_to_its_qpdl_code`,
`test_every_ppd_input_slot_maps_to_its_qpdl_code`,
`test_every_ppd_media_type_is_accepted_by_the_filter`,
`test_band_height_matches_splix_rule_for_every_ppd_resolution`,
`test_ppd_defaults_match_filter_defaults`) currently bind constants to the PPD
file; they must be re-pointed at the new driver-capability table, or the PPD
becomes an unmaintained second source of truth.

---

## 8. Licensing

### Current state (verified, not changed)
| Artifact | Declared license | Evidence |
|---|---|---|
| Crate metadata | `GPL-2.0-only` | `Cargo.toml` |
| `LICENSE` | GNU GPL **version 2**, full text (339 lines) | `LICENSE` |
| `src/spl.rs` | "Lisans: GPLv2 (yalnızca v2 — SpliX kaynağıyla aynı)" | `spl.rs:9` |
| `packaging/debian/copyright` | DEP-5, `Files: *` → `License: GPL-2` | rationale given: derived from GPLv2 OpenPrinting SpliX |
| `ppd/samsung-ml2160.ppd` | **no licence header at all** | inspected — this is a gap |
| Third-party crates | **none** | `Cargo.lock` contains only this crate |

The GPL-2.0-**only** choice is not incidental: `spl.rs` states the SPL2/QPDL
implementation is derived from SpliX, which is GPLv2-only, so this project
cannot relicense to GPL-2.0-or-later or anything permissive.

### PAPPL's licence
PAPPL is Apache-2.0 **with an exception permitting linking to GPL2/LGPL2
software**. That exception is what makes this migration legal at all: bare
Apache-2.0 and GPL-2.0-only are incompatible (Apache-2.0's patent-termination
and notice clauses are "further restrictions" under GPLv2 §6). **I have not
been able to read the exception text** — PAPPL is not installed on this machine
(see Q-1), so this paragraph is from general knowledge, not from a file I
verified. Before any FFI code is written, the actual `LICENSE`/`NOTICE` shipped
with the PAPPL we build against must be read and its exact exception wording
quoted into `debian/copyright`.

### Proposed per-crate licensing

| Crate | Proposal | Reasoning |
|---|---|---|
| `spl2-core` | **GPL-2.0-only** (unchanged) | Derived from SpliX. No choice here. |
| `ml216x-printer-app` | **GPL-2.0-only** (unchanged) | Links `spl2-core`. |
| `pappl-sys` | **Apache-2.0** — *not* MIT, *not* dual | Declarations transcribed from Apache-2.0 headers are arguably derivative of them. Matching upstream's licence is the conservative choice and keeps the crate reusable by anyone. |
| `pappl` | **Apache-2.0 OR MIT** — *proposed, needs your decision* | The safe wrapper is original work and permissive licensing maximises reuse. But its API shape follows PAPPL's, so if you'd rather be conservative, make it plain Apache-2.0 to match `pappl-sys`. |

**Superseded by the decision actually taken:** both crates are `MIT` only.
They were `Apache-2.0 OR MIT` from 2026-09-05 to 2026-09-10; the table above is
the proposal, not the outcome. See `docs/DECISIONS.md`, Q-8a.

Making `pappl-sys`/`pappl` permissive is safe in the other direction:
GPL-2.0-only code may link permissively licensed crates. The exception is only
needed for the GPL ↔ Apache direction, i.e. for libpappl itself.

### Concerns to flag (no header touched)
1. **`ppd/samsung-ml2160.ppd` has no licence header.** It is covered by
   `Files: *` in `debian/copyright`, but as a file that is installed
   standalone it should carry an explicit GPL-2 header. Not changing it — your call.
2. **The current .deb links statically against musl** (`README.md`,
   `packaging/debian/control`: *"statically linked against musl"*). If the
   2.0 package statically links libpappl, the binary *contains* Apache-2.0
   code and `debian/copyright` must carry the full Apache-2.0 text plus the
   exception, and the whole arrangement rests entirely on that exception.
   **Recommendation: link libpappl dynamically** (`Depends: libpappl1t64`) for
   2.0. This is the normal Debian arrangement, keeps the licensing argument
   short, and removes musl from the picture. Needs your decision (Q-7).
3. **`Depends: cups (>= 2.4), cups-filters` is wrong for 2.0.** A Printer
   Application does not need `cups-filters`. The control file will change.
4. **Zero third-party crates is a licensing asset.** Every new dependency adds a
   `debian/copyright` stanza and a compatibility check. `pappl-sys` needs
   nothing but `libc` if we hand-write the declarations; bindgen would pull in a
   large tree. This is another argument for hand-written FFI.

### `debian/copyright` licence inventory — started now

| # | Covers | Licence | Status |
|---|---|---|---|
| 1 | `src/**` → `crates/spl2-core/**`, `crates/ml216x-printer-app/**` | GPL-2.0-only, © Çağan ERGÜN; derived from OpenPrinting SpliX (GPLv2) | present; SpliX attribution should be made explicit per-file |
| 2 | `ppd/samsung-ml2160.ppd` | GPL-2.0-only (implied) | **no header — needs a decision** |
| 3 | `README.md`, `docs/**`, `packaging/**` | GPL-2.0-only via `Files: *` | present |
| 4 | `LICENSE` | GPL-2 text, © FSF | present |
| 5 | `crates/pappl-sys/**` | MIT (Apache-2.0 was proposed) | present |
| 6 | `crates/pappl/**` | MIT (Apache-2.0 OR MIT was proposed) | present |
| 7 | libpappl itself | Apache-2.0 + GPL2/LGPL2 linking exception | **to add — exact text must be read from the installed package (Q-1)** |
| 8 | Any new Cargo dependency | TBD | one stanza each; keep the count at zero if possible |

---

## 9. Risk list — top 5 ways this migration silently corrupts output

Ranked by *silence*: how likely a wrong result is to look plausible and reach paper.

### R-1 — The hard margin disappears (or doubles) under PWG Raster
`hard_margin_bytes` (`main.rs:406`) reads `Margins[0]` @312 of the classic CUPS
header, which cups-filters populates from the PPD's `*ImageableArea` left
margin (12 pt). PAPPL's raster header is a different structure with a different
field set, and PAPPL applies its own margin handling from the driver data. If
`Margins[0]` arrives as 0, `hard_margin_bytes` returns 0, `band_placement`
returns pure centring — and every page shifts right by 13 bytes = 104 px ≈ 12 pt
≈ 4 mm, pushing the right edge outside the printable area. **This is the
regression the code already fixed once** (the "D-06" comment at `main.rs:427`
documents exactly this bug and its 4 mm symptom). It produces a page that looks
normal until you measure it.
*Resolved by decision D-2 (see Decisions): the margin is never read from a
raster header in either path. It is a driver constant declared from the table
in `src/golden.rs` / the driver-capability table, and cross-checked at page
start; a zero or absent margin fails the job instead of falling back.*
*Mitigation:* the `*-marks` golden cases pin the exact band-buffer column of a
one-pixel corner mark, so a one-byte placement shift changes the compressed
payload and breaks the comparison at byte level rather than at ruler level.
The existing tests `test_hard_margin_matches_splix_alignment`,
`test_band_placement_subtracts_hard_margin`,
`test_content_lands_at_hard_margin_corrected_column` and
`test_horizontal_and_vertical_origins_agree` must be ported and kept green.

### R-2 — The transpose or the polarity inversion is disturbed
`stream_page_bands` (`main.rs:1043`) does two things that are invisible in a
diff and catastrophic on paper: it fills the band **column-major**
(`band_data[col * band_height + y]`, because SpliX's `Algo0x11::reverseLineColumn()`
is true), and it then inverts **every byte** of the band unconditionally
(CUPS K: 1 = black; Samsung: 0 = black). Two specific failure modes:
- Refactoring the band buffer to be reused without `band_data.fill(0)` at the
  top of each band leaves the previous band's tail in the final partial band.
- Moving the inversion before the zero-fill, or zero-filling *after* inverting,
  turns the final partial band's padding from white (0xFF after inversion) into
  solid black — a black bar at the bottom of the last page of every job.
*Mitigation:* the compressed payload of band 0 for a fixed input is the tightest
possible golden value. `test_algo0x11_roundtrip_real_band0` already does this
against a real captured raster; make that fixture a committed test asset rather
than an optional skip.

### R-3 — Band height silently reverts to 128 at 300 dpi
`band_height_for` (`main.rs:587`) returns 64 **only** when *both* axes are 300,
and 128 otherwise — including the asymmetric 1200×600 mode. This one value feeds
three places at once: the band buffer size, the transpose stride
(`col * band_height + y`), and the height field written into the band record.
Get it wrong and the printer decodes correctly-compressed data at the wrong
stride: a plausible-looking but scrambled page, only at 300 dpi, which is the
resolution nobody tests with.
*Mitigation:* port `test_band_height_is_halved_only_at_300x300`,
`test_300dpi_job_writes_64_line_band_records`,
`test_600dpi_job_still_writes_128_line_band_records`,
`test_asymmetric_1200x600_keeps_128_line_bands`.

### R-4 — The resolution axes get swapped again
In the 17-byte page header, `0x1` carries **Y** and `0x10` carries **X**
(`spl.rs:907, 940`). The order is counter-intuitive, and the code comment at
`spl.rs:318–334` records that this was already wrong once: a single `resolution`
field wrote the horizontal value into both bytes, which is invisible in every
symmetric mode (300/600/1200) and squashes the page 2× vertically **only** in
1200×600. Any struct reshuffle during the move to `spl2-core` can reintroduce it,
and three of the four supported modes will not notice.
*Mitigation:* `test_begin_page_maps_resolution_axes_to_correct_bytes` and
`test_validate_page_header_accepts_asymmetric_resolution` are the guard; keep both.

### R-5 — Job-level PJL is rebuilt from a different source and changes order or content
`begin_job` must emit duplex and paper type *before* `@PJL ENTER LANGUAGE = QPDL`,
so today the code peeks the first page header before starting the job
(`main.rs:773–790`). Under PAPPL, job attributes arrive up front, which is
better — but three things can change silently:
- **Ordering.** `begin_job` writes 13 PJL lines in an exact SpliX-derived order
  (`spl.rs:827–890`). Rebuilding this from an IPP attribute map will not preserve
  order unless it is explicitly hard-coded. Byte-for-byte fails immediately;
  worse, a *partially* reordered header may still print, masking the change.
- **`SERVICEDATE`.** `current_service_date()` makes `begin_job` output
  date-dependent — so a naive byte-for-byte comparison fails every midnight and
  will be "fixed" by loosening the comparison, which then hides real diffs.
  Inject the date instead (§7).
- **Copies counted twice.** `copies` is written into the page header
  (`0x2..0x4`) *and* the end-of-page footer, from one `sanitize_copies` call.
  PAPPL may also implement copies itself by replaying pages. If both happen,
  the job prints copies², silently, and only for multi-copy jobs.

### R-6 — libcups changes `cups_page_header2_t` and nothing tells us
`pappl_pr_options_t` embeds `cups_page_header2_t` **by value as its first
member** (`printer.h`), so that struct's layout is part of PAPPL's ABI even
though it belongs to CUPS. If a libcups release changed it, every field after
it — `copies`, `media`, `printer_resolution`, `sides` — would move.

The reason this is a real exposure rather than a theoretical one is that
**nothing in the link chain would notice**:

* We reference no libcups *symbol*: we only use the struct's layout. The
  linker's default `--as-needed` therefore drops `-lcups` entirely. Verified
  by building a binary against `pappl-sys` and reading its `NEEDED` entries —
  `libpappl.so.1`, `libgcc_s`, `libc`, and no `libcups.so.2`. So
  `${shlibs:Depends}` records no libcups dependency at all.
* Debian's `libpappl1t64` ships a **symbols** file (`@Base 1.0.1` style
  entries), which versions *symbols*. A struct layout change with unchanged
  symbols produces no dependency bump.
* `libpappl.so.1`'s soname is PAPPL's own. Whether it would necessarily change
  if PAPPL's public struct layout changed underneath it could not be
  established from anything installed here, so per the rule for undetermined
  ABI questions this is treated as exposed rather than covered.

*Mitigation, in three layers:*
1. **Build time.** `probe/layout_probe.c` reports `CUPS_VERSION_MAJOR/MINOR`
   and the measured struct size; `tests/layout.rs` asserts both against
   `CUPS_ABI_MAJOR`/`CUPS_ABI_MINOR`/`CUPS_PAGE_HEADER2_SIZE` in `pappl-sys`.
   A rebuild against a CUPS whose raster header moved fails the test.
2. **Package time.** `packaging/debian/control` carries an explicit
   `libcups2-dev` build-dependency and a versioned `libcups2t64` runtime
   dependency, because the automatic machinery cannot derive either.
3. **Run time.** No introspection is possible — neither PAPPL nor libcups
   exports its own `sizeof`, and libcups exports no runtime version accessor
   (`httpGetVersion` reports the HTTP protocol version, not the library's).
   The backstop is therefore the geometry validation the driver performs on
   every page: a shifted layout turns `copies`, `printer_resolution` and the
   media dimensions into implausible values, and the D-01/D-02 checks refuse
   the job instead of printing from them.

   > **Requirement R-6/H — the backstop is a hard error, never a clamp.**
   > When the raster callbacks first read `pappl_pr_options_t`, an
   > implausible field must **fail the job** with a specific message naming
   > the field and its value. It must not be clamped, defaulted, rounded into
   > range, or "corrected" to the nearest sensible value.
   >
   > This is written down because it is exactly the kind of requirement that
   > erodes: months from now someone hits a printer or a client that produces
   > an odd but harmless value, the job fails, and the obvious fix is a clamp.
   > A clamp would not merely weaken this check — it would **delete the only
   > runtime signal** that the libcups ABI moved underneath us, because a
   > shifted struct produces exactly the "odd value" that a clamp is designed
   > to absorb. The job that gets refused is the evidence.
   >
   > If a legitimate value is being rejected, the fix is to widen the
   > validated range deliberately, with the reason recorded — not to make
   > the failure path forgiving. Same rule the margin table already follows:
   > a page that looks fine until measured is worse than a refused job.

*Status:* layers 1 and 2 are in place. Layer 3 lands with the raster
callbacks, when the option fields are first read.

**Honourable mentions** (real, but louder when they break): the 1-based
`page_number` feeding the tumble parity byte (`spl.rs:933`); the `u8` band-index
ceiling at 256 bands per page (`spl.rs:986`); `PJL_END`'s leading TAB
(`spl.rs:15`); and `Drop`-based UEL emission interacting with `catch_unwind`
at the FFI boundary.

### Recommended step 0, before any PAPPL code
Build a golden-file harness **on the 1.x code as it stands today**: a handful of
committed CUPS Raster inputs (A4/Letter/A5 × 300/600/1200/1200×600, simplex,
1 and 3 copies, plus one envelope) and their exact SPL2 output bytes, with
`SERVICEDATE` pinned. Ground rule 6 says byte-for-byte identity is the
acceptance criterion; that criterion needs artefacts to compare against, and
they can only be captured before the code moves. Every risk above is caught by
this harness except R-1, which additionally needs a real check of what PAPPL
actually supplies as the margin.

---

---

## Decisions (answers received 2026-09-05)

These supersede the corresponding open questions. Recorded here so the
reasoning survives into the workspace.

### D-1 — Target Debian trixie's PAPPL 1.3.1; never vendor or static-link it
**Answered: Q-1.** Build against the `libpappl-dev` the archive actually ships
(1.3.1-2.1+b2). Do **not** build 1.4.x from source, do **not** vendor libpappl,
do **not** link it statically.

> **Amended by decision Q-27 (2026-09-12), on the maintainer's instruction.**
> Upstream 1.4.12 is now a second supported target and *is* built from source,
> by `scripts/install-pappl-1.4.sh`, because no Debian suite packages it —
> stable, testing and unstable all carry 1.3.1-2.1. What it buys is the two
> memory-corruption fixes recorded as S-1 and S-2 in
> `docs/SECURITY-REVIEW.md`. The archive's 1.3.1 remains supported and remains
> what the `.deb` depends on, and the two prohibitions this entry ends with —
> never vendor, never static-link — are untouched. See Q-27 in
> `docs/DECISIONS.md` for the measurements behind it.

Reasoning to keep encoded here:
- Nothing 1.4 added is needed by a monochrome raster driver. Upstream's own
  release notes list 1.4.0's additions as `job-retain-until`,
  `PAPPL-Create-Printers`, the device-type removal APIs, job suspend/resume at
  copy boundaries, server config files, and paused-state persistence. The
  raster callbacks, driver data, device API and mainloop have been stable since
  1.0.
- Vendoring a C library earns the lintian `embedded-library` tag, removes the
  package from apt security updates, and makes us the CVE response path.
- Linking the archive's library gives `${shlibs:Depends}` for free.

Consequences already applied to this plan:
- **Version guard: accept PAPPL `>= 1.3` and `< 2.0`** (not "major must be 1").
- `pappl-sys` must bind only symbols present in the 1.3.1 headers. After
  bindings are generated, produce a table of every bound symbol against the
  PAPPL version that introduced it and assert none is newer than 1.3. If a
  1.4-only symbol turns out to be genuinely necessary — stop and ask.
- `packaging/debian/control` loses `cups-filters` and gains `libpappl1t64`
  via `${shlibs:Depends}`; the musl static build goes away.

**pkg-config vs pappl-config:** `libpappl-dev` on trixie installs
`/usr/lib/x86_64-linux-gnu/pkgconfig/pappl.pc` and ships **no** `pappl-config`
script. So **pkg-config is the primary path in `build.rs`**, and there is no
`pappl-config` fallback to write. Headers land in `/usr/include/pappl/`:
`base.h`, `client.h`, `device.h`, `job.h`, `loc.h`, `log.h`, `mainloop.h`,
`pappl.h`, `printer.h`, `subscription.h`, `system.h`.

### D-2 — The hard margin is a driver constant, never a raster-header field
**Answered: Q-2.** `Margins[]` arriving as zero is correct behaviour under
PAPPL, not a bug to compensate for.

1. Derive per-media hard margins from the PPD's `*ImageableArea` and
   `*PaperDimension`, convert to hundredths of a millimetre, and hold them in
   one committed table that cites the PPD lines it came from. That table is the
   only source of truth. *(The pt-level half of this table already exists as
   the `Media` constants in `src/golden.rs`, validated against the PPD by
   `test_golden_media_matches_ppd`.)*
2. Declare those margins in the PAPPL driver data, using field names read from
   the 1.3.1 headers — verified and quoted, not assumed.
3. At page start, read the margins back for the media actually selected and
   cross-check against the table. **A mismatch is a hard error.**
4. **No zero fallback anywhere.** A margin that resolves to zero or is absent
   fails the job with a specific error and a clear log line, referencing the
   regression recorded at `src/main.rs:427`. A page that looks fine until
   measured is worse than a refused job.

**Measured in P5 (2026-09-06):** the BLACK_1/PWG path delivers full-media
geometry and scanlines, with zero header margins. All 17 cases passed; see
`docs/MARGINS.md` and `docs/P5-MEASUREMENTS.json`.

### D-3 — Golden harness ships JSON sidecars and a margin-specific case
**Answered: Q-9.** Implemented in P2; see `src/golden.rs` and `goldens/`.

---

## Question status

All eleven questions raised in this document have been answered. The full
decision log, with reasoning, is in [`docs/DECISIONS.md`](DECISIONS.md);
excluded features are in [`docs/NON-GOALS.md`](NON-GOALS.md). This section is
the index.

### Decided

| # | Question | Decision |
|---|---|---|
| Q-1 | Which PAPPL version to target | Debian trixie's **1.3.1**, dynamically linked, guard `>= 1.3, < 2.0`. No vendoring, no static linking, no building 1.4.x from source. |
| Q-2 | Where the hard margin comes from | A **driver constant** derived from the PPD, cross-checked at page start. Never read from a raster header. **No zero fallback** — a zero or absent margin fails the job. |
| Q-3 | Duplex | **Out of scope for 2.0**, but publish `sides-supported` explicitly as one-sided rather than omitting it. |
| Q-4 | Toner save / density | **Deferred.** No invented PJL values. Replace the hardcoded `DENSITY=3` with a documented named constant. |
| Q-5 | Clean break | Approved for what we **ship**, not for what we **delete**. 1.x code frozen in-tree until P11 is green; the PPD stays permanently as project data. Delete list needs separate approval. |
| Q-6 | Keep `raster.rs`? | **Keep**, behind a non-default Cargo feature `golden-replay` — *not* `#[cfg(test)]`, which is invisible to integration tests in another crate. |
| Q-7 | Static or dynamic linking | **Dynamic**, against the archive's `libpappl1t64`. Answered together with Q-1; never open. |
| Q-8a | Licence for the FFI crates | **`MIT`** for both `pappl-sys` and `pappl` (`Apache-2.0 OR MIT` until 2026-09-10, when the unused Apache arm was dropped); `spl2-core` and `ml216x-printer-app` stay `GPL-2.0-only`. |
| Q-8b | Missing PPD licence header | **Add a GPL-2 header.** More important now that the PPD is permanent project data. |
| Q-9 | Golden-file fixtures | **Approved**, with JSON sidecars and a registration-mark corpus case. **Implemented** — see `src/golden.rs` and `goldens/`. |
| Q-10 | Device transport | **USB first, socket second.** Do not defer socket past 2.0 without asking. |
| Q-11 | Language for new code | **English** for code, comments, identifiers, commits, docs and packaging metadata. |

### Current outstanding validation

The maintainer selected 12.5 pt and P5 measured full-media raster delivery.
Hardware gate G-1 and P9's raster/dithering validation remain open.

Historical question closure: Q-8a — the licence for the FFI crates — was the last open
question and was decided on 2026-09-05: `pappl-sys` and `pappl` are both
permissively licensed — `Apache-2.0 OR MIT` then, `MIT` alone since
2026-09-10 — while `spl2-core` and `ml216x-printer-app` stay `GPL-2.0-only`.
MIT is what lets a GPL-2.0-only binary link them without inheriting
Apache-2.0's "further restrictions" under GPLv2 §6. See
`docs/DECISIONS.md`, Q-8a, for the reasoning and for the actions it leaves
outstanding until the crates exist.

### Deferred to a later step, not blocking

- **Completed in P5: printable-area vs full-media experiment (Q-2).** The
  tested BLACK_1/PWG path supplies full media; see `docs/MARGINS.md`.
- **Settled in P9: dithering-path exposure (from the Q-1 follow-up).**
  Declaring only 1-bit black raster does **not** make the dithering overflow
  unreachable — forcing `BLACK_1` selects the dithering path rather than
  avoiding it. See `docs/SECURITY-REVIEW.md` (S-1) and the P9 entry in
  `docs/DECISIONS.md`.
- **Confirmed in P9: the unpatched lines in 1.3.1's source.** Both
  `4587888f50` (dithering) and `44327aaac3` (ready-media) are present in
  `pappl 1.3.1-2.1` and were reproduced to a crash; `scripts/security-probe.py`
  re-runs them. Filing the Debian bug that cites them is the one action left,
  and it needs a bug-tracker submission (maintainer).
