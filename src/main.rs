//! The frozen 1.x CUPS raster filter.
//!
//! The protocol engine moved to the `spl2-core` crate during the PAPPL
//! migration; what stays here is the CUPS filter front end — argv, stdin,
//! stderr and the page loop that drives the engine. Decision Q-5 freezes this
//! binary's behaviour, so the golden corpus must not move when this file does.

/// The golden-file test harness; see `src/golden.rs`.
#[cfg(test)]
mod golden;

use std::env;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::process;

use spl2_core::engine::PageSetup;
use spl2_core::geometry::{
    duplex_mode, pjl_paper_type_for, quote_untrusted, validate_page_geometry, JobBudget,
    PageGeometry,
};
use spl2_core::log::{Level, Log};
use spl2_core::media;
use spl2_core::qpdl::{
    self as spl, current_service_date, JobConfig, SplPaperSource, SplStreamWriter,
};
use spl2_core::raster::{CupsRasterReader, PageHeader};

/// Sends the engine's diagnostics to stderr with the prefixes CUPS routes on.
///
/// `spl2-core` may not own stderr (`docs/MIGRATION-PLAN.md` §7), so the filter
/// supplies the sink. The strings themselves are unchanged, which is why the
/// prefix is spelled out here rather than derived.
struct CupsFilterLog;

impl Log for CupsFilterLog {
    fn log(&self, level: Level, message: &str) {
        let prefix = match level {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
            Level::Page => "PAGE",
        };
        eprintln!("{}: {}", prefix, message);
    }
}

/// The CUPS page header, as the engine sees it.
fn geometry_of(header: &PageHeader) -> PageGeometry {
    PageGeometry {
        width: header.width,
        height: header.height,
        bytes_per_line: header.bytes_per_line,
        hw_resolution: header.hw_resolution,
        page_size_points: header.page_size_points,
        margins: header.margins,
        bits_per_color: header.bits_per_color,
        bits_per_pixel: header.bits_per_pixel,
        color_space: header.color_space,
        color_order: header.color_order,
        num_copies: header.num_copies,
        media_position: header.media_position,
        duplex: header.duplex,
        tumble: header.tumble,
        media_type: header.media_type.clone(),
    }
}

/// Kept so the filter's own tests keep naming the check they exercise.
fn validate_page_header(header: &PageHeader) -> io::Result<()> {
    validate_page_geometry(&geometry_of(header))
}

/// CUPS filter arguments.
/// Standard invocation: `filter job-id user title num-copies options [filename]`
///
/// `num_copies` (argv[4]) and `options` (argv[5]) are DELIBERATELY not read;
/// the fields are kept only for diagnostics and positional correctness. The
/// reason: this is a raster filter, and the cups-filters stage that runs before
/// it in the chain (`gstoraster`/`pdftoraster`) has already interpreted the PPD
/// and written the selected media, resolution and copy count into the CUPS
/// Raster PAGE HEADER. When the page header and the command line disagree, the
/// header is binding — the page data was produced to match it. Reading options
/// here would mean writing a header that contradicts the produced data.
#[allow(dead_code)]
#[derive(Default)]
struct CupsFilterArgs {
    pub job_id: Option<String>,
    pub user: Option<String>,
    pub title: Option<String>,
    pub num_copies: Option<String>,
    pub options: Option<String>,
    pub filename: Option<String>,
}

impl CupsFilterArgs {
    fn parse(args: &[String]) -> Self {
        if args.len() >= 6 {
            Self {
                job_id: Some(args[1].clone()),
                user: Some(args[2].clone()),
                title: Some(args[3].clone()),
                num_copies: Some(args[4].clone()),
                options: Some(args[5].clone()),
                filename: args.get(6).cloned(),
            }
        } else if args.len() == 2 && !args[1].starts_with('-') {
            // Direct file mode: `cargo run -- file.raster`
            Self {
                filename: Some(args[1].clone()),
                ..Self::default()
            }
        } else {
            Self::default()
        }
    }
}

fn main() {
    // `env::args()` panics on an invalid UTF-8 byte in argv; but these
    // arguments (`job-id user title copies options [file]`) are derived by
    // `cupsd` from fields the submitting client supplied (e.g. job-name) and
    // must be treated as untrusted. So that a corrupt/malicious header cannot
    // crash the filter (a DoS) while it is still reading its first argument,
    // `env::args_os()` + a lossy UTF-8 conversion is used: invalid bytes are
    // silently replaced with `U+FFFD` and there is no panic.
    let raw_args: Vec<String> = env::args_os()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

    // CUPS filters are normally invoked with
    // `filter job-id user title copies options [file]` (5-6 arguments). Running
    // the program with no arguments at all (just the binary name) is not a real
    // `cupsd` invocation — it is either a misconfiguration or a manual/probing
    // run. This was verified by running the closest architectural equivalents
    // of this tool, `/usr/lib/cups/filter/rastertopwg` and `pstops`: both print
    // a "Usage: ..." message and exit with code 1 in this case; they do NOT
    // implement the "list supported MIME types and exit 0" behaviour specific
    // to CUPS *backends* (that behaviour is for device-discovering backends,
    // not for filters). So the same approach is taken here: exit early with a
    // clear usage message before attempting to read an empty stdin.
    if raw_args.len() <= 1 {
        let prog = raw_args
            .first()
            .cloned()
            .unwrap_or_else(|| "rastertospl-rust".to_string());
        eprintln!("Usage: {} job-id user title copies options [file]", prog);
        process::exit(1);
    }

    let args = CupsFilterArgs::parse(&raw_args);

    // `user`, `title` and `job_id` come from the submitting client (via CUPS)
    // and must be treated as untrusted: using `{:?}` (Debug) instead of `{}`
    // prints embedded ANSI/terminal escape sequences and control characters
    // (e.g. ESC, CR) in escaped form like `\u{1b}`, preventing fake log-line
    // injection or triggering terminal-emulator vulnerabilities. Although
    // `job_id` is a numeric string in the normal flow, that guarantee does not
    // hold when the filter is invoked by hand with manipulated arguments.
    if let (Some(job), Some(user)) = (&args.job_id, &args.user) {
        eprintln!("DEBUG: CUPS Job ID: {:?}, User: {:?}", job, user);
    }
    if let Some(title) = &args.title {
        eprintln!("DEBUG: CUPS Title: {:?}", title);
    }

    let input_reader: Box<dyn Read> = match &args.filename {
        Some(path) => {
            eprintln!(
                "DEBUG: reading CUPS Raster from file: {}",
                quote_untrusted(path)
            );
            match File::open(path) {
                Ok(file) => Box::new(BufReader::new(file)),
                Err(err) => {
                    eprintln!(
                        "ERROR: could not open the raster file {}: {}",
                        quote_untrusted(path),
                        err
                    );
                    process::exit(1);
                }
            }
        }
        None => {
            eprintln!("DEBUG: reading CUPS Raster from standard input (stdin)");
            Box::new(BufReader::new(io::stdin()))
        }
    };

    // `process_cups_raster_to_spl` keeps the `SplStreamWriter` LOCAL: when an
    // error returns via `?`, the writer is dropped before reaching this line
    // and its `Drop` impl writes the closing UEL. Because `process::exit` does
    // not run `Drop`, the ordering matters — the error is reported here, after
    // the writer has already been dropped.
    if let Err(err) =
        process_cups_raster_to_spl(&args, input_reader, io::stdout(), &current_service_date())
    {
        eprintln!("ERROR: raster processing error: {}", err);
        process::exit(1);
    }
}

/// Reads the standard CUPS Raster stream and converts it to Samsung QPDL/SPL2.
///
/// `writer` is taken as a parameter (rather than using `io::stdout()` directly)
/// so tests can inspect the produced SPL stream; in particular it is needed to
/// verify that the closing UEL is written on error paths.
///
/// `service_date` is supplied FROM OUTSIDE for the same reason (rather than
/// calling `current_service_date()` directly): because the
/// `@PJL DEFAULT SERVICEDATE` line carries today's date, the produced stream
/// would be clock-dependent and the golden-file comparison would break every
/// midnight. Loosening the comparison instead of fixing the date would also
/// hide real deviations; see `src/golden.rs`. `main` still passes
/// `current_service_date()`, so runtime behaviour does not change.
fn process_cups_raster_to_spl<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
) -> io::Result<()> {
    process_with_margin(args, reader, writer, service_date, media::HARD_MARGIN_PT)
}

