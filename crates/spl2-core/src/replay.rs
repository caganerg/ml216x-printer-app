// SPDX-License-Identifier: GPL-2.0-only

//! Classic CUPS Raster in, SPL2/QPDL out — the 1.x filter's page loop.
//!
//! This is the loop `src/main.rs` used to hold, moved here unchanged so that
//! the golden corpus does not depend on the 1.x binary surviving. Decision Q-5
//! keeps that binary in the tree until gate P11 passes; the 32 goldens are the
//! byte-for-byte evidence the migration is judged by, and evidence that dies
//! with the thing it was measuring is no evidence at all. With the loop here,
//! `crates/spl2-core/tests/golden.rs` replays the corpus through the engine
//! directly and P11 becomes a deletion of front-end code — argv, stdin, stderr
//! — rather than a deletion of the harness.
//!
//! It sits behind `golden-replay` with the classic raster parser it reads
//! (decision Q-6), so the shipping printer application compiles none of it.
//!
//! Nothing here writes to a stream: diagnostics go to the [`Log`] the caller
//! supplies, with the same text and the same levels the filter printed, so the
//! prefixes CUPS routes on are unchanged.

use std::io::{self, Read, Write};

use crate::engine::PageSetup;
use crate::geometry::{
    duplex_mode, pjl_paper_type_for, quote_untrusted, validate_page_geometry, JobBudget,
    PageGeometry,
};
use crate::log::{Level, Log};
use crate::media;
use crate::qpdl::{self as spl, JobConfig, SplPaperSource, SplStreamWriter};
use crate::raster::{CupsRasterReader, PageHeader};

/// The two argv fields the loop reads.
///
/// The filter's own `CupsFilterArgs` carries six, four of which are read by
/// nothing (see its comment on why `num_copies` and `options` are deliberately
/// ignored). Only the two that reach the PJL job header follow the loop here.
#[derive(Debug, Default, Clone)]
pub struct JobIdentity {
    /// `argv[3]`, the job title. `"CUPS Document"` when absent.
    pub title: Option<String>,
    /// `argv[2]`, the submitting user. `"guest"` when absent.
    pub user: Option<String>,
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

/// Kept under its own name so the filter's tests and the golden harness keep
/// naming the check they exercise.
pub fn validate_page_header(header: &PageHeader) -> io::Result<()> {
    validate_page_geometry(&geometry_of(header))
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
pub fn process<W: Write>(
    identity: &JobIdentity,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
    log: &dyn Log,
) -> io::Result<()> {
    process_with_margin(
        identity,
        reader,
        writer,
        service_date,
        media::HARD_MARGIN_PT,
        log,
    )
}

// The production path always supplies the driver constant. The explicit input
// also preserves synthetic geometry cases in the golden harness.
pub fn process_with_margin<W: Write>(
    identity: &JobIdentity,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
    margin_pt: f64,
    log: &dyn Log,
) -> io::Result<()> {
    if !margin_pt.is_finite() || margin_pt <= 0.0 || margin_pt > 36.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid driver hard margin",
        ));
    }
    // 1. CUPS Raster header/magic check (RaSt, RaS2, RaS3, etc.)
    let mut raster_reader = CupsRasterReader::new(reader)?;

    log.log(
        Level::Info,
        &format!(
            "valid CUPS Raster stream detected (version: {:?}, endian: {})",
            raster_reader.version(),
            if raster_reader.version().is_big_endian() {
                "Big Endian"
            } else {
                "Little Endian"
            }
        ),
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
        Some(h) => pjl_paper_type_for(&h.media_type, log),
        None => spl::PJL_PAPERTYPE_DEFAULT,
    };

    // 2. Samsung ML-2160 series PJL header (@PJL ENTER LANGUAGE = QPDL)
    let job_config = JobConfig {
        job_name: identity
            .title
            .clone()
            .unwrap_or_else(|| "CUPS Document".to_string()),
        user_name: identity.user.clone().unwrap_or_else(|| "guest".to_string()),
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
        let copies = crate::geometry::sanitize_copies(header.num_copies);

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
        log.log(Level::Page, &format!("{} {}", page_number, copies));
        log.log(Level::Info, &format!("starting page {}...", page_number));

        print_header_info(page_number, &header, log);

        // `cupsCompression` in CUPS Raster is not STREAM compression but a
        // driver-specific "device compression" hint (stream compression is
        // determined by the sync word; see raster.rs is_compressed). SpliX does
        // not use this field, and this filter always does band compression with
        // Algo 0x11; so the field is deliberately ignored. If it is non-zero we
        // report it once for diagnostics, because there may be a mismatch
        // between the PPD and this filter's assumptions.
        if header.compression != 0 {
            log.log(
                Level::Warning,
                &format!(
                    "cupsCompression={} ignored; band compression is always Algo 0x11 RLE.",
                    header.compression
                ),
            );
        }

        // Geometry, placement and the 17-byte page header are now in
        // `spl2-core`: the same computation runs on the PAPPL path too, so the
        // two front ends cannot diverge.
        let setup = PageSetup::new(&geometry, margin_pt, page_number, log)?;

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

        log.log(Level::Info, &format!("page {} complete.\n", page_number));

        next_header = raster_reader.next_page_header()?;
    }

    if page_number == 0 {
        log.log(Level::Warning, "no pages found in the CUPS Raster stream.");
    } else {
        log.log(
            Level::Info,
            &format!(
                "{} pages successfully converted to SPL/QPDL format.",
                page_number
            ),
        );
    }

    // Job end (the PJL UEL). Called even if no page was found: because
    // `begin_job` has already put the printer into QPDL, the stream must end
    // with a closing UEL in any case. `end_job` flushes internally.
    spl_writer.end_job()?;
    Ok(())
}

