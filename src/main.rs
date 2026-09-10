//! The frozen 1.x CUPS raster filter.
//!
//! The protocol engine moved to the `spl2-core` crate during the PAPPL
//! migration; what stays here is the CUPS filter front end — argv, stdin,
//! stderr and the page loop that drives the engine. Decision Q-5 freezes this
//! binary's behaviour, so the golden corpus must not move when this file does.

use std::env;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::process;

use spl2_core::geometry::quote_untrusted;
use spl2_core::log::{Level, Log};
use spl2_core::media;
use spl2_core::qpdl::current_service_date;
use spl2_core::replay::{process_with_margin as replay_with_margin, JobIdentity};

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
/// The loop itself now lives in `spl2_core::replay`, so that the golden corpus
/// outlives this binary: gate P11 deletes the front end, not the harness. What
/// stays here is what a CUPS filter is — argv, stdin, stderr — and these two
/// wrappers, which supply the stderr sink and the two argv fields the loop
/// reads. Behaviour is unchanged, and the goldens are what says so.
fn process_cups_raster_to_spl<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
) -> io::Result<()> {
    process_with_margin(args, reader, writer, service_date, media::HARD_MARGIN_PT)
}

/// The production path always supplies the driver constant. The explicit input
/// also preserves synthetic geometry cases in the golden harness.
fn process_with_margin<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
    margin_pt: f64,
) -> io::Result<()> {
    replay_with_margin(
        &identity_of(args),
        reader,
        writer,
        service_date,
        margin_pt,
        &CupsFilterLog,
    )
}