// The production path always supplies the driver constant. The explicit input
// also preserves synthetic geometry cases in the golden harness.
fn process_with_margin<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
    margin_pt: f64,
) -> io::Result<()> {
    if !margin_pt.is_finite() || margin_pt <= 0.0 || margin_pt > 36.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid driver hard margin",
        ));
    }
    // 1. CUPS Raster header/magic check (RaSt, RaS2, RaS3, etc.)
    let mut raster_reader = CupsRasterReader::new(reader)?;

    eprintln!(
        "INFO: valid CUPS Raster stream detected (version: {:?}, endian: {})",
        raster_reader.version(),
        if raster_reader.version().is_big_endian() {
            "Big Endian"
        } else {
            "Little Endian"
        }
    );

    let mut spl_writer = SplStreamWriter::new(writer);

    // Read the first page header before starting the job (begin_job): CUPS
    // Raster carries the duplex information in the PAGE header, but the PJL job
    // header must report duplex at the JOB level. So we "peek" the first header
    // and set up the job config from it; we do not read it again in the loop.
    let mut next_header = raster_reader.next_page_header()?;

    let job_duplex = match &next_header {
        Some(h) => duplex_mode(h.duplex, h.tumble),
        None => spl::SplDuplex::Simplex,
    };

    // The paper type, like duplex, is reported at the JOB level (PJL), whereas
    // CUPS carries it in the PAGE header; so the same "peek the first header"
    // pattern is used. A different paper type per page cannot be expressed in
    // QPDL anyway.
    let job_paper_type = match &next_header {
        Some(h) => pjl_paper_type_for(&h.media_type, &CupsFilterLog),
        None => spl::PJL_PAPERTYPE_DEFAULT,
    };

    // 2. Samsung ML-2160 series PJL header (@PJL ENTER LANGUAGE = QPDL)
    let job_config = JobConfig {
        job_name: args
            .title
            .clone()
            .unwrap_or_else(|| "CUPS Document".to_string()),
        user_name: args.user.clone().unwrap_or_else(|| "guest".to_string()),
        service_date: service_date.to_string(),
        duplex: job_duplex,
        paper_type: job_paper_type,
    };
    spl_writer.begin_job(&job_config)?;

    let mut page_number = 0;
    let mut budget = JobBudget::default();

    // 3. Page loop
    while let Some(header) = next_header.take() {
        validate_page_header(&header)?;

        // The copy count is normalised once and the SAME value goes to both
        // the budget and the printer; computing it separately in two places
        // would make the budget count copies that are never actually printed.
        let geometry = geometry_of(&header);
        let copies = spl2_core::geometry::sanitize_copies(header.num_copies);

        // The page-count, raw-raster-volume and sheet-count limits; see
        // `JobBudget` for the rationale. Counted AFTER validation, so a
        // rejected page does not consume the budget.
        page_number = budget.account_page(geometry.total_raster_bytes(), copies)?;

        // The same value as `sanitize_copies` is logged: otherwise this line
        // could show the raw/unbounded `header.num_copies` value, different
        // from the copy count actually sent to the printer (written below via
        // begin_page/end_page through sanitize_copies()), producing misleading
        // diagnostics (e.g. if 65536 is requested, "65536" would be written here
        // but 999 sent to the printer).
        eprintln!("PAGE: {} {}", page_number, copies);
        eprintln!("INFO: starting page {}...", page_number);

        print_header_info(page_number, &header);

        // `cupsCompression` in CUPS Raster is not STREAM compression but a
        // driver-specific "device compression" hint (stream compression is
        // determined by the sync word; see raster.rs is_compressed). SpliX does
        // not use this field, and this filter always does band compression with
        // Algo 0x11; so the field is deliberately ignored. If it is non-zero we
        // report it once for diagnostics, because there may be a mismatch
        // between the PPD and this filter's assumptions.
        if header.compression != 0 {
            eprintln!(
                "WARNING: cupsCompression={} ignored; band compression is always Algo 0x11 RLE.",
                header.compression
            );
        }

        // Geometry, placement and the 17-byte page header are now in
        // `spl2-core`: the same computation runs on the PAPPL path too, so the
        // two front ends cannot diverge.
        let setup = PageSetup::new(&geometry, margin_pt, page_number, &CupsFilterLog)?;

        // The 17-byte QPDL page header
        spl_writer.begin_page(&setup.config)?;

        // Transfer the page bands with the SpliX-compatible stride
        let mut encoder = setup.encoder();
        let mut line_buffer = vec![0u8; setup.cups_bytes_per_line];
        for _ in 0..setup.total_lines {
            raster_reader.read_line(&mut line_buffer)?;
            encoder.write_line(&mut spl_writer, &line_buffer)?;
        }
        encoder.finish(&mut spl_writer)?;

        // The 3-byte QPDL page footer
        spl_writer.end_page(copies)?;

        eprintln!("INFO: page {} complete.\n", page_number);

        next_header = raster_reader.next_page_header()?;
    }

    if page_number == 0 {
        eprintln!("WARNING: no pages found in the CUPS Raster stream.");
    } else {
        eprintln!(
            "INFO: {} pages successfully converted to SPL/QPDL format.",
            page_number
        );
    }

    // Job end (the PJL UEL). Called even if no page was found: because
    // `begin_job` has already put the printer into QPDL, the stream must end
    // with a closing UEL in any case. `end_job` flushes internally.
    spl_writer.end_job()?;
    Ok(())
}

/// Formats the metadata from the CUPS Raster page header and prints it to stderr.
fn print_header_info(page_num: u32, header: &PageHeader) {
    // Every line starts with `DEBUG: `. CUPS routes the prefixes it recognises
    // in a filter's stderr (DEBUG/INFO/WARNING/ERROR/PAGE/...) to that level;
    // it also treats UNPREFIXED lines as DEBUG, so under the default
    // `LogLevel warn` the behaviour is the same. The difference showed up at
    // `LogLevel debug`: this block produces ~15 lines per page and the
    // unprefixed lines left the intent unclear. The prefix tells both CUPS and
    // the log reader plainly that the lines are diagnostic.
    eprintln!("DEBUG: --------------------------------------------------");
    eprintln!("DEBUG:  [CUPS RASTER PAGE {} METADATA]", page_num);
    eprintln!(
        "DEBUG:   Resolution (DPI): {} x {}",
        header.hw_resolution[0], header.hw_resolution[1]
    );
    eprintln!(
        "DEBUG:   Dimensions (px) : {} x {} (width x height)",
        header.width, header.height
    );
    eprintln!(
        "DEBUG:   Page size (pt)  : {} x {} pt",
        header.page_size_points[0], header.page_size_points[1]
    );
    if let Some(name) = &header.page_size_name {
        // `cupsPageSizeName` is a 64-byte C string in the raster header coming
        // from the submitting client — as untrusted as `title`/`user` in argv,
        // so it is printed escaped rather than raw.
        eprintln!("DEBUG:   Media name      : {}", quote_untrusted(name));
    }
    eprintln!("DEBUG:   Colour space    : {}", header.color_space);
    eprintln!("DEBUG:   Colour order    : {:?}", header.color_order);
    eprintln!("DEBUG:   Bits per colour : {}", header.bits_per_color);
    eprintln!("DEBUG:   Bits per pixel  : {}", header.bits_per_pixel);
    eprintln!("DEBUG:   Bytes per line  : {} bytes", header.bytes_per_line);
    eprintln!(
        "DEBUG:   Raw raster size : {} bytes ({:.2} MB)",
        header.total_raster_bytes(),
        header.total_raster_bytes() as f64 / (1024.0 * 1024.0)
    );
    eprintln!(
        "DEBUG:   Duplex          : {}",
        if header.duplex { "on" } else { "off" }
    );
    eprintln!("DEBUG:   Copies          : {}", header.num_copies);
    eprintln!(
        "DEBUG:   Paper source    : MediaPosition={} -> {:?}",
        header.media_position,
        SplPaperSource::from_media_position(header.media_position)
    );
    // `pjl_paper_type_for` prints the warning once while the job header is set
    // up; here only the result of the mapping is shown (to avoid a warning
    // repeated per page).
    eprintln!(
        "DEBUG:   Paper type      : MediaType={} -> PAPERTYPE={}",
        quote_untrusted(&header.media_type),
        spl::pjl_paper_type(&header.media_type).unwrap_or(spl::PJL_PAPERTYPE_DEFAULT)
    );
    eprintln!("DEBUG: --------------------------------------------------");
}

#[cfg(test)]
mod tests {
    use super::*;
    // The engine moved to `spl2-core`; these tests still exercise it, and are
    // the reason the split has to be behaviour preserving rather than merely
    // compiling.
    use spl2_core::geometry::*;
    use spl2_core::qpdl::{Algo0x11, SplDuplex, SplResolution};
    use spl2_core::raster::CupsRasterVersion;
    use std::io::Cursor;

    /// Represents an argument-less (direct pipeline) invocation.
    fn no_args() -> CupsFilterArgs {
        CupsFilterArgs::default()
    }