/// Formats the metadata from the CUPS Raster page header and sends it to the front end's log.
fn print_header_info(page_num: u32, header: &PageHeader, log: &dyn Log) {
    // Every line starts with `DEBUG: `. CUPS routes the prefixes it recognises
    // in a filter's stderr (DEBUG/INFO/WARNING/ERROR/PAGE/...) to that level;
    // it also treats UNPREFIXED lines as DEBUG, so under the default
    // `LogLevel warn` the behaviour is the same. The difference showed up at
    // `LogLevel debug`: this block produces ~15 lines per page and the
    // unprefixed lines left the intent unclear. The prefix tells both CUPS and
    // the log reader plainly that the lines are diagnostic.
    log.log(
        Level::Debug,
        "--------------------------------------------------",
    );
    log.log(
        Level::Debug,
        &format!(" [CUPS RASTER PAGE {} METADATA]", page_num),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Resolution (DPI): {} x {}",
            header.hw_resolution[0], header.hw_resolution[1]
        ),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Dimensions (px) : {} x {} (width x height)",
            header.width, header.height
        ),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Page size (pt)  : {} x {} pt",
            header.page_size_points[0], header.page_size_points[1]
        ),
    );
    if let Some(name) = &header.page_size_name {
        // `cupsPageSizeName` is a 64-byte C string in the raster header coming
        // from the submitting client — as untrusted as `title`/`user` in argv,
        // so it is printed escaped rather than raw.
        log.log(
            Level::Debug,
            &format!("  Media name      : {}", quote_untrusted(name)),
        );
    }
    log.log(
        Level::Debug,
        &format!("  Colour space    : {}", header.color_space),
    );
    log.log(
        Level::Debug,
        &format!("  Colour order    : {:?}", header.color_order),
    );
    log.log(
        Level::Debug,
        &format!("  Bits per colour : {}", header.bits_per_color),
    );
    log.log(
        Level::Debug,
        &format!("  Bits per pixel  : {}", header.bits_per_pixel),
    );
    log.log(
        Level::Debug,
        &format!("  Bytes per line  : {} bytes", header.bytes_per_line),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Raw raster size : {} bytes ({:.2} MB)",
            header.total_raster_bytes(),
            header.total_raster_bytes() as f64 / (1024.0 * 1024.0)
        ),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Duplex          : {}",
            if header.duplex { "on" } else { "off" }
        ),
    );
    log.log(
        Level::Debug,
        &format!("  Copies          : {}", header.num_copies),
    );
    log.log(
        Level::Debug,
        &format!(
            "  Paper source    : MediaPosition={} -> {:?}",
            header.media_position,
            SplPaperSource::from_media_position(header.media_position)
        ),
    );
    // `pjl_paper_type_for` prints the warning once while the job header is set
    // up; here only the result of the mapping is shown (to avoid a warning
    // repeated per page).
    log.log(
        Level::Debug,
        &format!(
            "  Paper type      : MediaType={} -> PAPERTYPE={}",
            quote_untrusted(&header.media_type),
            spl::pjl_paper_type(&header.media_type).unwrap_or(spl::PJL_PAPERTYPE_DEFAULT)
        ),
    );
    log.log(
        Level::Debug,
        "--------------------------------------------------",
    );
}