/// The job title and user, which are the only argv fields the loop reads.
fn identity_of(args: &CupsFilterArgs) -> JobIdentity {
    JobIdentity {
        title: args.title.clone(),
        user: args.user.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The engine moved to `spl2-core`; these tests still exercise it, and are
    // the reason the split has to be behaviour preserving rather than merely
    // compiling.
    use spl2_core::geometry::*;
    // The engine's own types, imported here rather than at the top of the file:
    // the front end that survives P11 does not name them, and the test module
    // does. See the `replay` module for where the loop went.
    use spl2_core::qpdl::{self as spl, Algo0x11, SplDuplex, SplResolution};
    use spl2_core::raster::{CupsRasterVersion, PageHeader};
    use spl2_core::replay::validate_page_header;
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
                put(312, self.margin_left_pt); // Margins[0] (left, pt)
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
        // The record is located deterministically: searching for a 0x0C byte
        // is not safe, because byte 0x4 of the page header (the EnvIsoB5 paper
        // code) can be 0x0C as well.
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let pos = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("no QPDL language-switch line in the stream")
            + QPDL_MARK.len()
            + 17; // past the 17-byte page header
        assert_eq!(out[pos], 0x0C, "expected a band record signature");
        assert_eq!(out[pos + 6], 0x11, "expected Algo 0x11");
        let total = u32::from_be_bytes(out[pos + 7..pos + 11].try_into().unwrap()) as usize;
        // 11-byte record header + 4-byte sub-header; the trailing 4 bytes are
        // the checksum.
        let payload = &out[pos + 15..pos + 11 + total - 4];
        let mut band = Algo0x11::decompress(payload);
        // `stream_page_bands` inverts the buffer immediately before writing
        // it; undo that here.
        for b in &mut band {
            *b = !*b;
        }
        band
    }

    /// The column indices of the non-zero bytes on the first line (`y == 0`)
    /// of a transposed band buffer.
    fn nonzero_columns_in_first_line(band: &[u8], band_height: usize) -> Vec<usize> {
        band.chunks(band_height)
            .enumerate()
            .filter(|(_, col)| col[0] != 0)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// The header fields of one band record in the produced SPL stream.
    #[derive(Debug, PartialEq, Eq)]
    struct BandRecord {
        index: u8,
        width_px: u16,
        height_lines: u16,
    }

    /// One page of the produced SPL stream: a 17-byte header and its bands.
    #[derive(Debug)]
    struct SplPage {
        header: [u8; 17],
        bands: Vec<BandRecord>,
    }

    /// Parses the SPL output along its real record structure, so that a test
    /// asserts on the bytes written to the wire rather than on its own
    /// assumptions about them.
    fn parse_spl(out: &[u8]) -> Vec<SplPage> {
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let start = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("no QPDL language-switch line in the stream")
            + QPDL_MARK.len();

        let mut pages = Vec::new();
        let mut pos = start;
        while pos < out.len() && !out[pos..].starts_with(spl::PJL_END) {
            assert_eq!(out[pos], 0x00, "expected a page header signature @ {}", pos);
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
                // 11-byte record header + (sub-header + payload + checksum)
                pos += 11 + total;
            }

            assert_eq!(
                out[pos], 0x01,
                "expected an end-of-page signature @ {}",
                pos
            );
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
        .expect("the filter failed on a valid stream");
        out
    }

    fn count_uel(stream: &[u8]) -> usize {
        stream
            .windows(spl::PJL_UEL.len())
            .filter(|w| *w == spl::PJL_UEL)
            .count()
    }

    /// Y-03 regression, end to end: truncated page data must make the
    /// conversion return an error, BUT the stream sent to the printer must
    /// still end with the closing UEL. Otherwise the printer is left in the
    /// QPDL language, waiting for the rest of a half-written band record.
    #[test]
    fn test_closing_uel_written_on_truncated_page_data() {
        let stream = v3_stream(2); // 4 lines x 4 bytes = 16 bytes were required
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("short page data should have returned an error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            out.ends_with(spl::PJL_END),
            "the error path left the stream without its closing UEL"
        );
        assert_eq!(count_uel(&out), 2);
    }

    /// Y-03 regression: the same guarantee when a page header fails
    /// validation.
    #[test]
    fn test_closing_uel_written_on_invalid_page_header() {
        let mut stream = v3_stream(16);
        // Set bytes_per_line to 0, which validate_page_header must reject.
        stream[4 + 392..4 + 396].copy_from_slice(&0u32.to_be_bytes());

        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("an invalid header should have returned an error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(out.ends_with(spl::PJL_END));
    }

    /// A stream with no pages at all must still close the job: `begin_job`
    /// has already put the printer into the QPDL language.
    #[test]
    fn test_closing_uel_written_when_stream_has_no_pages() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(b"RaS3".to_vec())),
            &mut out,
            &current_service_date(),
        )
        .expect("a page-less stream must warn rather than fail");
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2);
    }

    /// A successful stream carries exactly two UELs: `Drop` must not append a
    /// third one to a job that was closed explicitly.
    #[test]
    fn test_successful_stream_has_exactly_one_uel_pair() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_stream(16))),
            &mut out,
            &current_service_date(),
        )
        .expect("a valid stream must succeed");
        assert!(out.starts_with(spl::PJL_UEL));
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2, "an extra UEL was written");
    }

    /// Y-01 regression, end to end: a compressed v2 stream must produce
    /// BYTE-IDENTICAL SPL output to the uncompressed v3 form of the same
    /// content.
    ///
    /// v2 page data used to be taken for raw pixels, and a garbage page went to
    /// the printer. This test pins that `CupsLineDecoder` sits in the path and
    /// that the stream version has no effect on the output at all.
    #[test]
    fn test_v2_and_v3_streams_produce_identical_output() {
        // 4 bytes per line, 3 lines: 0xAA 0xAA 0xAA 0x55 / 0x00 x4 / 0xFF x4
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

        // The same content as CUPS line-RLE.
        let mut v2 = b"RaS2".to_vec();
        v2.extend_from_slice(&hdr);
        v2.extend_from_slice(&[0x00, 0x02, 0xAA, 0x00, 0x55]); // 0xAA x3, 0x55 x1
        v2.extend_from_slice(&[0x00, 0x80]); // blank to end of line (K => 0x00)
        v2.extend_from_slice(&[0x00, 0x03, 0xFF]); // 0xFF x4

        let mut out_v3: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3)),
            &mut out_v3,
            &current_service_date(),
        )
        .expect("the v3 stream must be processed");
        let mut out_v2: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v2)),
            &mut out_v2,
            &current_service_date(),
        )
        .expect("the v2 stream must be processed");

        assert!(!out_v3.is_empty());
        assert_eq!(out_v2, out_v3, "the v2 and v3 outputs diverged");
    }

    /// `validate_page_header`'s limits must cover EVERY option the PPD offers.
    ///
    /// The constants were derived from the PPD by hand and the link between
    /// them was only a comment: add a larger sheet or a higher resolution to
    /// the PPD, forget to update the constants, and legitimate jobs would be
    /// refused in silence. This test makes that link mandatory.
    #[test]
    fn test_limits_cover_every_ppd_option() {
        for (x_dpi, y_dpi) in ppd_resolutions() {
            assert!(
                SplResolution::pair_is_supported(x_dpi, y_dpi),
                "the PPD offers {}x{} DPI but the filter does not support that pair",
                x_dpi,
                y_dpi
            );
            for dpi in [x_dpi, y_dpi] {
                assert!(
                    dpi <= MAX_DPI,
                    "the PPD offers {} DPI but MAX_DPI = {}; update the constant",
                    dpi,
                    MAX_DPI
                );
            }
        }

        for (name, w, h) in ppd_paper_dimensions() {
            for pt in [w, h] {
                assert!(
                    pt <= MAX_POINTS,
                    "the PPD offers '{}' at {} pt but MAX_POINTS = {}; update the constant",
                    name,
                    pt,
                    MAX_POINTS
                );
                // The largest sheet at the highest resolution must also fit
                // the line and column limits.
                let pixels = (pt as u64 * MAX_DPI as u64).div_ceil(72);
                assert!(
                    pixels <= MAX_LINES as u64,
                    "{} pt @ {} DPI = {} lines, MAX_LINES = {}",
                    pt,
                    MAX_DPI,
                    pixels,
                    MAX_LINES
                );
                assert!(
                    pixels.div_ceil(8) <= MAX_BYTES_PER_LINE as u64,
                    "{} pt @ {} DPI = {} bytes per line, MAX_BYTES_PER_LINE = {}",
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
        header.page_size_points = [297, 420]; // A6, a supported size
        header.width = 4960;
        header.bytes_per_line = 620; // consistent with cupsWidth, not with the page
        let err =
            validate_page_header(&header).expect_err("a too-narrow page should have been refused");
        assert!(
            err.to_string().contains("does not fit the page width"),
            "the reason must be explained: {}",
            err
        );
    }

    /// A real full-width A4 page (595 pt @ 600 DPI = 620 bytes per line) must
    /// not be refused — the over-correction check.
    #[test]
    fn test_validate_page_header_accepts_full_width_a4() {
        let mut header = valid_header();
        header.width = 4960;
        header.bytes_per_line = 620;
        assert!(validate_page_header(&header).is_ok());
    }

    /// Rounding slack: one byte of overshoot is tolerated, two are refused.
    #[test]
    fn test_validate_page_header_line_width_slack_is_one_byte() {
        // 595 pt @ 600 DPI => 4960 px => a 620-byte band width.
        let mut ok = valid_header();
        ok.width = 4968; // 621 bytes
        ok.bytes_per_line = 621;
        assert!(
            validate_page_header(&ok).is_ok(),
            "one byte of slack must be accepted"
        );

        let mut too_wide = valid_header();
        too_wide.width = 4976; // 622 bytes
        too_wide.bytes_per_line = 622;
        assert!(
            validate_page_header(&too_wide).is_err(),
            "two bytes of overshoot must be refused"
        );
    }

    /// D-02: `cupsHeight` must fit the sheet's physical height — the vertical
    /// counterpart of D-01. Before the fix this header was accepted.
    #[test]
    fn test_validate_page_header_rejects_page_taller_than_paper() {
        let mut header = valid_header();
        header.page_size_points = [297, 420]; // A6, a supported size
        header.height = 4_000; // within MAX_LINES, but taller than A6
        let err =
            validate_page_header(&header).expect_err("a too-tall page should have been refused");
        assert!(
            err.to_string().contains("does not fit the page height"),
            "the reason must be explained: {}",
            err
        );

        // The overshoot must also be caught on an ordinary A4 page: 842 pt @
        // 600 DPI = 7017 lines, and the raster has to stay under that.
        let mut a4 = valid_header();
        a4.height = 24_000;
        assert!(
            validate_page_header(&a4).is_err(),
            "a height that does not fit A4 must be refused"
        );
    }

    /// The over-correction check: heights that real `cupsfilter` output
    /// produces must not be refused. The values were measured by running
    /// `cupsfilter -m application/vnd.cups-raster` with
    /// ppd/samsung-ml2160.ppd; every one stays under the physical limit
    /// because the `*ImageableArea` margins (12 pt top + 12 pt bottom) are
    /// subtracted.
    #[test]
    fn test_validate_page_header_accepts_real_cupsfilter_heights() {
        // (page_width_pt, page_height_pt, y_dpi, measured cupsHeight)
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
            // The measurements in this table came from symmetric resolutions.
            h.hw_resolution = [ydpi, ydpi];
            h.height = cups_height;
            assert!(
                validate_page_header(&h).is_ok(),
                "real cupsfilter output was refused: {}x{} pt @ {} DPI => {} lines",
                width_pt,
                height_pt,
                ydpi,
                cups_height
            );
        }
    }

    /// Rounding slack: eight lines of overshoot are tolerated, more are
    /// refused.
    #[test]
    fn test_validate_page_header_height_slack_is_eight_lines() {
        // 842 pt @ 600 DPI => ceil(842 * 600 / 72) = 7017 lines.
        let exact = compute_page_height_lines(842, 600);
        assert_eq!(exact, 7017);

        let mut ok = valid_header();
        ok.height = exact + 8;
        assert!(
            validate_page_header(&ok).is_ok(),
            "eight lines of slack must be accepted"
        );

        let mut too_tall = valid_header();
        too_tall.height = exact + 9;
        assert!(
            validate_page_header(&too_tall).is_err(),
            "nine lines of overshoot must be refused"
        );
    }

    /// D-06: the hard margin must be computed exactly as SpliX computes it.
    ///
    /// SpliX compress.cpp: `((ceil(marginPt * dpi / 72) + 7) & ~7) / 8`.
    /// Rounding UP to 8 matters: 12 pt @ 600 DPI is 100 pixels, which aligns to
    /// 104 pixels = 13 bytes; without the alignment it is 12.5 bytes (12 once
    /// truncated) and the band shifts by one byte.
    #[test]
    fn test_hard_margin_matches_splix_alignment() {
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 300), 7);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 600), 14);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 1200), 27);

        // The *ImageableArea left margin in this project's PPD: 12 pt.
        assert_eq!(hard_margin_bytes(12.0, 600), 13);
        assert_eq!(hard_margin_bytes(12.0, 300), 7);
        assert_eq!(hard_margin_bytes(12.0, 1200), 25);
        // No declared margin means no shift either.
        assert_eq!(hard_margin_bytes(0.0, 600), 0);
    }

    /// D-06 regression: horizontal placement must be CENTRING MINUS THE HARD
    /// MARGIN, not centring alone.
    ///
    /// Only `(bandWidthInB - lineSize) / 2` used to be applied; on A4 @ 600 DPI
    /// that meant a 12-byte (96 pixel, about 4 mm) shift to the right, because
    /// SpliX SKIPS `hardMarginXInB` (13 bytes) while filling the band
    /// (compress.cpp:227). The net offset is zero.
    #[test]
    fn test_band_placement_subtracts_hard_margin() {
        // A4 @ 600 DPI, the real numbers from this project's PPD:
        // a 620-byte band, a 595-byte CUPS line, a 13-byte hard margin.
        let a4 = band_placement(620, 595, hard_margin_bytes(12.0, 600)).unwrap();
        assert_eq!(
            a4,
            BandPlacement {
                dst_offset: 0,
                src_skip: 1
            },
            "centring (12 B) must offset the hard margin (13 B)"
        );

        // The regression anchor: without subtracting the hard margin, the old
        // and wrong 12-byte shift comes back.
        assert_eq!(
            band_placement(620, 595, 0).unwrap(),
            BandPlacement {
                dst_offset: 12,
                src_skip: 0
            },
            "with no margin the behaviour is plain centring"
        );

        // When centring is LARGER than the hard margin, the difference stays
        // in the destination.
        assert_eq!(
            band_placement(620, 560, 13).unwrap(),
            BandPlacement {
                dst_offset: 17,
                src_skip: 0
            }
        );

        // A band narrower than the line cannot be centred; the margin is
        // skipped out of the line instead.
        assert_eq!(
            band_placement(600, 620, 13).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 13
            }
        );

        // Skipping the whole line means the geometry is inconsistent: it is
        // refused rather than silently clamped (`src_skip` used to be clamped
        // to 3, producing a one-byte, effectively blank page).
        let err = band_placement(4, 4, 999)
            .expect_err("a hard margin wider than the line should have been refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("the hard margin"), "{}", err);

        // The boundary: exactly one byte left over is still accepted.
        assert_eq!(
            band_placement(4, 4, 3).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 3
            }
        );
    }

    /// D-07: with `Margins[0]` unvalidated, the `px + 7` addition inside
    /// `hard_margin_bytes` overflowed — a panic in the middle of a job where
    /// `overflow-checks` is on, and a silent wrap to 0 in release builds. A
    /// left margin wider than the page is now refused by header validation.
    #[test]
    fn test_validate_page_header_rejects_out_of_page_left_margin() {
        for margin in [595, 600, 300_000_000] {
            let mut header = valid_header();
            header.margins[0] = margin;
            let err = validate_page_header(&header)
                .expect_err("a left margin wider than the page should have been refused");
            assert!(
                err.to_string().contains("invalid left margin"),
                "the wrong error for {} pt: {}",
                margin,
                err
            );
        }

        // The PPD's real value (12 pt) and just under the limit must both be
        // accepted.
        for margin in [0, 12, 594] {
            let mut header = valid_header();
            header.margins[0] = margin;
            assert!(
                validate_page_header(&header).is_ok(),
                "{} pt should have been accepted",
                margin
            );
        }
    }

    /// D-06 regression, end to end: raster content must land in the band
    /// buffer sent to the printer at the column the hard margin leaves it in.
    ///
    /// Real A4 @ 600 DPI geometry is set up (a 620-byte band, a 595-byte CUPS
    /// line, `Margins[0] = 12 pt`) and a mark is placed in byte 3 of the line.
    /// The expected column is `3 - src_skip + dst_offset = 2`; before the fix
    /// the same byte was written to column 15 (a 12-byte shift plus 3).
    #[test]
    fn test_content_lands_at_hard_margin_corrected_column() {
        const MARKER_INDEX: usize = 3;
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760; // 595 bytes per line, as real cupsfilter output has
        spec.margin_left_pt = 12; // PPD *ImageableArea: "12 12 583 830"

        let mut pattern = vec![0u8; 595];
        pattern[MARKER_INDEX] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);
        assert_eq!(band.len(), 620 * QPDL_BAND_HEIGHT, "band buffer size");

        let columns = nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT);
        assert_eq!(
            columns,
            vec![1],
            "the marker byte is in the wrong column; 15 means the hard margin is not
             being subtracted"
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

    /// The horizontal and vertical axes must use the SAME origin.
    ///
    /// This was the heart of the bug: nothing shifted vertically while the
    /// horizontal axis shifted by 12 bytes. In SpliX the net offset is zero on
    /// both axes (centring minus the hard margin). This test pins the first
    /// raster line to line 0 and column 0 of the band buffer.
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

        // Column 0, line 0: the content starts at the band's top-left corner.
        assert_eq!(
            band[0], 0xFF,
            "content must start at the band's (0,0) corner"
        );
        assert_eq!(
            nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT),
            vec![0]
        );
    }

    /// The height limit must follow the vertical resolution:
    /// `compute_page_height_lines` takes `hw_resolution[1]`, not `[0]`. 1200x600
    /// is a supported mode, so the two axes have to be used independently.
    #[test]
    fn test_page_height_lines_uses_vertical_resolution() {
        assert_eq!(compute_page_height_lines(842, 600), 7017);
        assert_eq!(compute_page_height_lines(842, 300), 3509);
        assert_eq!(compute_page_height_lines(842, 1200), 14034);

        // The limit really does scale with resolution: a height that is valid
        // at 600 DPI must be refused at 300 DPI on the same sheet.
        let mut h300 = valid_header();
        h300.hw_resolution = [300, 300];
        h300.width = 2480;
        h300.bytes_per_line = 310;
        h300.height = 6817; // the line count of 600 DPI
        assert!(
            validate_page_header(&h300).is_err(),
            "600 DPI line counts must not be accepted at 300 DPI"
        );

        h300.height = 3408; // the real cupsfilter value for 300 DPI
        assert!(validate_page_header(&h300).is_ok());
    }

    /// An asymmetric resolution (`1200x600dpi`) is a real, SUPPORTED QPDL mode
    /// — SpliX offers the same option in its ml2010/ml2015/ml1640/ml2510/ml2525
    /// PPDs — and must not be refused.
    #[test]
    fn test_validate_page_header_accepts_asymmetric_resolution() {
        // A4 @ 1200x600: the values cupsfilter actually produces.
        let mut h = valid_header();
        h.hw_resolution = [1200, 600];
        h.width = 9517;
        h.bytes_per_line = 1190;
        h.height = 6817;
        assert!(
            validate_page_header(&h).is_ok(),
            "1200x600dpi is a real QPDL mode and must not be refused: {:?}",
            validate_page_header(&h).err()
        );
    }

    /// Every resolution the PPD offers must be accepted by the filter as well;
    /// otherwise the user sees an unexplained "filter failed".
    #[test]
    fn test_filter_accepts_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            // Build a header consistent with A4 at that resolution.
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            h.bytes_per_line = (595 * x).div_ceil(72).div_ceil(8);
            h.width = h.bytes_per_line * 8;
            h.height = (842 * y).div_ceil(72);
            assert!(
                validate_page_header(&h).is_ok(),
                "the PPD offers {}x{} DPI but the filter refuses it: {:?}",
                x,
                y,
                validate_page_header(&h).err()
            );
        }
    }

    /// D-04 regression: `cupsColorOrder` is checked now.
    #[test]
    fn test_validate_page_header_checks_color_order() {
        // For 1-bit single-channel data all three orders are equivalent, so
        // all three are accepted.
        for order in [
            CupsColorOrder::Chunked,
            CupsColorOrder::Banded,
            CupsColorOrder::Planar,
        ] {
            let mut header = valid_header();
            header.color_order = order;
            assert!(
                validate_page_header(&header).is_ok(),
                "{:?} should have been accepted",
                order
            );
        }

        let mut unknown = valid_header();
        unknown.color_order = CupsColorOrder::Unknown(99);
        assert!(
            validate_page_header(&unknown).is_err(),
            "an unrecognised order must be refused"
        );
    }

    /// Builds a V3 stream of the requested number of small, valid pages.
    fn v3_multipage_stream(pages: u32) -> Vec<u8> {
        v3_multipage_stream_with_copies(pages, 0)
    }

    /// The variant of `v3_multipage_stream` that also sets `cupsNumCopies` in
    /// the page header, for exercising the impression (page x copies) budget.
    fn v3_multipage_stream_with_copies(pages: u32, copies: u32) -> Vec<u8> {
        let mut page = vec![0u8; 1796];
        {
            let mut put = |off: usize, val: u32| {
                page[off..off + 4].copy_from_slice(&val.to_be_bytes());
            };
            // The smallest supported sheet and resolution (A6 @ 300 DPI), but
            // only an 8x1 pixel valid raster area. The band buffer stays around
            // 10 KiB per page, which keeps the 5,000-page limit test fast.
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
            stream.push(0u8); // 1 line x 1 byte
        }
        stream
    }

    /// D-04 regression: the page count must have an upper bound.
    #[test]
    fn test_page_count_is_capped() {
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB + 1))),
            &mut out,
            &current_service_date(),
        )
        .expect_err("exceeding the page limit must be an error");
        assert!(
            err.to_string().contains("exceeded the page limit"),
            "{}",
            err
        );
        // Even over the limit the job must be closed properly (the Y-03
        // guarantee).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// The raw raster volume budget must be enforced, and must accept the
    /// limit exactly.
    #[test]
    fn test_job_raster_byte_budget_is_enforced() {
        let mut budget = JobBudget::default();
        budget
            .account_page(MAX_JOB_RASTER_BYTES, 1)
            .expect("the whole budget must be accepted");
        assert_eq!(budget.raster_bytes, MAX_JOB_RASTER_BYTES);

        let err = budget
            .account_page(1, 1)
            .expect_err("one byte over the budget must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeded the raster volume limit"),
            "{}",
            err
        );

        // Saturate rather than wrap: even if the limits are raised later, an
        // overflow must not silently reset the budget.
        let mut huge = JobBudget {
            pages: 0,
            raster_bytes: u64::MAX - 1,
            impressions: 0,
        };
        assert!(huge.account_page(u64::MAX, 1).is_err());
        assert_eq!(huge.raster_bytes, u64::MAX);
    }

    /// FINDING 2: the PRODUCT of the page limit and the copy limit must not be
    /// unbounded.
    ///
    /// This test exists because the two limits looked "reasonable" separately
    /// while together they allowed 4,995,000 impressions with the old values.
    /// The counter is in impressions; a page count alone is not a meaningful
    /// ceiling.
    #[test]
    fn test_page_and_copy_limits_cannot_multiply_without_bound() {
        let unbounded_product = MAX_PAGES_PER_JOB as u64 * MAX_REALISTIC_COPIES as u64;
        assert!(
            MAX_JOB_IMPRESSIONS < unbounded_product,
            "the impression limit must be smaller than the page x copies product \
             ({}); otherwise nothing is limiting anything",
            unbounded_product
        );

        // At the maximum copy count, the number of pages the impression
        // budget allows.
        let mut budget = JobBudget::default();
        let allowed = MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64;
        for page in 1..=allowed {
            budget
                .account_page(1, MAX_REALISTIC_COPIES)
                .unwrap_or_else(|e| panic!("page {} should not have been refused: {}", page, e));
        }
        let err = budget
            .account_page(1, MAX_REALISTIC_COPIES)
            .expect_err("exceeding the impression limit must be an error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeded the sheet limit"),
            "{}",
            err
        );
    }

    /// The impression budget must accept the limit exactly and refuse one
    /// impression more.
    #[test]
    fn test_impression_budget_is_not_off_by_one() {
        let mut budget = JobBudget::default();
        budget
            .account_page(1, u16::try_from(MAX_JOB_IMPRESSIONS).unwrap())
            .expect("impressions up to the exact limit must be accepted");
        assert_eq!(budget.impressions, MAX_JOB_IMPRESSIONS);
        assert!(budget.account_page(1, 1).is_err());
    }

    /// The budget must count the value that came through `sanitize_copies`,
    /// not the RAW `num_copies`: copies that never reach the printer must not
    /// consume budget, and a value such as 65536 must not be counted as
    /// "0 copies" and escape the budget either.
    #[test]
    fn test_impression_budget_counts_sanitized_copies() {
        let mut budget = JobBudget::default();
        // Under the old `as u16` behaviour this became 0; it must count 999.
        budget.account_page(1, sanitize_copies(65_536)).unwrap();
        assert_eq!(budget.impressions, MAX_REALISTIC_COPIES as u64);

        let mut zero = JobBudget::default();
        zero.account_page(1, sanitize_copies(0)).unwrap();
        assert_eq!(
            zero.impressions, 1,
            "0 copies must count as at least 1 impression"
        );
    }

    /// The impression limit must be enforced end to end, and the job must
    /// still close properly.
    #[test]
    fn test_impression_limit_is_enforced_end_to_end() {
        // Every page at the maximum copy count: the limit fills long before
        // the page count does.
        let pages = (MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64) as u32 + 1;
        assert!(
            pages < MAX_PAGES_PER_JOB,
            "the page limit must not bind first"
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
        .expect_err("exceeding the impression limit must be an error");
        assert!(
            err.to_string().contains("exceeded the sheet limit"),
            "{}",
            err
        );
        // Even over the limit the job must be closed properly (the Y-03
        // guarantee).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// The budget must NOT change the behaviour of ordinary-resolution jobs:
    /// `MAX_PAGES_PER_JOB` A4 pages at 600 DPI have to stay under it.
    #[test]
    fn test_job_budget_does_not_bind_for_600dpi_pages_within_page_limit() {
        // Use the larger physical A4 geometry rather than the imageable area,
        // so the limit survives different cups-filters rounding as well.
        let a4_600 = compute_page_width_pixels(595, 600).div_ceil(8) as u64
            * compute_page_height_lines(842, 600) as u64;
        let worst = a4_600 * MAX_PAGES_PER_JOB as u64;
        assert!(
            worst < MAX_JOB_RASTER_BYTES,
            "{} A4 pages at 600 DPI ({} bytes) must not exceed the budget ({})",
            MAX_PAGES_PER_JOB,
            worst,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// The largest raw raster geometry the validator accepts: Legal @ 1200 DPI
    /// plus the D-01/D-02 rounding slack. Unknown sizes are no longer accepted,
    /// so the global `MAX_POINTS` value is not a reachable page geometry.
    fn largest_accepted_page_geometry() -> (u64, u64) {
        let bytes_per_line = compute_page_width_pixels(612, 1200).div_ceil(8) as u64
            + LINE_OVERSHOOT_SLACK_BYTES as u64;
        let lines =
            compute_page_height_lines(1008, 1200) as u64 + HEIGHT_OVERSHOOT_SLACK_LINES as u64;
        (bytes_per_line * lines, lines)
    }

    /// ...but on the largest acceptable page it must REALLY bind, otherwise
    /// the page limit's blindness to resolution has not been closed.
    #[test]
    fn test_job_budget_binds_before_page_limit_at_max_page_size() {
        let (max_page, _) = largest_accepted_page_geometry();
        let pages_allowed = MAX_JOB_RASTER_BYTES / max_page;
        assert!(
            pages_allowed > 0,
            "the budget refuses even a single maximum-sized page"
        );
        assert!(
            pages_allowed < MAX_PAGES_PER_JOB as u64,
            "on maximum-sized pages the budget must bind before the page limit \
             (allowed: {}, page limit: {})",
            pages_allowed,
            MAX_PAGES_PER_JOB
        );
        assert_eq!(
            pages_allowed + 1,
            401,
            "the first refused page quoted in the resource-budget commentary changed"
        );
    }

    /// FINDING 1: the budget must bound worst-case CPU time as well.
    ///
    /// The limit is in bytes, so it binds CPU time only indirectly, through the
    /// compressor's measured throughput. The value below was measured on this
    /// machine, in a release build, at the real band size (346,752 bytes):
    /// incompressible noise runs at about 6.65 MB/s (a zero-filled band runs at
    /// about 166 MB/s, so there is a factor of about 25 between best and worst).
    ///
    /// The point is not to measure a speed — that varies from machine to
    /// machine — but to make the CPU-side cost of raising the budget visible:
    /// the filter processes the CUPS queue on a single thread, so everything
    /// behind it waits for this long.
    const MEASURED_WORST_CASE_COMPRESS_BPS: u64 = 6_650_000;

    #[test]
    fn test_raster_budget_bounds_worst_case_cpu_time() {
        let worst_case_seconds = MAX_JOB_RASTER_BYTES / MEASURED_WORST_CASE_COMPRESS_BPS;
        assert!(
            worst_case_seconds <= 30 * 60,
            "in the worst case a single job blocks the queue for {} seconds; \
             MAX_JOB_RASTER_BYTES ({}) should be lowered",
            worst_case_seconds,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// The core of FINDING 1: the budget counts DECODED raster volume, not
    /// input volume — and the line-RLE of CUPS Raster v2 provides an enormous
    /// expansion in between. At the largest supported geometry an all-white
    /// stream of about 0.74 MiB can produce more than 8 GiB of raster work.
    ///
    /// So "a small input means a small job" cannot be assumed; the ceiling
    /// itself is the only real defence. This test pins that the ceiling is
    /// bounded by the constant alone, NOT by how many bytes an attacker can
    /// send.
    #[test]
    fn test_compressed_input_cannot_amplify_past_the_raster_budget() {
        // One v2 line record is 2 bytes ([repeat][0x80 = blank to end of line])
        // and produces up to 256 lines; the page header is 1796 bytes.
        let (page, lines) = largest_accepted_page_geometry();
        let input_per_page = 1796 + 2 * lines.div_ceil(256);
        let pages = MAX_JOB_RASTER_BYTES / page + 1;
        let attacker_bytes = pages * input_per_page;

        assert!(
            attacker_bytes < MAX_JOB_RASTER_BYTES / 1000,
            "the expansion ratio is lower than this test assumes; revisit the
             measurement"
        );

        // The real guarantee: however small the input, the processed volume
        // cannot exceed the budget.
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
            "the processed volume ({}) exceeded the budget ({})",
            processed,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// A job exactly at the limit must be processed without complaint.
    #[test]
    fn test_page_count_limit_is_not_off_by_one() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB))),
            &mut out,
            &current_service_date(),
        )
        .expect("pages up to the exact limit must be accepted");
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
    // Band height: SpliX compress.cpp `_compressBandedPage`
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
            "the band height must be 64 at 300x300 DPI"
        );
        assert_eq!(with(600, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(1200, 1200), QPDL_BAND_HEIGHT);
        // The rule requires BOTH axes to be 300; asymmetric modes stay at 128.
        assert_eq!(with(1200, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(300, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(600, 300), QPDL_BAND_HEIGHT);
    }

    /// End to end: a 300 DPI job must REALLY write 64-line band records to the
    /// wire. That is where the regression value is — this test breaks if
    /// `band_height_for` is right but is not used at the call site.
    #[test]
    fn test_300dpi_job_writes_64_line_band_records() {
        let spec = RasterSpec::a4(300, 300, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 1);
        let bands = &pages[0].bands;
        assert_eq!(
            bands.len(),
            200_usize.div_ceil(64),
            "200 lines / 64 = 4 bands"
        );
        for (i, b) in bands.iter().enumerate() {
            assert_eq!(b.height_lines, 64, "band {} must be 64 lines high", i);
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
            "200 lines / 128 = 2 bands"
        );
        assert!(bands.iter().all(|b| b.height_lines == 128));
    }

    /// The asymmetric `1200x600dpi` mode must not be caught by the 300 DPI
    /// rule.
    #[test]
    fn test_asymmetric_1200x600_keeps_128_line_bands() {
        let spec = RasterSpec::a4(1200, 600, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert!(pages[0].bands.iter().all(|b| b.height_lines == 128));
    }

    /// For EVERY resolution the PPD offers, the band height must match SpliX's
    /// rule. A new resolution added to the PPD is covered by this test.
    #[test]
    fn test_band_height_matches_splix_rule_for_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            let expected = if x == 300 && y == 300 { 64 } else { 128 };
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            assert_eq!(
                band_height_for(h.hw_resolution),
                expected,
                "the PPD offers {}x{} DPI; by SpliX's rule the band height must be {}",
                x,
                y,
                expected
            );
        }
    }

    // ======================================================================
    // Duplex / tumble: SpliX request.cpp (mode selection) + qpdl.cpp (bytes)
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
            "with duplex off, tumble is ignored"
        );
        // The ML-2160 family declares `*QPDL ManualDuplex: "On"`, so the result
        // must always be Manual*; automatic LongEdge/ShortEdge is wrong here.
        assert_eq!(mode(true, false), SplDuplex::ManualLongEdge);
        assert_eq!(mode(true, true), SplDuplex::ManualShortEdge);
    }

    /// On one-sided jobs the tumble byte must be 0 and the duplex byte 1 on
    /// every page. (SpliX: Simplex -> duplex = 1, tumble = 0.)
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
                "page {}: the Simplex duplex byte must be 1",
                i + 1
            );
            assert_eq!(
                p.header[0xC],
                0,
                "page {}: the Simplex tumble byte must be 0",
                i + 1
            );
        }
    }

    /// In manual duplex, tumble is the parity of the PAGE NUMBER, and the
    /// counter starts at 1: 1 on odd pages, 0 on even ones. The old code wrote
    /// an unconditional 0 here.
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
            "tumble = pageNr % 2 (pageNr is 1-based)"
        );
        for p in &pages {
            assert_eq!(
                p.header[0xB], 0,
                "in manual duplex the duplex byte must be 0"
            );
        }
    }

    /// Manual duplex must say `DUPLEX=MANUAL` in the PJL, not `DUPLEX=ON`
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
            "manual duplex must not announce ON: {}",
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

    /// The `Tumble` field of the CUPS raster header must be read at byte 368,
    /// immediately before cupsWidth. The field used to be named `turn_off`, and
    /// because nothing ever used it the misnaming went unnoticed.
    #[test]
    fn test_tumble_is_parsed_from_offset_368() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.tumble = true;
        let stream = spec.build();
        let header = PageHeader::parse(&stream[4..4 + 1796], CupsRasterVersion::V3Be).unwrap();
        assert!(header.tumble, "the Tumble field at byte 368 was not read");
        assert!(header.duplex);
    }

    // ======================================================================
    // Paper size mapping
    // ======================================================================

    /// Every paper size the PPD offers must map to the right QPDL paper code.
    /// Any divergence between the PPD and the table refuses a legitimate job;
    /// this test keeps the two lists in step.
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
                other => panic!("a paper size in the PPD that the table lacks: {}", other),
            }
        };

        let papers = ppd_paper_dimensions();
        assert_eq!(
            papers.len(),
            11,
            "the expected number of paper sizes was not read from the PPD"
        );
        for (name, w, h) in papers {
            assert_eq!(
                SplPaperSize::from_dimensions_pt_exact(w, h),
                Some(expected(&name)),
                "the PPD says '{}' = {}x{} pt, but the exact mapping gives another code",
                name,
                w,
                h
            );
        }
    }

    /// Folio is 210x330 mm (595x935 pt). The 8.5x13 inch size, 612x936 pt, is
    /// FanFoldGermanLegal in Adobe's naming and is not Folio; `cupstestppd`
    /// warned about the PPD for exactly this reason.
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
        // 8.5x13 inch must map neither to Folio nor silently to A4.
        assert_eq!(SplPaperSize::from_dimensions_pt_exact(612, 936), None);
    }

    // ======================================================================
    // Paper source (PPD *InputSlot -> CUPS MediaPosition -> QPDL byte 0x9)
    // ======================================================================

    /// Every paper source the PPD offers must map to the right QPDL source
    /// code.
    ///
    /// The PPD numbers its `*InputSlot` options with the QPDL codes directly
    /// (`<</MediaPosition 1>>` = Auto). If that link stays a comment, adding a
    /// value with no QPDL code to the PPD (Auto used to be 0) makes the option
    /// fall back to Auto in silence.
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
                other => panic!("a paper source in the PPD that the table lacks: {}", other),
            }
        };

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*InputSlot ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("malformed *InputSlot line");
            let name = name.split('/').next().unwrap().trim();
            // "<</MediaPosition 2>>setpagedevice" -> 2
            let pos: u32 = code
                .split("MediaPosition")
                .nth(1)
                .expect("no MediaPosition")
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .expect("MediaPosition is not a number");

            assert_eq!(
                SplPaperSource::from_media_position(pos),
                Some(expected(name)),
                "the PPD says '{}' = MediaPosition {}, but the filter maps another code",
                name,
                pos
            );
            checked += 1;
        }
        assert_eq!(
            checked, 2,
            "the expected number of paper sources was not read from the PPD"
        );
    }

    /// End to end: choosing "Manual Feeder" must reach byte 0x9 of the QPDL
    /// page header. Before the fix that byte was an unconditional 1 (Auto).
    #[test]
    fn test_input_slot_reaches_qpdl_page_header() {
        for (media_position, expected) in [(1u32, 1u8), (2, 2), (0, 1)] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_position = media_position;
            let pages = parse_spl(&run_filter(spec.build()));
            assert_eq!(
                pages[0].header[0x9], expected,
                "MediaPosition {} must give QPDL source code {}",
                media_position, expected
            );
        }
    }

    /// An unrecognised `MediaPosition` must not turn silently into the wrong
    /// code; it falls back to Auto.
    #[test]
    fn test_unknown_media_position_falls_back_to_auto() {
        use spl::SplPaperSource;
        assert_eq!(SplPaperSource::from_media_position(6), None);
        assert_eq!(SplPaperSource::from_media_position(u32::MAX), None);

        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.media_position = 6;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(
            pages[0].header[0x9], 1,
            "an unknown source must fall back to Auto"
        );
    }

    // ======================================================================
    // Paper type (PPD *MediaType -> CUPS MediaType -> @PJL SET PAPERTYPE)
    // ======================================================================

    /// EVERY media-type keyword the PPD offers must be recognised by the
    /// filter too. Otherwise the user's choice falls back to `OFF` in silence
    /// and an envelope or label is printed with plain-paper fuser settings.
    #[test]
    fn test_every_ppd_media_type_is_accepted_by_the_filter() {
        let ppd = ppd_text();

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*MediaType ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("malformed *MediaType line");
            let name = name.split('/').next().unwrap().trim();

            assert_eq!(
                spl::pjl_paper_type(name),
                Some(name),
                "the PPD offers '{}' but it is not in the filter's PJL vocabulary",
                name
            );
            // The choice must reach the raster header: the PostScript code has
            // to set the MediaType string to the keyword ITSELF, or the filter
            // sees a different value.
            assert!(
                code.contains(&format!("MediaType({})", name)),
                "the PPD choice '{}' does not reach the raster header under the same \
                 keyword: {}",
                name,
                line
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            spl::PJL_PAPER_TYPES.len(),
            "the PPD does not offer every type in the printer's PJL vocabulary"
        );
    }

    /// End to end: the selected media type must reach the PJL header. Before
    /// the fix this always said `PAPERTYPE=OFF`.
    #[test]
    fn test_media_type_reaches_pjl_papertype() {
        for media_type in ["ENV", "LABEL", "THICK", "OFF"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains(&format!("@PJL SET PAPERTYPE={}\n", media_type)),
                "{} did not reach the PJL: {}",
                media_type,
                pjl
            );
        }
    }

    /// An unrecognised or empty `MediaType` must fall back to the safe default
    /// and must never be written raw into the PJL line (the line is unquoted: a
    /// space or a CR/LF would corrupt the command).
    #[test]
    fn test_unknown_media_type_falls_back_to_papertype_off() {
        for media_type in ["", "Envelope", "Plain", "EVIL VALUE"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains("@PJL SET PAPERTYPE=OFF\n"),
                "{:?} did not fall back to OFF: {}",
                media_type,
                pjl
            );
            assert!(
                !pjl.contains("PAPERTYPE=EVIL"),
                "an untrusted value leaked into the PJL line: {}",
                pjl
            );
        }
    }

    /// The PPD's defaults must agree with the filter's: if the PPD says
    /// `*DefaultMediaType: OFF` / `*DefaultInputSlot: Auto`, a job that selects
    /// nothing has to produce the same result.
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