    /// Reads the project's PPD file. Some of the tests below tie the filter's
    /// constants to the PPD's real contents: forgetting to update the constants
    /// when a new option is added to the PPD breaks the test.
    fn ppd_text() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ppd/samsung-ml2160.ppd"
        ))
        .expect("could not read the PPD")
    }

    /// The `*Resolution` options the PPD offers, as (x_dpi, y_dpi). Single-value
    /// names like `600dpi` are written to both axes.
    fn ppd_resolutions() -> Vec<(u32, u32)> {
        let ppd = ppd_text();
        let list: Vec<(u32, u32)> = ppd
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("*Resolution ")?;
                let name = rest.split('/').next().unwrap_or("").trim_end_matches("dpi");
                let parse = |v: &str| {
                    v.parse::<u32>()
                        .unwrap_or_else(|_| panic!("could not parse PPD resolution: {}", line))
                };
                Some(match name.split_once('x') {
                    Some((x, y)) => (parse(x), parse(y)),
                    None => (parse(name), parse(name)),
                })
            })
            .collect();
        assert!(
            list.len() >= 4,
            "no resolutions read from the PPD: {}",
            list.len()
        );
        list
    }

    /// The `*PaperDimension` options the PPD offers, as (name, width_pt,
    /// height_pt). Sizes written with decimals are rounded up.
    fn ppd_paper_dimensions() -> Vec<(String, u32, u32)> {
        let ppd = ppd_text();
        let list: Vec<(String, u32, u32)> = ppd
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("*PaperDimension ")?;
                let (name, dims) = rest
                    .split_once(':')
                    .expect("malformed *PaperDimension line");
                let name = name.split('/').next().unwrap().trim().to_string();
                let mut it = dims.trim().trim_matches('"').split_whitespace();
                let mut pt = || {
                    it.next()
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or_else(|| panic!("could not parse PPD paper size: {}", line))
                        .ceil() as u32
                };
                let (w, h) = (pt(), pt());
                Some((name, w, h))
            })
            .collect();
        assert!(
            list.len() >= 10,
            "no paper sizes read from the PPD: {}",
            list.len()
        );
        list
    }

    #[test]
    fn test_sanitize_copies_never_returns_zero() {
        assert_eq!(sanitize_copies(0), 1);
        assert_eq!(sanitize_copies(1), 1);
        assert_eq!(sanitize_copies(5), 5);
        assert_eq!(
            sanitize_copies(MAX_REALISTIC_COPIES as u32),
            MAX_REALISTIC_COPIES
        );
        // Old behaviour: `.max(1) as u16` returned 0 here.
        assert_eq!(sanitize_copies(65536), MAX_REALISTIC_COPIES);
        assert_eq!(sanitize_copies(131072), MAX_REALISTIC_COPIES);
        assert_eq!(sanitize_copies(u32::MAX), MAX_REALISTIC_COPIES);
    }

    /// Produces a valid header: A4, 600 DPI, 1-bit monochrome (K), with a
    /// consistent `bytesPerLine`; tests base themselves on it and corrupt a
    /// single field to verify that `validate_page_header` rejects it.
    fn valid_header() -> PageHeader {
        let mut buf = vec![0u8; 1796];
        buf[276..280].copy_from_slice(&600u32.to_be_bytes()); // hw_resolution[0]
        buf[280..284].copy_from_slice(&600u32.to_be_bytes()); // hw_resolution[1]
        buf[352..356].copy_from_slice(&595u32.to_be_bytes()); // page_size_points[0] (A4)
        buf[356..360].copy_from_slice(&842u32.to_be_bytes()); // page_size_points[1]
        buf[372..376].copy_from_slice(&8u32.to_be_bytes()); // width
        buf[376..380].copy_from_slice(&8u32.to_be_bytes()); // height
        buf[384..388].copy_from_slice(&1u32.to_be_bytes()); // bits_per_color
        buf[388..392].copy_from_slice(&1u32.to_be_bytes()); // bits_per_pixel
        buf[392..396].copy_from_slice(&1u32.to_be_bytes()); // bytes_per_line = ceil(8*1/8)
        buf[400..404].copy_from_slice(&3u32.to_be_bytes()); // color_space = K
        PageHeader::parse(&buf, CupsRasterVersion::V2Be).unwrap()
    }

    #[test]
    fn test_validate_page_header_accepts_valid_mono_header() {
        assert!(validate_page_header(&valid_header()).is_ok());
    }

    #[test]
    fn test_validate_page_header_rejects_unsupported_resolution_without_rounding() {
        for resolution in [[599, 599], [600, 1200], [300, 600], [1200, 300]] {
            let mut header = valid_header();
            header.hw_resolution = resolution;
            let err = validate_page_header(&header)
                .expect_err("a resolution outside the PPD should have been rejected");
            assert!(
                err.to_string().contains("unsupported resolution"),
                "wrong error for {}x{}: {}",
                resolution[0],
                resolution[1],
                err
            );
        }
    }

    #[test]
    fn test_validate_page_header_rejects_unknown_paper_geometry() {
        let mut header = valid_header();
        header.page_size_points = [612, 936];
        let err = validate_page_header(&header)
            .expect_err("a paper size with no QPDL code should have been rejected");
        assert!(
            err.to_string().contains("unsupported paper size"),
            "{}",
            err
        );
    }

    #[test]
    fn test_validate_page_header_rejects_non_k_color_space() {
        let mut header = valid_header();
        header.color_space = CupsColorSpace(1); // RGB
        assert!(validate_page_header(&header).is_err());
    }

    #[test]
    fn test_validate_page_header_rejects_wrong_bit_depth() {
        // Simulates a multi-bit stream like 24-bit RGB / 32-bit CMYK.
        let mut header = valid_header();
        header.bits_per_color = 8;
        header.bits_per_pixel = 24;
        assert!(validate_page_header(&header).is_err());
    }

    #[test]
    fn test_validate_page_header_rejects_inconsistent_bytes_per_line() {
        let mut header = valid_header();
        header.bytes_per_line = 999; // inconsistent with width=8, bits_per_pixel=1
        assert!(validate_page_header(&header).is_err());
    }

    /// Produces a valid, single-page, uncompressed V3 big-endian raster
    /// stream. `pixel_bytes` decides how many bytes of page data are written;
    /// giving fewer than the header declares triggers the short-read (error)
    /// path.
    fn v3_stream(pixel_bytes: usize) -> Vec<u8> {
        let mut buf = vec![0u8; 1796];
        let mut put = |off: usize, val: u32| {
            buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600); // hw_resolution[0]
        put(280, 600); // hw_resolution[1]
        put(352, 595); // page_size_points[0] (A4)
        put(356, 842); // page_size_points[1]
        put(372, 32); // width
        put(376, 4); // height
        put(384, 1); // bits_per_color
        put(388, 1); // bits_per_pixel
        put(392, 4); // bytes_per_line = ceil(32 * 1 / 8)
        put(400, 3); // color_space = K

        let mut stream = b"RaS3".to_vec();
        stream.extend_from_slice(&buf);
        stream.extend_from_slice(&vec![0u8; pixel_bytes]);
        stream
    }

    /// Builds a multi-page, uncompressed (v3) CUPS Raster stream with the
    /// requested geometry/duplex. `v3_stream` produces a fixed A4/600 DPI; this
    /// flexible version exists so the resolution and duplex flags can be varied.
    struct RasterSpec {
        res_x: u32,
        res_y: u32,
        page_pt: (u32, u32),
        width_px: u32,
        height: u32,
        duplex: bool,
        tumble: bool,
        pages: usize,
        /// CUPS `MediaPosition` (PPD `*InputSlot`).
        media_position: u32,
        /// CUPS `MediaType` (PPD `*MediaType`); empty = not selected.
        media_type: &'static str,
        /// CUPS `Margins[0]` (the left margin of the PPD `*ImageableArea`, pt).
        margin_left_pt: u32,
        /// The pattern to use for each raster line; `None` = a fully blank
        /// line. Its length must be `bytes_per_line()`.
        line_pattern: Option<Vec<u8>>,
    }

    impl RasterSpec {
        /// A4, a page that fits the page width exactly at the given resolution.
        fn a4(res_x: u32, res_y: u32, height: u32) -> Self {
            let width_px = compute_page_width_pixels(595, res_x);
            Self {
                res_x,
                res_y,
                page_pt: (595, 842),
                width_px,
                height,
                duplex: false,
                tumble: false,
                pages: 1,
                media_position: 0,
                media_type: "",
                margin_left_pt: 0,
                line_pattern: None,
            }
        }

        fn bytes_per_line(&self) -> u32 {
            self.width_px.div_ceil(8)
        }

        fn build(&self) -> Vec<u8> {
            let mut stream = b"RaS3".to_vec();
            for _ in 0..self.pages {
                let mut buf = vec![0u8; 1796];
                let mut put = |off: usize, val: u32| {
                    buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
                };
                put(272, self.duplex as u32);
                put(312, self.margin_left_pt); // Margins[0] (sol, pt)
                put(324, self.media_position);
                put(276, self.res_x);
                put(280, self.res_y);
                put(352, self.page_pt.0);
                put(356, self.page_pt.1);
                put(368, self.tumble as u32); // Tumble (immediately before cupsWidth)
                put(372, self.width_px);
                put(376, self.height);
                put(384, 1); // bits_per_color
                put(388, 1); // bits_per_pixel
                put(392, self.bytes_per_line());
                put(400, 3); // color_space = K
                let media_type = self.media_type.as_bytes();
                buf[128..128 + media_type.len()].copy_from_slice(media_type);
                stream.extend_from_slice(&buf);
                let line = match &self.line_pattern {
                    Some(pattern) => {
                        assert_eq!(
                            pattern.len(),
                            self.bytes_per_line() as usize,
                            "the pattern does not match the line length"
                        );
                        pattern.clone()
                    }
                    None => vec![0u8; self.bytes_per_line() as usize],
                };
                for _ in 0..self.height {
                    stream.extend_from_slice(&line);
                }
            }
            stream
        }
    }

    /// Decodes the payload of the FIRST band record in the produced SPL stream
    /// and returns the band buffer `stream_page_bands` wrote (in its state
    /// BEFORE inversion).
    ///
    /// The buffer is transposed: `band[col * band_height + y]`.
    fn first_band_buffer(out: &[u8]) -> Vec<u8> {
        // Kayıt konumu deterministik olarak bulunur: 0x0C baytını aramak
        // güvenli değil, çünkü sayfa başlığının 0x4 baytı da (EnvIsoB5 kağıt
        // kodu) 0x0C olabilir.
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let pos = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("QPDL diline geçiş satırı yok")
            + QPDL_MARK.len()
            + 17; // 17 baytlık sayfa başlığından sonrası
        assert_eq!(out[pos], 0x0C, "şerit kaydı imzası beklendi");
        assert_eq!(out[pos + 6], 0x11, "Algo 0x11 bekleniyordu");
        let total = u32::from_be_bytes(out[pos + 7..pos + 11].try_into().unwrap()) as usize;
        // 11 bayt kayıt başlığı + 4 bayt alt başlık; sondaki 4 bayt checksum.
        let payload = &out[pos + 15..pos + 11 + total - 4];
        let mut band = Algo0x11::decompress(payload);
        // `stream_page_bands` yazmadan hemen önce tersliyor; geri al.
        for b in &mut band {
            *b = !*b;
        }
        band
    }

    /// Transpoze bant tamponunda, ilk satırdaki (`y == 0`) sıfır olmayan
    /// baytların sütun indislerini döner.
    fn nonzero_columns_in_first_line(band: &[u8], band_height: usize) -> Vec<usize> {
        band.chunks(band_height)
            .enumerate()
            .filter(|(_, col)| col[0] != 0)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Üretilen SPL akışındaki tek bir şerit kaydının başlık alanları.
    #[derive(Debug, PartialEq, Eq)]
    struct BandRecord {
        index: u8,
        width_px: u16,
        height_lines: u16,
    }

    /// Üretilen SPL akışındaki tek bir sayfa: 17 baytlık başlık + şeritleri.
    #[derive(Debug)]
    struct SplPage {
        header: [u8; 17],
        bands: Vec<BandRecord>,
    }

    /// SPL çıktısını gerçek kayıt yapısına göre ayrıştırır. Testlerin
    /// varsayımlarını değil, tele yazılan baytları doğrulayabilmesi için.
    fn parse_spl(out: &[u8]) -> Vec<SplPage> {
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let start = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("QPDL diline geçiş satırı yok")
            + QPDL_MARK.len();

        let mut pages = Vec::new();
        let mut pos = start;
        while pos < out.len() && !out[pos..].starts_with(spl::PJL_END) {
            assert_eq!(out[pos], 0x00, "sayfa başlığı imzası beklendi @ {}", pos);
            let mut header = [0u8; 17];
            header.copy_from_slice(&out[pos..pos + 17]);
            pos += 17;

            let mut bands = Vec::new();
            while pos < out.len() && out[pos] == 0x0C {
                let total = u32::from_be_bytes(out[pos + 7..pos + 11].try_into().unwrap()) as usize;
                bands.push(BandRecord {
                    index: out[pos + 1],
                    width_px: u16::from_be_bytes(out[pos + 2..pos + 4].try_into().unwrap()),
                    height_lines: u16::from_be_bytes(out[pos + 4..pos + 6].try_into().unwrap()),
                });
                // 11 baytlık kayıt başlığı + (alt başlık + payload + checksum)
                pos += 11 + total;
            }

            assert_eq!(out[pos], 0x01, "sayfa sonu imzası beklendi @ {}", pos);
            pos += 3;
            pages.push(SplPage { header, bands });
        }
        pages
    }

    fn run_filter(stream: Vec<u8>) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect("filtre geçerli akışı işleyemedi");
        out
    }

    fn count_uel(stream: &[u8]) -> usize {
        stream
            .windows(spl::PJL_UEL.len())
            .filter(|w| *w == spl::PJL_UEL)
            .count()
    }

    /// Y-03 regresyonu (uçtan uca): sayfa verisi yarıda kesilirse dönüşüm
    /// hata döndürmeli, AMA yazıcıya giden akış yine de kapanış UEL'i ile
    /// bitmeli. Aksi hâlde yazıcı QPDL dilinde, yarım bir bant kaydını
    /// bekler hâlde asılı kalır.
    #[test]
    fn test_closing_uel_written_on_truncated_page_data() {
        let stream = v3_stream(2); // 4 satır x 4 bayt = 16 bayt gerekiyordu
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("kısa sayfa verisi hata döndürmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            out.ends_with(spl::PJL_END),
            "hata yolu akışı kapanış UEL'i olmadan bıraktı"
        );
        assert_eq!(count_uel(&out), 2);
    }

    /// Y-03 regresyonu: sayfa başlığı doğrulamadan geçemezse de aynı garanti.
    #[test]
    fn test_closing_uel_written_on_invalid_page_header() {
        let mut stream = v3_stream(16);
        // bytes_per_line'ı 0 yap: validate_page_header reddedecek.
        stream[4 + 392..4 + 396].copy_from_slice(&0u32.to_be_bytes());

        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("geçersiz başlık hata döndürmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Akışta hiç sayfa yoksa da iş kapatılmalı: `begin_job` yazıcıyı çoktan
    /// QPDL diline sokmuştur.
    #[test]
    fn test_closing_uel_written_when_stream_has_no_pages() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(b"RaS3".to_vec())),
            &mut out,
            &current_service_date(),
        )
        .expect("sayfasız akış hata değil, uyarı üretmeli");
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2);
    }

    /// Başarılı akış tam olarak iki UEL içermeli: `Drop`, açıkça kapatılmış
    /// bir işe üçüncü bir UEL eklememelidir.
    #[test]
    fn test_successful_stream_has_exactly_one_uel_pair() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_stream(16))),
            &mut out,
            &current_service_date(),
        )
        .expect("geçerli akış başarılı olmalı");
        assert!(out.starts_with(spl::PJL_UEL));
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2, "fazladan UEL yazıldı");
    }

    /// Y-01 regresyonu (uçtan uca): sıkıştırılmış v2 akışı, aynı içeriğin
    /// sıkıştırmasız v3 hâliyle BİRE BİR aynı SPL çıktısını üretmeli.
    ///
    /// Eskiden v2 sayfa verisi ham piksel sanılıyordu ve yazıcıya çöp bir
    /// sayfa gidiyordu. Bu test, `CupsLineDecoder`'ın araya girdiğini ve
    /// akışın sürümünün çıktıyı hiç etkilemediğini sabitler.
    #[test]
    fn test_v2_and_v3_streams_produce_identical_output() {
        // 4 bayt/satır, 3 satır: 0xAA 0xAA 0xAA 0x55 / 0x00 x4 / 0xFF x4
        let pixels: [&[u8]; 3] = [
            &[0xAA, 0xAA, 0xAA, 0x55],
            &[0x00, 0x00, 0x00, 0x00],
            &[0xFF, 0xFF, 0xFF, 0xFF],
        ];

        let mut hdr = vec![0u8; 1796];
        {
            let mut put = |off: usize, val: u32| {
                hdr[off..off + 4].copy_from_slice(&val.to_be_bytes());
            };
            put(276, 600);
            put(280, 600);
            put(352, 595);
            put(356, 842);
            put(372, 32);
            put(376, 3);
            put(384, 1);
            put(388, 1);
            put(392, 4);
            put(400, 3);
        }

        let mut v3 = b"RaS3".to_vec();
        v3.extend_from_slice(&hdr);
        for line in pixels {
            v3.extend_from_slice(line);
        }

        // Aynı içeriğin CUPS satır-RLE karşılığı.
        let mut v2 = b"RaS2".to_vec();
        v2.extend_from_slice(&hdr);
        v2.extend_from_slice(&[0x00, 0x02, 0xAA, 0x00, 0x55]); // 0xAA x3, 0x55 x1
        v2.extend_from_slice(&[0x00, 0x80]); // satır sonuna kadar boş (K => 0x00)
        v2.extend_from_slice(&[0x00, 0x03, 0xFF]); // 0xFF x4

        let mut out_v3: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3)),
            &mut out_v3,
            &current_service_date(),
        )
        .expect("v3 akışı işlenmeli");
        let mut out_v2: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v2)),
            &mut out_v2,
            &current_service_date(),
        )
        .expect("v2 akışı işlenmeli");

        assert!(!out_v3.is_empty());
        assert_eq!(out_v2, out_v3, "v2 ve v3 çıktıları ayrıştı");
    }

    /// `validate_page_header` sınırları, PPD'nin sunduğu HER seçeneği
    /// kapsamalı.
    ///
    /// Sabitler PPD'den elle türetilmişti ve aradaki bağ yalnızca bir
    /// yorumdu: PPD'ye daha büyük bir kağıt ya da daha yüksek bir çözünürlük
    /// eklenirse, sabitleri güncellemeyi unutmak meşru işlerin sessizce
    /// reddedilmesine yol açardı. Bu test o bağı zorunlu kılar.
    #[test]
    fn test_limits_cover_every_ppd_option() {
        for (x_dpi, y_dpi) in ppd_resolutions() {
            assert!(
                SplResolution::pair_is_supported(x_dpi, y_dpi),
                "PPD {}x{} DPI sunuyor ama filtre bu çifti desteklemiyor",
                x_dpi,
                y_dpi
            );
            for dpi in [x_dpi, y_dpi] {
                assert!(
                    dpi <= MAX_DPI,
                    "PPD {} DPI sunuyor ama MAX_DPI = {}; sabiti güncelleyin",
                    dpi,
                    MAX_DPI
                );
            }
        }

        for (name, w, h) in ppd_paper_dimensions() {
            for pt in [w, h] {
                assert!(
                    pt <= MAX_POINTS,
                    "PPD '{}' {} pt kağıt sunuyor ama MAX_POINTS = {}; sabiti güncelleyin",
                    name,
                    pt,
                    MAX_POINTS
                );
                // En büyük kağıt en yüksek çözünürlükte satır/sütun
                // sınırlarına da sığmalı.
                let pixels = (pt as u64 * MAX_DPI as u64).div_ceil(72);
                assert!(
                    pixels <= MAX_LINES as u64,
                    "{} pt @ {} DPI = {} satır, MAX_LINES = {}",
                    pt,
                    MAX_DPI,
                    pixels,
                    MAX_LINES
                );
                assert!(
                    pixels.div_ceil(8) <= MAX_BYTES_PER_LINE as u64,
                    "{} pt @ {} DPI = {} bayt/satır, MAX_BYTES_PER_LINE = {}",
                    pt,
                    MAX_DPI,
                    pixels.div_ceil(8),
                    MAX_BYTES_PER_LINE
                );
            }
        }
    }

    #[test]
    fn test_validate_page_header_rejects_line_wider_than_page() {
        let mut header = valid_header();
        header.page_size_points = [297, 420]; // desteklenen A6
        header.width = 4960;
        header.bytes_per_line = 620; // cupsWidth ile tutarlı, sayfayla değil
        let err = validate_page_header(&header).expect_err("dar sayfa reddedilmeliydi");
        assert!(
            err.to_string().contains("does not fit the page width"),
            "hata nedeni açıklanmalı: {}",
            err
        );
    }

    /// Gerçek bir tam genişlik A4 sayfası (595 pt @ 600 DPI = 620 B/satır)
    /// reddedilmemeli — aşırı düzeltme kontrolü.
    #[test]
    fn test_validate_page_header_accepts_full_width_a4() {
        let mut header = valid_header();
        header.width = 4960;
        header.bytes_per_line = 620;
        assert!(validate_page_header(&header).is_ok());
    }

    /// Yuvarlama payı: 1 baytlık aşım hoş görülür, 2 bayt reddedilir.
    #[test]
    fn test_validate_page_header_line_width_slack_is_one_byte() {
        // 595 pt @ 600 DPI => 4960 px => 620 bayt bant genişliği.
        let mut ok = valid_header();
        ok.width = 4968; // 621 bayt
        ok.bytes_per_line = 621;
        assert!(
            validate_page_header(&ok).is_ok(),
            "1 baytlık pay kabul edilmeli"
        );

        let mut too_wide = valid_header();
        too_wide.width = 4976; // 622 bayt
        too_wide.bytes_per_line = 622;
        assert!(
            validate_page_header(&too_wide).is_err(),
            "2 bayt aşım reddedilmeli"
        );
    }

    /// D-02: `cupsHeight` sayfanın fiziksel yüksekliğine sığmalı — D-01'in
    /// dikey karşılığı. Yamadan önce bu başlık kabul ediliyordu.
    #[test]
    fn test_validate_page_header_rejects_page_taller_than_paper() {
        let mut header = valid_header();
        header.page_size_points = [297, 420]; // desteklenen A6
        header.height = 4_000; // MAX_LINES içinde, ama A6'ya sığmıyor
        let err = validate_page_header(&header).expect_err("uzun sayfa reddedilmeliydi");
        assert!(
            err.to_string().contains("does not fit the page height"),
            "hata nedeni açıklanmalı: {}",
            err
        );

        // Normal boyutlu bir A4 sayfasında da aşım yakalanmalı: 842 pt @ 600
        // DPI = 7017 satır; raster bunun altında kalmalıdır.
        let mut a4 = valid_header();
        a4.height = 24_000;
        assert!(
            validate_page_header(&a4).is_err(),
            "A4'e sığmayan yükseklik reddedilmeli"
        );
    }

    /// Aşırı düzeltme kontrolü: gerçek `cupsfilter` çıktısının ürettiği
    /// yükseklikler reddedilmemeli. Değerler, ppd/samsung-ml2160.ppd ile
    /// `cupsfilter -m application/vnd.cups-raster` çalıştırılarak ölçüldü;
    /// hepsi fiziksel sınırın altında kalır çünkü `*ImageableArea` kenar
    /// boşlukları (12 pt üst + 12 pt alt) düşülür.
    #[test]
    fn test_validate_page_header_accepts_real_cupsfilter_heights() {
        // (sayfa_genişliği_pt, sayfa_yüksekliği_pt, y_dpi, ölçülen cupsHeight)
        let measured = [
            (595u32, 842u32, 300u32, 3408u32), // A4
            (595, 842, 600, 6817),
            (595, 842, 1200, 13633),
            (612, 792, 600, 6400),  // Letter
            (612, 1008, 600, 8200), // Legal
            (612, 1008, 1200, 16400),
            (297, 420, 600, 3300),   // A6
            (595, 935, 1200, 15183), // Folio
        ];
        for (width_pt, height_pt, ydpi, cups_height) in measured {
            let mut h = valid_header();
            h.page_size_points = [width_pt, height_pt];
            // Bu tablodaki ölçümler simetrik çözünürlüklerden alındı.
            h.hw_resolution = [ydpi, ydpi];
            h.height = cups_height;
            assert!(
                validate_page_header(&h).is_ok(),
                "gerçek cupsfilter çıktısı reddedildi: {}x{} pt @ {} DPI => {} satır",
                width_pt,
                height_pt,
                ydpi,
                cups_height
            );
        }
    }

    /// Yuvarlama payı: 8 satırlık aşım hoş görülür, fazlası reddedilir.
    #[test]
    fn test_validate_page_header_height_slack_is_eight_lines() {
        // 842 pt @ 600 DPI => ceil(842 * 600 / 72) = 7017 satır.
        let exact = compute_page_height_lines(842, 600);
        assert_eq!(exact, 7017);

        let mut ok = valid_header();
        ok.height = exact + 8;
        assert!(
            validate_page_header(&ok).is_ok(),
            "8 satırlık pay kabul edilmeli"
        );

        let mut too_tall = valid_header();
        too_tall.height = exact + 9;
        assert!(
            validate_page_header(&too_tall).is_err(),
            "9 satır aşım reddedilmeli"
        );
    }

    /// D-06: sert kenar boşluğu SpliX ile birebir aynı hesaplanmalı.
    ///
    /// SpliX compress.cpp: `((ceil(marginPt * dpi / 72) + 7) & ~7) / 8`.
    /// 8'e YUKARI hizalama önemli: 12 pt @600 DPI = 100 piksel, hizalanınca
    /// 104 piksel = 13 bayt olur; hizalamasız 12,5 bayt (kırpılınca 12) çıkar
    /// ve bant bir bayt kayar.
    #[test]
    fn test_hard_margin_matches_splix_alignment() {
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 300), 7);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 600), 14);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 1200), 27);

        // Bu projenin PPD'sindeki *ImageableArea sol kenar boşluğu: 12 pt.
        assert_eq!(hard_margin_bytes(12.0, 600), 13);
        assert_eq!(hard_margin_bytes(12.0, 300), 7);
        assert_eq!(hard_margin_bytes(12.0, 1200), 25);
        // Kenar boşluğu bildirilmemişse kaydırma da yok.
        assert_eq!(hard_margin_bytes(0.0, 600), 0);
    }

    /// D-06 regresyonu: yatay yerleşim ORTALAMA EKSİ SERT KENAR BOŞLUĞU
    /// olmalıdır, yalnızca ortalama değil.
    ///
    /// Eskiden yalnızca `(bandWidthInB - lineSize) / 2` uygulanıyordu; A4 @600
    /// DPI'da bu 12 baytlık (96 piksel ≈ 4 mm) bir sağa kayma demekti, çünkü
    /// SpliX bandı doldururken `hardMarginXInB` (13 bayt) kadar ATLAR
    /// (compress.cpp:227). Net ofset sıfırdır.
    #[test]
    fn test_band_placement_subtracts_hard_margin() {
        // A4 @600 DPI, bu projenin PPD'sindeki gerçek sayılar:
        // bant 620 B, CUPS satırı 595 B, sert kenar boşluğu 13 B.
        let a4 = band_placement(620, 595, hard_margin_bytes(12.0, 600)).unwrap();
        assert_eq!(
            a4,
            BandPlacement {
                dst_offset: 0,
                src_skip: 1
            },
            "ortalama (12 B) sert kenar boşluğunu (13 B) telafi etmeli"
        );

        // Regresyon çapası: sert kenar boşluğu düşülmezse eski, hatalı
        // 12 baytlık kayma geri gelir.
        assert_eq!(
            band_placement(620, 595, 0).unwrap(),
            BandPlacement {
                dst_offset: 12,
                src_skip: 0
            },
            "kenar boşluğu yoksa davranış saf ortalamadır"
        );

        // Ortalama sert kenar boşluğundan BÜYÜKSE fark hedefe kalır.
        assert_eq!(
            band_placement(620, 560, 13).unwrap(),
            BandPlacement {
                dst_offset: 17,
                src_skip: 0
            }
        );

        // Bant satırdan darsa ortalama yoktur; kenar boşluğu satırdan atılır.
        assert_eq!(
            band_placement(600, 620, 13).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 13
            }
        );

        // Satırın tamamı atlanacaksa geometri tutarsızdır: sessizce kırpmak
        // yerine reddedilir (eskiden `src_skip` 3'e kırpılıp tek baytlık,
        // yani fiilen boş bir sayfa üretiliyordu).
        let err = band_placement(4, 4, 999)
            .expect_err("satırdan geniş sert kenar boşluğu reddedilmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("the hard margin"), "{}", err);

        // Sınır: tam olarak bir bayt kalması hâlâ kabul edilir.
        assert_eq!(
            band_placement(4, 4, 3).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 3
            }
        );
    }

    /// D-07: `Margins[0]` doğrulanmadığında `hard_margin_bytes` içindeki
    /// `px + 7` toplaması taşıyordu — `overflow-checks` açık yapılarda iş
    /// ortasında panik, sürüm yapılarında sessizce 0'a sarma. Sayfadan geniş
    /// bir sol kenar boşluğu artık başlık doğrulamasında reddediliyor.
    #[test]
    fn test_validate_page_header_rejects_out_of_page_left_margin() {
        for margin in [595, 600, 300_000_000] {
            let mut header = valid_header();
            header.margins[0] = margin;
            let err = validate_page_header(&header)
                .expect_err("sayfadan geniş sol kenar boşluğu reddedilmeliydi");
            assert!(
                err.to_string().contains("invalid left margin"),
                "{} pt için yanlış hata: {}",
                margin,
                err
            );
        }

        // PPD'nin gerçek değeri (12 pt) ve sınırın hemen altı kabul edilmeli.
        for margin in [0, 12, 594] {
            let mut header = valid_header();
            header.margins[0] = margin;
            assert!(
                validate_page_header(&header).is_ok(),
                "{} pt kabul edilmeliydi",
                margin
            );
        }
    }

    /// D-06 regresyonu (uçtan uca): raster içeriği, yazıcıya giden bant
    /// tamponunda sert kenar boşluğu düşülmüş sütunda durmalı.
    ///
    /// Gerçek A4 @600 DPI geometrisi kurulur (620 B bant, 595 B CUPS satırı,
    /// `Margins[0] = 12 pt`) ve satırın 3. baytına bir işaret konur.
    /// Beklenen sütun `3 - src_skip + dst_offset = 2`'dir; düzeltmeden önce
    /// aynı bayt 15. sütuna (12 baytlık kayma + 3) yazılıyordu.
    #[test]
    fn test_content_lands_at_hard_margin_corrected_column() {
        const MARKER_INDEX: usize = 3;
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760; // 595 B/satır: gerçek cupsfilter çıktısına eşit
        spec.margin_left_pt = 12; // PPD *ImageableArea: "12 12 583 830"

        let mut pattern = vec![0u8; 595];
        pattern[MARKER_INDEX] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);
        assert_eq!(band.len(), 620 * QPDL_BAND_HEIGHT, "bant tamponu boyutu");

        let columns = nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT);
        assert_eq!(
            columns,
            vec![1],
            "işaret baytı yanlış sütunda; 15 ise sert kenar boşluğu düşülmüyor"
        );
    }

    /// Missing integer header margins must not remove the 12.5 pt driver margin.
    #[test]
    fn test_missing_header_margin_still_uses_driver_constant() {
        const MARKER_INDEX: usize = 3;
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760;
        spec.margin_left_pt = 0;

        let mut pattern = vec![0u8; 595];
        pattern[MARKER_INDEX] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);
        let columns = nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT);
        assert_eq!(columns, vec![MARKER_INDEX - 2]);
    }

    /// Yatay ve dikey eksen AYNI origin'i kullanmalı.
    ///
    /// Hatanın özü buydu: dikeyde hiçbir kaydırma yokken yatayda 12 baytlık
    /// bir kaydırma vardı. SpliX'te iki eksenin de neti sıfırdır (ortalama
    /// eksi sert kenar boşluğu). Bu test ilk raster satırının bant tamponunun
    /// hem 0. satırında hem 0. sütununda başladığını sabitler.
    #[test]
    fn test_horizontal_and_vertical_origins_agree() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760;
        spec.margin_left_pt = 12;
        // With 14 margin bytes and 12 centring bytes, source byte 2 lands at column 0.
        let mut pattern = vec![0u8; 595];
        pattern[2] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);

        // Sütun 0, satır 0: içerik bandın sol-üst köşesinden başlar.
        assert_eq!(band[0], 0xFF, "içerik bandın (0,0) köşesinde başlamalı");
        assert_eq!(
            nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT),
            vec![0]
        );
    }

    /// Yükseklik sınırı dikey çözünürlüğe bağlı olmalı: `compute_page_height_lines`
    /// `hw_resolution[1]`'i alır, `[0]`'ı değil. 1200x600 desteklenen bir mod
    /// olduğundan iki eksenin birbirinden bağımsız kullanılması gerekir.
    #[test]
    fn test_page_height_lines_uses_vertical_resolution() {
        assert_eq!(compute_page_height_lines(842, 600), 7017);
        assert_eq!(compute_page_height_lines(842, 300), 3509);
        assert_eq!(compute_page_height_lines(842, 1200), 14034);

        // Sınır gerçekten çözünürlükle ölçekleniyor: 600 DPI'da geçerli olan
        // bir yükseklik, aynı kağıtta 300 DPI'da reddedilmeli.
        let mut h300 = valid_header();
        h300.hw_resolution = [300, 300];
        h300.width = 2480;
        h300.bytes_per_line = 310;
        h300.height = 6817; // 600 DPI'nın satır sayısı
        assert!(
            validate_page_header(&h300).is_err(),
            "300 DPI'da 600 DPI'nın satır sayısı kabul edilmemeli"
        );

        h300.height = 3408; // 300 DPI için gerçek cupsfilter değeri
        assert!(validate_page_header(&h300).is_ok());
    }

    /// Asimetrik çözünürlük (`1200x600dpi`) DESTEKLENEN gerçek bir QPDL
    /// modudur — SpliX de aynı seçeneği ml2010/ml2015/ml1640/ml2510/ml2525
    /// PPD'lerinde sunar — ve reddedilmemelidir.
    #[test]
    fn test_validate_page_header_accepts_asymmetric_resolution() {
        // A4 @ 1200x600: cupsfilter'ın gerçekte ürettiği değerler.
        let mut h = valid_header();
        h.hw_resolution = [1200, 600];
        h.width = 9517;
        h.bytes_per_line = 1190;
        h.height = 6817;
        assert!(
            validate_page_header(&h).is_ok(),
            "1200x600dpi gerçek bir QPDL modu, reddedilmemeli: {:?}",
            validate_page_header(&h).err()
        );
    }

    /// PPD'nin sunduğu her çözünürlük, filtre tarafından da kabul edilmeli:
    /// aksi hâlde kullanıcı sebebi belirsiz bir "filter failed" görür.
    #[test]
    fn test_filter_accepts_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            // O çözünürlükte A4 için tutarlı bir başlık kur.
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            h.bytes_per_line = (595 * x).div_ceil(72).div_ceil(8);
            h.width = h.bytes_per_line * 8;
            h.height = (842 * y).div_ceil(72);
            assert!(
                validate_page_header(&h).is_ok(),
                "PPD {}x{} DPI sunuyor ama filtre reddediyor: {:?}",
                x,
                y,
                validate_page_header(&h).err()
            );
        }
    }

    /// D-04 regresyonu: `cupsColorOrder` artık denetleniyor.
    #[test]
    fn test_validate_page_header_checks_color_order() {
        // 1-bit tek kanallı veride üç dizilim de eşdeğerdir, üçü de kabul.
        for order in [
            CupsColorOrder::Chunked,
            CupsColorOrder::Banded,
            CupsColorOrder::Planar,
        ] {
            let mut header = valid_header();
            header.color_order = order;
            assert!(
                validate_page_header(&header).is_ok(),
                "{:?} kabul edilmeliydi",
                order
            );
        }

        let mut unknown = valid_header();
        unknown.color_order = CupsColorOrder::Unknown(99);
        assert!(
            validate_page_header(&unknown).is_err(),
            "tanınmayan dizilim reddedilmeli"
        );
    }

    /// Belirtilen sayıda küçük, geçerli sayfadan oluşan bir V3 akışı üretir.
    fn v3_multipage_stream(pages: u32) -> Vec<u8> {
        v3_multipage_stream_with_copies(pages, 0)
    }

    /// `v3_multipage_stream`'in, sayfa başlığındaki `cupsNumCopies` alanını da
    /// ayarlayan sürümü; yaprak (sayfa x kopya) bütçesini sınamak için.
    fn v3_multipage_stream_with_copies(pages: u32, copies: u32) -> Vec<u8> {
        let mut page = vec![0u8; 1796];
        {
            let mut put = |off: usize, val: u32| {
                page[off..off + 4].copy_from_slice(&val.to_be_bytes());
            };
            // Desteklenen en küçük kâğıt ve çözünürlük (A6 @ 300 DPI), fakat
            // yalnızca 8x1 piksellik geçerli bir raster bölgesi. Sayfa başına
            // bant tamponu yaklaşık 10 KiB'ta kalır; 5.000 sayfalık sınır
            // testi yine hızlıdır.
            put(276, 300); // hw_resolution
            put(280, 300);
            put(352, 297); // page_size_points: A6
            put(356, 420);
            put(372, 8); // width
            put(376, 1); // height
            put(384, 1); // bits_per_color
            put(388, 1); // bits_per_pixel
            put(392, 1); // bytes_per_line
            put(400, 3); // color_space = K
            put(340, copies); // num_copies
        }
        let mut stream = b"RaS3".to_vec();
        for _ in 0..pages {
            stream.extend_from_slice(&page);
            stream.push(0u8); // 1 satır x 1 bayt
        }
        stream
    }

    /// D-04 regresyonu: sayfa sayısının bir üst sınırı olmalı.
    #[test]
    fn test_page_count_is_capped() {
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB + 1))),
            &mut out,
            &current_service_date(),
        )
        .expect_err("sayfa sınırı aşılınca hata beklenir");
        assert!(
            err.to_string().contains("exceeded the page limit"),
            "{}",
            err
        );
        // Sınır aşılsa bile iş düzgün kapatılmalı (Y-03 garantisi).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Ham raster hacmi bütçesi uygulanmalı ve tam sınırda kabul etmeli.
    #[test]
    fn test_job_raster_byte_budget_is_enforced() {
        let mut budget = JobBudget::default();
        budget
            .account_page(MAX_JOB_RASTER_BYTES, 1)
            .expect("bütçenin tamamı kabul edilmeli");
        assert_eq!(budget.raster_bytes, MAX_JOB_RASTER_BYTES);

        let err = budget
            .account_page(1, 1)
            .expect_err("bütçeyi bir bayt aşmak hata vermeli");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeded the raster volume limit"),
            "{}",
            err
        );

        // Sarma yerine doyma: sınırlar ileride büyütülse bile taşma sessizce
        // bütçeyi sıfırlamamalı.
        let mut huge = JobBudget {
            pages: 0,
            raster_bytes: u64::MAX - 1,
            impressions: 0,
        };
        assert!(huge.account_page(u64::MAX, 1).is_err());
        assert_eq!(huge.raster_bytes, u64::MAX);
    }

    /// BULGU 2: sayfa sınırı ile kopya sınırının ÇARPIMI sınırsız olmamalı.
    ///
    /// Bu testin varlık nedeni, iki sınırın ayrı ayrı "makul" görünürken
    /// birlikte 4.995.000 yaprağa (eski değerlerle) izin vermesiydi. Sayaç
    /// yaprak cinsindendir; sayfa sayısı tek başına anlamlı bir tavan değil.
    #[test]
    fn test_page_and_copy_limits_cannot_multiply_without_bound() {
        let unbounded_product = MAX_PAGES_PER_JOB as u64 * MAX_REALISTIC_COPIES as u64;
        assert!(
            MAX_JOB_IMPRESSIONS < unbounded_product,
            "yaprak sınırı, sayfa x kopya çarpımından ({}) küçük olmalı; \
             aksi hâlde hiçbir şey sınırlamıyor demektir",
            unbounded_product
        );

        // Azami kopya sayısıyla, yaprak bütçesinin izin verdiği sayfa sayısı.
        let mut budget = JobBudget::default();
        let allowed = MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64;
        for page in 1..=allowed {
            budget
                .account_page(1, MAX_REALISTIC_COPIES)
                .unwrap_or_else(|e| panic!("sayfa {} reddedilmemeliydi: {}", page, e));
        }
        let err = budget
            .account_page(1, MAX_REALISTIC_COPIES)
            .expect_err("yaprak sınırı aşılınca hata beklenir");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeded the sheet limit"),
            "{}",
            err
        );
    }

    /// Yaprak bütçesi tam sınırda kabul etmeli, bir yaprak fazlasında reddetmeli.
    #[test]
    fn test_impression_budget_is_not_off_by_one() {
        let mut budget = JobBudget::default();
        budget
            .account_page(1, u16::try_from(MAX_JOB_IMPRESSIONS).unwrap())
            .expect("tam sınır kadar yaprak kabul edilmeli");
        assert_eq!(budget.impressions, MAX_JOB_IMPRESSIONS);
        assert!(budget.account_page(1, 1).is_err());
    }

    /// Bütçe, HAM `num_copies` ile değil `sanitize_copies`'ten geçmiş değerle
    /// saymalı: yazıcıya gitmeyen kopyalar bütçeyi tüketmemeli, ama 65536 gibi
    /// bir değer de "0 kopya" sayılıp bütçeden kaçmamalı.
    #[test]
    fn test_impression_budget_counts_sanitized_copies() {
        let mut budget = JobBudget::default();
        // Eski `as u16` davranışında bu 0 ediyordu; şimdi 999 sayılmalı.
        budget.account_page(1, sanitize_copies(65_536)).unwrap();
        assert_eq!(budget.impressions, MAX_REALISTIC_COPIES as u64);

        let mut zero = JobBudget::default();
        zero.account_page(1, sanitize_copies(0)).unwrap();
        assert_eq!(zero.impressions, 1, "0 kopya en az 1 yaprak sayılmalı");
    }

    /// Yaprak sınırı uçtan uca da uygulanmalı ve iş yine düzgün kapanmalı.
    #[test]
    fn test_impression_limit_is_enforced_end_to_end() {
        // Her sayfa azami kopyayla: sınır sayfa sayısından çok önce dolar.
        let pages = (MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64) as u32 + 1;
        assert!(
            pages < MAX_PAGES_PER_JOB,
            "sayfa sınırı önce devreye girmemeli"
        );

        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream_with_copies(
                pages,
                MAX_REALISTIC_COPIES as u32,
            ))),
            &mut out,
            &current_service_date(),
        )
        .expect_err("yaprak sınırı aşılınca hata beklenir");
        assert!(
            err.to_string().contains("exceeded the sheet limit"),
            "{}",
            err
        );
        // Sınır aşılsa bile iş düzgün kapatılmalı (Y-03 garantisi).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Bütçe, normal çözünürlüklü işlerin davranışını DEĞİŞTİRMEMELİ:
    /// `MAX_PAGES_PER_JOB` kadar A4 @600 DPI sayfa bütçenin altında kalmalı.
    #[test]
    fn test_job_budget_does_not_bind_for_600dpi_pages_within_page_limit() {
        // Görüntülenebilir alanı değil daha büyük olan fiziksel A4 geometrisini
        // kullan; böylece sınır farklı cups-filters yuvarlamalarına da dayanır.
        let a4_600 = compute_page_width_pixels(595, 600).div_ceil(8) as u64
            * compute_page_height_lines(842, 600) as u64;
        let worst = a4_600 * MAX_PAGES_PER_JOB as u64;
        assert!(
            worst < MAX_JOB_RASTER_BYTES,
            "{} A4@600DPI sayfa ({} bayt) bütçeyi ({}) aşmamalı",
            MAX_PAGES_PER_JOB,
            worst,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// Doğrulayıcının kabul ettiği en büyük ham raster geometrisi:
    /// Legal @1200 DPI ile D-01/D-02 yuvarlama payları. Bilinmeyen ölçüler
    /// artık kabul edilmediğinden global `MAX_POINTS` değeri erişilebilir bir
    /// sayfa geometrisi değildir.
    fn largest_accepted_page_geometry() -> (u64, u64) {
        let bytes_per_line = compute_page_width_pixels(612, 1200).div_ceil(8) as u64
            + LINE_OVERSHOOT_SLACK_BYTES as u64;
        let lines =
            compute_page_height_lines(1008, 1200) as u64 + HEIGHT_OVERSHOOT_SLACK_LINES as u64;
        (bytes_per_line * lines, lines)
    }

    /// ...ama en büyük kabul edilebilir sayfada GERÇEKTEN devreye girmeli,
    /// yoksa sayfa sınırının çözünürlük körlüğü kapanmamış olur.
    #[test]
    fn test_job_budget_binds_before_page_limit_at_max_page_size() {
        let (max_page, _) = largest_accepted_page_geometry();
        let pages_allowed = MAX_JOB_RASTER_BYTES / max_page;
        assert!(
            pages_allowed > 0,
            "bütçe tek bir azami sayfayı bile reddediyor"
        );
        assert!(
            pages_allowed < MAX_PAGES_PER_JOB as u64,
            "azami boyutlu sayfalarda bütçe sayfa sınırından önce devreye girmeli \
             (izin verilen: {}, sayfa sınırı: {})",
            pages_allowed,
            MAX_PAGES_PER_JOB
        );
        assert_eq!(
            pages_allowed + 1,
            401,
            "kaynak bütçesi açıklamasındaki ilk reddedilen sayfa değişti"
        );
    }

    /// BULGU 1: bütçe, en kötü durumdaki CPU süresini de sınırlamalı.
    ///
    /// Bu sınır bayt cinsinden olduğu için CPU süresini ancak sıkıştırıcının
    /// ölçülen hızı üzerinden dolaylı olarak bağlar. Aşağıdaki değer bu
    /// makinede, release derlemesinde, gerçek bant boyutunda (346.752 bayt)
    /// ölçüldü: sıkıştırılamaz gürültü ~6,65 MB/s (sıfır dolu bantta ~166 MB/s,
    /// yani en iyi ile en kötü arasında ~25 kat var).
    ///
    /// Testin amacı bir hız ölçmek değil — makineden makineye değişir — bütçe
    /// büyütüldüğünde bunun CPU tarafındaki bedelini görünür kılmak: filtre
    /// CUPS kuyruğunu tek iş parçacığıyla işlediği için bu süre boyunca
    /// sıradaki tüm işler bekler.
    const MEASURED_WORST_CASE_COMPRESS_BPS: u64 = 6_650_000;

    #[test]
    fn test_raster_budget_bounds_worst_case_cpu_time() {
        let worst_case_seconds = MAX_JOB_RASTER_BYTES / MEASURED_WORST_CASE_COMPRESS_BPS;
        assert!(
            worst_case_seconds <= 30 * 60,
            "en kötü durumda tek bir iş kuyruğu {} saniye bloke ediyor; \
             MAX_JOB_RASTER_BYTES ({}) düşürülmeli",
            worst_case_seconds,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// BULGU 1'in asıl çekirdeği: bütçe ÇÖZÜLMÜŞ raster hacmini sayar, girdi
    /// hacmini değil — ve CUPS Raster v2'nin satır-RLE'si arada çok büyük bir
    /// genişleme sağlar. Desteklenen en büyük geometride yaklaşık 0,74 MiB'lik
    /// tamamen beyaz bir akış 8 GiB'tan fazla raster işi doğurabilir.
    ///
    /// Bu yüzden "girdi küçükse iş de küçüktür" varsayımı yapılamaz; tek gerçek
    /// savunma tavanın kendisidir. Test, o tavanın saldırganın gönderebileceği
    /// bayt sayısıyla DEĞİL, yalnızca sabitle sınırlı kaldığını pinliyor.
    #[test]
    fn test_compressed_input_cannot_amplify_past_the_raster_budget() {
        // Tek bir v2 satır kaydı 2 bayttır ([tekrar][0x80 = satır sonuna kadar
        // boşalt]) ve 256 satıra kadar üretir; sayfa başlığı 1796 bayt.
        let (page, lines) = largest_accepted_page_geometry();
        let input_per_page = 1796 + 2 * lines.div_ceil(256);
        let pages = MAX_JOB_RASTER_BYTES / page + 1;
        let attacker_bytes = pages * input_per_page;

        assert!(
            attacker_bytes < MAX_JOB_RASTER_BYTES / 1000,
            "genişleme oranı bu testin varsayımından düşük; ölçümü gözden geçirin"
        );

        // Asıl garanti: girdi ne kadar küçük olursa olsun, işlenen hacim
        // bütçeyi aşamaz.
        let mut budget = JobBudget::default();
        let mut processed = 0u64;
        for _ in 0..pages {
            match budget.account_page(page, 1) {
                Ok(_) => processed += page,
                Err(_) => break,
            }
        }
        assert!(
            processed <= MAX_JOB_RASTER_BYTES,
            "işlenen hacim ({}) bütçeyi ({}) aştı",
            processed,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// Sınırın tam üstündeki bir iş sorunsuz işlenmeli.
    #[test]
    fn test_page_count_limit_is_not_off_by_one() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB))),
            &mut out,
            &current_service_date(),
        )
        .expect("tam sınır kadar sayfa kabul edilmeli");
        assert!(out.ends_with(spl::PJL_END));
    }

    #[test]
    fn test_validate_page_header_rejects_oversized_fields() {
        let mut over_dpi = valid_header();
        over_dpi.hw_resolution = [9999, 9999];
        assert!(validate_page_header(&over_dpi).is_err());

        let mut over_points = valid_header();
        over_points.page_size_points = [999_999, 999_999];
        assert!(validate_page_header(&over_points).is_err());

        let mut over_lines = valid_header();
        over_lines.height = 9_999_999;
        assert!(validate_page_header(&over_lines).is_err());

        let mut over_bpl = valid_header();
        over_bpl.width = 9_999_999;
        over_bpl.bytes_per_line = 9_999_999;
        assert!(validate_page_header(&over_bpl).is_err());
    }

    // ======================================================================
    // Şerit yüksekliği: SpliX compress.cpp `_compressBandedPage`
    //   if (xResolution == 300 && yResolution == 300) bandHeight /= 2;
    // ======================================================================

    #[test]
    fn test_band_height_is_halved_only_at_300x300() {
        let with = |x, y| {
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            band_height_for(h.hw_resolution)
        };
        assert_eq!(
            with(300, 300),
            64,
            "300x300 DPI'da şerit yüksekliği 64 olmalı"
        );
        assert_eq!(with(600, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(1200, 1200), QPDL_BAND_HEIGHT);
        // Kural İKİ eksenin de 300 olmasını istiyor; asimetrik modlar 128'de kalır.
        assert_eq!(with(1200, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(300, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(600, 300), QPDL_BAND_HEIGHT);
    }

    /// Uçtan uca: 300 DPI bir iş, tele GERÇEKTEN 64 satırlık şerit kayıtları
    /// yazmalı. Regresyon değeri buradadır — `band_height_for` doğru olsa bile
    /// çağrı yerinde kullanılmazsa bu test kırılır.
    #[test]
    fn test_300dpi_job_writes_64_line_band_records() {
        let spec = RasterSpec::a4(300, 300, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 1);
        let bands = &pages[0].bands;
        assert_eq!(
            bands.len(),
            200_usize.div_ceil(64),
            "200 satır / 64 = 4 şerit"
        );
        for (i, b) in bands.iter().enumerate() {
            assert_eq!(b.height_lines, 64, "şerit {} yüksekliği 64 olmalı", i);
            assert_eq!(b.index, i as u8);
        }
    }

    #[test]
    fn test_600dpi_job_still_writes_128_line_band_records() {
        let spec = RasterSpec::a4(600, 600, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        let bands = &pages[0].bands;
        assert_eq!(
            bands.len(),
            200_usize.div_ceil(128),
            "200 satır / 128 = 2 şerit"
        );
        assert!(bands.iter().all(|b| b.height_lines == 128));
    }

    /// Asimetrik `1200x600dpi` modu 300 DPI kuralına takılmamalı.
    #[test]
    fn test_asymmetric_1200x600_keeps_128_line_bands() {
        let spec = RasterSpec::a4(1200, 600, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert!(pages[0].bands.iter().all(|b| b.height_lines == 128));
    }

    /// PPD'nin sunduğu HER çözünürlük için şerit yüksekliği SpliX kuralıyla
    /// aynı olmalı. PPD'ye yeni bir çözünürlük eklenirse bu test onu kapsar.
    #[test]
    fn test_band_height_matches_splix_rule_for_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            let expected = if x == 300 && y == 300 { 64 } else { 128 };
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            assert_eq!(
                band_height_for(h.hw_resolution),
                expected,
                "PPD {}x{} DPI sunuyor; SpliX kuralına göre şerit yüksekliği {} olmalı",
                x,
                y,
                expected
            );
        }
    }

    // ======================================================================
    // Duplex / tumble: SpliX request.cpp (mod seçimi) + qpdl.cpp (baytlar)
    // ======================================================================

    #[test]
    fn test_duplex_mode_maps_cups_duplex_and_tumble() {
        let mode = |duplex, tumble| {
            let mut h = valid_header();
            h.duplex = duplex;
            h.tumble = tumble;
            duplex_mode(h.duplex, h.tumble)
        };
        assert_eq!(mode(false, false), SplDuplex::Simplex);
        assert_eq!(
            mode(false, true),
            SplDuplex::Simplex,
            "Duplex kapalıyken Tumble yok sayılır"
        );
        // ML-2160 ailesi `*QPDL ManualDuplex: "On"` bildirdiği için sonuç
        // daima Manual* olmalı; otomatik LongEdge/ShortEdge bu ailede yanlış.
        assert_eq!(mode(true, false), SplDuplex::ManualLongEdge);
        assert_eq!(mode(true, true), SplDuplex::ManualShortEdge);
    }

    /// Tek taraflı işlerde tumble baytı her sayfada 0, duplex baytı 1 olmalı.
    /// (SpliX: Simplex -> duplex = 1, tumble = 0.)
    #[test]
    fn test_simplex_pages_have_duplex_byte_one_and_no_tumble() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.pages = 3;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 3);
        for (i, p) in pages.iter().enumerate() {
            assert_eq!(
                p.header[0xB],
                1,
                "sayfa {}: Simplex duplex baytı 1 olmalı",
                i + 1
            );
            assert_eq!(
                p.header[0xC],
                0,
                "sayfa {}: Simplex tumble baytı 0 olmalı",
                i + 1
            );
        }
    }

    /// Elle duplex'te tumble, SAYFA NUMARASININ paritesidir ve sayaç 1'den
    /// başlar: tek numaralı sayfalarda 1, çift numaralılarda 0.
    /// Eski kod burada koşulsuz 0 yazıyordu.
    #[test]
    fn test_manual_duplex_tumble_alternates_from_page_one() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.pages = 4;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 4);
        let tumbles: Vec<u8> = pages.iter().map(|p| p.header[0xC]).collect();
        assert_eq!(
            tumbles,
            vec![1, 0, 1, 0],
            "tumble = pageNr % 2 (pageNr 1 tabanlı)"
        );
        for p in &pages {
            assert_eq!(p.header[0xB], 0, "elle duplex'te duplex baytı 0 olmalı");
        }
    }

    /// Elle duplex PJL'de `DUPLEX=ON` değil `DUPLEX=MANUAL` demeli
    /// (SpliX printer.cpp sendPJLHeader).
    #[test]
    fn test_manual_duplex_job_sends_pjl_duplex_manual() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        let out = run_filter(spec.build());
        let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
        assert!(pjl.contains("@PJL SET DUPLEX=MANUAL\n"), "PJL: {}", pjl);
        assert!(pjl.contains("@PJL SET BINDING=LONGEDGE\n"), "PJL: {}", pjl);
        assert!(
            !pjl.contains("@PJL SET DUPLEX=ON"),
            "elle duplex ON bildirmemeli: {}",
            pjl
        );
    }

    #[test]
    fn test_short_edge_manual_duplex_sends_shortedge_binding() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.tumble = true;
        let out = run_filter(spec.build());
        let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
        assert!(pjl.contains("@PJL SET DUPLEX=MANUAL\n"), "PJL: {}", pjl);
        assert!(pjl.contains("@PJL SET BINDING=SHORTEDGE\n"), "PJL: {}", pjl);
    }

    /// CUPS raster başlığındaki `Tumble` alanı 368. baytta (cupsWidth'ten
    /// hemen önce) okunmalı. Alan daha önce `turn_off` adıyla duruyordu ve
    /// hiç kullanılmadığı için yanlış adlandırma fark edilmiyordu.
    #[test]
    fn test_tumble_is_parsed_from_offset_368() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.tumble = true;
        let stream = spec.build();
        let header = PageHeader::parse(&stream[4..4 + 1796], CupsRasterVersion::V3Be).unwrap();
        assert!(header.tumble, "368. bayttaki Tumble alanı okunmadı");
        assert!(header.duplex);
    }

    // ======================================================================
    // Kağıt boyutu eşlemesi
    // ======================================================================

    /// PPD'nin sunduğu her kağıt boyutu, QPDL'nin doğru kağıt koduna
    /// eşlenmeli. PPD ile tablo arasındaki her sapma, meşru bir işi reddeder;
    /// bu test iki listeyi birlikte güncel tutar.
    #[test]
    fn test_every_ppd_paper_size_maps_to_its_qpdl_code() {
        use spl::SplPaperSize;

        let expected = |name: &str| -> SplPaperSize {
            match name {
                "A4" => SplPaperSize::A4,
                "Letter" => SplPaperSize::Letter,
                "Legal" => SplPaperSize::Legal,
                "Executive" => SplPaperSize::Executive,
                "A5" => SplPaperSize::A5,
                "A6" => SplPaperSize::A6,
                "B5" => SplPaperSize::B5,
                "Env10" => SplPaperSize::Env10,
                "EnvDL" => SplPaperSize::Dl,
                "EnvC5" => SplPaperSize::C5,
                "Folio" => SplPaperSize::Folio,
                other => panic!("PPD'de tabloya eklenmemiş kağıt boyutu: {}", other),
            }
        };

        let papers = ppd_paper_dimensions();
        assert_eq!(
            papers.len(),
            11,
            "PPD'den beklenen sayıda kağıt boyutu okunamadı"
        );
        for (name, w, h) in papers {
            assert_eq!(
                SplPaperSize::from_dimensions_pt_exact(w, h),
                Some(expected(&name)),
                "PPD '{}' = {}x{} pt, ama kesin eşleme başka bir kod veriyor",
                name,
                w,
                h
            );
        }
    }

    /// Folio 210x330 mm'dir (595x935 pt). 612x936 pt olan 8.5x13 inç ölçüsü
    /// Adobe adlandırmasında FanFoldGermanLegal'dir ve Folio değildir;
    /// `cupstestppd` de PPD'yi tam bu gerekçeyle uyarıyordu.
    #[test]
    fn test_folio_is_f4_not_fanfold_german_legal() {
        use spl::SplPaperSize;
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(595, 935),
            Some(SplPaperSize::Folio)
        );
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(935, 595),
            Some(SplPaperSize::Folio)
        );
        // 8.5x13 inç Folio'ya ya da sessizce A4'e eşlenmemeli.
        assert_eq!(SplPaperSize::from_dimensions_pt_exact(612, 936), None);
    }

    // ======================================================================
    // Kağıt kaynağı (PPD *InputSlot -> CUPS MediaPosition -> QPDL 0x9 baytı)
    // ======================================================================

    /// PPD'nin sunduğu her kağıt kaynağı, QPDL'nin doğru kaynak koduna
    /// eşlenmeli.
    ///
    /// PPD `*InputSlot` seçeneklerini doğrudan QPDL kodlarıyla
    /// numaralandırıyor (`<</MediaPosition 1>>` = Auto). Bağ bir yorum
    /// olarak kalırsa, PPD'ye QPDL kodu olmayan bir değer eklendiğinde
    /// (eskiden Auto = 0 idi) seçenek sessizce Auto'ya düşer.
    #[test]
    fn test_every_ppd_input_slot_maps_to_its_qpdl_code() {
        use spl::SplPaperSource;

        let ppd = ppd_text();

        let expected = |name: &str| -> SplPaperSource {
            match name {
                "Auto" => SplPaperSource::Auto,
                "Manual" => SplPaperSource::Manual,
                "Multi" => SplPaperSource::Multi,
                "Upper" => SplPaperSource::Upper,
                "Lower" => SplPaperSource::Lower,
                other => panic!("PPD'de tabloya eklenmemiş kağıt kaynağı: {}", other),
            }
        };

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*InputSlot ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("bozuk *InputSlot satırı");
            let name = name.split('/').next().unwrap().trim();
            // "<</MediaPosition 2>>setpagedevice" -> 2
            let pos: u32 = code
                .split("MediaPosition")
                .nth(1)
                .expect("MediaPosition yok")
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .expect("MediaPosition sayı değil");

            assert_eq!(
                SplPaperSource::from_media_position(pos),
                Some(expected(name)),
                "PPD '{}' = MediaPosition {}, ama filtre başka bir koda eşliyor",
                name,
                pos
            );
            checked += 1;
        }
        assert_eq!(
            checked, 2,
            "PPD'den beklenen sayıda kağıt kaynağı okunamadı"
        );
    }

    /// Uçtan uca: "Manual Feeder" seçimi QPDL sayfa başlığının 0x9 baytına
    /// ulaşmalı. Yamadan önce bu bayt koşulsuz 1 (Auto) idi.
    #[test]
    fn test_input_slot_reaches_qpdl_page_header() {
        for (media_position, expected) in [(1u32, 1u8), (2, 2), (0, 1)] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_position = media_position;
            let pages = parse_spl(&run_filter(spec.build()));
            assert_eq!(
                pages[0].header[0x9], expected,
                "MediaPosition {} -> QPDL kaynak kodu {} olmalı",
                media_position, expected
            );
        }
    }

    /// Tanınmayan bir `MediaPosition` sessizce yanlış bir koda dönüşmemeli;
    /// Auto'ya düşmeli.
    #[test]
    fn test_unknown_media_position_falls_back_to_auto() {
        use spl::SplPaperSource;
        assert_eq!(SplPaperSource::from_media_position(6), None);
        assert_eq!(SplPaperSource::from_media_position(u32::MAX), None);

        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.media_position = 6;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages[0].header[0x9], 1, "tanınmayan kaynak Auto'ya düşmeli");
    }

    // ======================================================================
    // Kağıt türü (PPD *MediaType -> CUPS MediaType -> @PJL SET PAPERTYPE)
    // ======================================================================

    /// PPD'nin sunduğu HER kağıt türü anahtarı, filtre tarafından da
    /// tanınmalı. Aksi hâlde kullanıcının seçimi sessizce `OFF`'a düşer ve
    /// zarf/etiket düz kağıt füzer ayarlarıyla basılır.
    #[test]
    fn test_every_ppd_media_type_is_accepted_by_the_filter() {
        let ppd = ppd_text();

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*MediaType ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("bozuk *MediaType satırı");
            let name = name.split('/').next().unwrap().trim();

            assert_eq!(
                spl::pjl_paper_type(name),
                Some(name),
                "PPD '{}' sunuyor ama filtrenin PJL sözlüğünde yok",
                name
            );
            // Seçim raster başlığına ulaşmalı: PostScript kodu MediaType
            // dizesini anahtarın KENDİSİYLE ayarlamalı, aksi hâlde filtre
            // farklı bir değer görür.
            assert!(
                code.contains(&format!("MediaType({})", name)),
                "PPD '{}' seçimi raster başlığına aynı anahtarla ulaşmıyor: {}",
                name,
                line
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            spl::PJL_PAPER_TYPES.len(),
            "PPD, yazıcının PJL sözlüğündeki türlerin tamamını sunmuyor"
        );
    }

    /// Uçtan uca: seçilen kağıt türü PJL başlığına ulaşmalı. Yamadan önce
    /// burada her zaman `PAPERTYPE=OFF` yazıyordu.
    #[test]
    fn test_media_type_reaches_pjl_papertype() {
        for media_type in ["ENV", "LABEL", "THICK", "OFF"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains(&format!("@PJL SET PAPERTYPE={}\n", media_type)),
                "{} PJL'e ulaşmadı: {}",
                media_type,
                pjl
            );
        }
    }

    /// Tanınmayan ya da boş bir `MediaType` güvenli varsayılana düşmeli ve
    /// asla ham olarak PJL satırına yazılmamalı (satır tırnaksızdır: bir
    /// boşluk ya da CR/LF komutu bozardı).
    #[test]
    fn test_unknown_media_type_falls_back_to_papertype_off() {
        for media_type in ["", "Envelope", "Plain", "EVIL VALUE"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains("@PJL SET PAPERTYPE=OFF\n"),
                "{:?} için OFF'a düşülmedi: {}",
                media_type,
                pjl
            );
            assert!(
                !pjl.contains("PAPERTYPE=EVIL"),
                "güvenilmez değer PJL satırına sızdı: {}",
                pjl
            );
        }
    }

    /// PPD varsayılanları filtrenin varsayılanlarıyla uyuşmalı: PPD
    /// `*DefaultMediaType: OFF` / `*DefaultInputSlot: Auto` diyorsa, hiçbir
    /// seçim yapılmamış bir iş de aynı sonucu üretmeli.
    #[test]
    fn test_ppd_defaults_match_filter_defaults() {
        let ppd = ppd_text();

        let default_of = |key: &str| -> String {
            ppd.lines()
                .find_map(|l| l.strip_prefix(key))
                .unwrap_or_else(|| panic!("{} yok", key))
                .trim()
                .to_string()
        };
        assert_eq!(default_of("*DefaultMediaType:"), spl::PJL_PAPERTYPE_DEFAULT);
        assert_eq!(default_of("*DefaultInputSlot:"), "Auto");
    }
}
