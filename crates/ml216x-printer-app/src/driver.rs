// SPDX-License-Identifier: GPL-2.0-only

//! The SPL2 raster driver: PAPPL's callbacks in, QPDL on the device out.
//!
//! The protocol itself lives in `spl2-core`, shared byte for byte with the
//! frozen 1.x filter. What is specific to this path is the geometry
//! adaptation, because the two front ends are fed differently:
//!
//! * cups-filters hands the classic filter the **printable area**, already
//!   centred on the sheet, so horizontal centring and the printer's hard
//!   margin cancel and no vertical correction is needed at all
//!   (`docs/MARGINS.md`).
//! * PAPPL hands this driver the **full media** with zero header margins, as
//!   17 measured cases show (`docs/P5-MEASUREMENTS.json`).
//!
//! So this module subtracts the hard margin on both axes and hands `spl2-core`
//! a page that means the same thing the filter's pages mean:
//!
//! * **Horizontally** nothing new is needed. `band_placement` centres the line
//!   in the sheet-wide band and subtracts the hard margin; with a full-media
//!   line the centring term is zero and the subtraction alone survives, which
//!   is exactly the sheet-to-engine mapping the classic path performs. This is
//!   the "do not subtract twice" trap `docs/MARGINS.md` warns about: the
//!   correct guard is to feed the real line width, not to special-case it.
//! * **Vertically** the cancellation the classic path relied on is gone, so
//!   the first `hard_margin_lines` scanlines are dropped and the page is cut
//!   to the printable height. See open question Q-13 in `docs/DECISIONS.md`:
//!   this is the axis with no direct precedent in the tree, and release gate
//!   G-1 has to measure it.

use std::collections::HashMap;
use std::sync::Mutex;

use pappl::application::{RasterDriver, RasterOptions};
use pappl::{Device, Error, Job, LogLevel, Result};
use spl2_core::engine::{BandEncoder, PageSetup};
use spl2_core::geometry::{
    hard_margin_lines, CupsColorOrder, CupsColorSpace, JobBudget, PageGeometry,
};
use spl2_core::log::{Level, Log};
use spl2_core::media::HARD_MARGIN_PT;
use spl2_core::qpdl::{
    current_service_date, pjl_paper_type, JobConfig, SplDuplex, SplStreamWriter,
};

use crate::media_table;

fn fail(message: impl Into<String>) -> Error {
    Error::Driver(message.into())
}

/// Routes `spl2-core` diagnostics to the job's own log.
struct JobLog<'a>(&'a Job<'a>);

impl Log for JobLog<'_> {
    fn log(&self, level: Level, message: &str) {
        let level = match level {
            Level::Debug => LogLevel::Debug,
            Level::Info | Level::Page => LogLevel::Info,
            Level::Warning => LogLevel::Warn,
            Level::Error => LogLevel::Error,
        };
        self.0.log(level, message);
    }
}

/// One page in flight.
struct PageState {
    setup: PageSetup,
    encoder: BandEncoder,
    /// Scanlines of the incoming full-media page that fall in the top hard
    /// margin and are not sent.
    skip_lines: u32,
    /// Scanlines handed to the encoder so far.
    accepted: u32,
}

/// One job in flight.
struct JobState {
    /// The QPDL stream is built into memory and drained to the device after
    /// every callback: PAPPL hands out a fresh device handle per call, so the
    /// writer cannot own one.
    writer: SplStreamWriter<Vec<u8>>,
    budget: JobBudget,
    page: Option<PageState>,
    /// The number PAPPL gave this job's **first** page, which is where its own
    /// page sequence starts. It is 1 under PAPPL 1.3 and 0 since 1.4, so it is
    /// learned from the job rather than assumed; see `check_pappl_page`.
    pappl_page_base: Option<u32>,
}

/// Cross-checks PAPPL's page number against the driver's own page counter.
///
/// The QPDL page number is the driver's, counted by `JobBudget::account_page`
/// and 1-based because the protocol is. PAPPL counts the same pages
/// independently, and this is the check that the two never drift apart — a
/// silent disagreement would put the wrong page number, and with it the wrong
/// tumble byte, on the wire.
///
/// **The two supported PAPPL releases start their sequence in different
/// places**, which is the one behavioural difference the 1.4 move brought:
///
/// * 1.3 increments its counter at the top of the page loop and so passes `1`
///   for the first page (`pappl/job-process.c:634` then `:664` in 1.3.1);
/// * 1.4 increments it after `rendpage` instead and so passes `0`
///   (`pappl/job-process.c:674` then `:823` in 1.4.12), part of upstream's
///   1.4.9 fix to the number `rendpage` is given.
///
/// Both advance by exactly one per page and never reset, including when the
/// client asked for a page range, so what can be checked without knowing the
/// release is the *step*: PAPPL's number must stay `base + pages - 1` for the
/// base its own first page established. A base other than 0 or 1 is not a
/// numbering either release uses and fails the job rather than being adopted.
fn check_pappl_page(base: &mut Option<u32>, page: u32, page_number: u32) -> Result<()> {
    let base = *base.get_or_insert(page);
    if base > 1 {
        return Err(fail(format!(
            "PAPPL numbered the first page of this job {base}; \
             1.3 numbers from 1 and 1.4 from 0, so this is a numbering \
             this driver has not been checked against"
        )));
    }
    let expected = base + page_number - 1;
    if page != expected {
        return Err(fail(format!(
            "PAPPL page {page} does not match job page {page_number} \
             (expected {expected} from a sequence based at {base})"
        )));
    }
    Ok(())
}

/// Turns PAPPL raster jobs into SPL2/QPDL.
///
/// The system runs with `PAPPL_SOPTIONS_MULTI_QUEUE`, so two printers can be
/// printing at once through this one driver; state is therefore keyed by job
/// id rather than assumed unique. Entries are removed by `end_job`, and by
/// `abandon_job` when a job dies without one — otherwise a failed job would
/// leave a half-written QPDL stream for the next job to continue.
#[derive(Default)]
pub struct Spl2Driver {
    jobs: Mutex<HashMap<i32, JobState>>,
}

impl Spl2Driver {
    pub fn new() -> Self {
        Self::default()
    }

    fn jobs(&self) -> Result<std::sync::MutexGuard<'_, HashMap<i32, JobState>>> {
        self.jobs
            .lock()
            .map_err(|_| fail("job state lock poisoned"))
    }
}

/// Moves everything the engine has produced so far onto the device.
fn drain(writer: &mut SplStreamWriter<Vec<u8>>, device: &mut Device<'_>) -> Result<()> {
    let buffer = writer.writer_mut();
    if !buffer.is_empty() {
        device.write_all(buffer)?;
        buffer.clear();
    }
    Ok(())
}

/// The page `spl2-core` should encode, derived from what PAPPL delivered.
///
/// Returns the geometry and how many leading scanlines to drop.
fn page_geometry(o: &RasterOptions) -> Result<(PageGeometry, u32)> {
    let points = media_table::legacy_points(&o.media_name)
        .ok_or_else(|| fail(format!("no QPDL paper size for media {:?}", o.media_name)))?;

    let y_dpi = u32::try_from(o.resolution[1]).map_err(|_| fail("negative resolution"))?;
    let x_dpi = u32::try_from(o.resolution[0]).map_err(|_| fail("negative resolution"))?;

    // Both hard margins come off the incoming full-media page. The horizontal
    // one is applied by `band_placement`; only the vertical one is applied here.
    let skip = hard_margin_lines(HARD_MARGIN_PT, y_dpi);
    let printable = o
        .height
        .checked_sub(skip.checked_mul(2).ok_or_else(|| fail("margin overflow"))?)
        .filter(|lines| *lines > 0)
        .ok_or_else(|| {
            fail(format!(
                "page of {} lines at {} dpi is shorter than its {} pt margins",
                o.height, y_dpi, HARD_MARGIN_PT
            ))
        })?;

    let media_position = media_table::media_position(&o.media_source)
        .ok_or_else(|| fail(format!("no QPDL tray for source {:?}", o.media_source)))?;
    let media_type = media_table::pjl_media_type(&o.media_type).ok_or_else(|| {
        fail(format!(
            "no PJL paper type for media-type {:?}",
            o.media_type
        ))
    })?;
    // The engine looks the name up again; refuse now rather than let it fall
    // back to the default halfway through a job.
    if pjl_paper_type(media_type).is_none() {
        return Err(fail(format!(
            "PJL paper type {media_type:?} is not in the printer's vocabulary"
        )));
    }

    Ok((
        PageGeometry {
            width: o.width,
            height: printable,
            bytes_per_line: o.bytes_per_line,
            hw_resolution: [x_dpi, y_dpi],
            page_size_points: points,
            // PAPPL's BLACK_1 path reports no header margins, and since
            // 2026-09-06 the driver constant is the only margin that counts.
            margins: [0, 0],
            bits_per_color: 1,
            bits_per_pixel: 1,
            color_space: CupsColorSpace::K,
            color_order: CupsColorOrder::Chunked,
            num_copies: u32::try_from(o.copies).map_err(|_| fail("negative copies"))?,
            media_position,
            duplex: false,
            tumble: false,
            media_type: media_type.to_string(),
        },
        skip,
    ))
}

impl RasterDriver for Spl2Driver {
    fn start_job(&self, job: &Job<'_>, o: &RasterOptions, device: &mut Device<'_>) -> Result<()> {
        let mut jobs = self.jobs()?;
        if jobs.contains_key(&job.id()) {
            return Err(fail("this job is already in progress"));
        }
        let paper_type = media_table::pjl_media_type(&o.media_type).ok_or_else(|| {
            fail(format!(
                "no PJL paper type for media-type {:?}",
                o.media_type
            ))
        })?;
        let mut writer = SplStreamWriter::new(Vec::new());
        writer.begin_job(&JobConfig {
            job_name: job.name()?.to_string(),
            user_name: job.username()?.to_string(),
            service_date: current_service_date(),
            // The capability table publishes one-sided only, and `validate`
            // rejects anything else before this runs.
            duplex: SplDuplex::Simplex,
            paper_type,
        })?;
        drain(&mut writer, device)?;
        jobs.insert(
            job.id(),
            JobState {
                writer,
                budget: JobBudget::default(),
                page: None,
                pappl_page_base: None,
            },
        );
        Ok(())
    }

    fn start_page(
        &self,
        job: &Job<'_>,
        o: &RasterOptions,
        device: &mut Device<'_>,
        page: u32,
    ) -> Result<()> {
        let mut jobs = self.jobs()?;
        let state = jobs
            .get_mut(&job.id())
            .ok_or_else(|| fail("no job in progress"))?;
        if state.page.is_some() {
            return Err(fail("a page is already in progress"));
        }

        let (geometry, skip_lines) = page_geometry(o)?;
        let copies = spl2_core::geometry::sanitize_copies(geometry.num_copies);
        let page_number = state
            .budget
            .account_page(geometry.total_raster_bytes(), copies)?;
        check_pappl_page(&mut state.pappl_page_base, page, page_number)?;

        let setup = PageSetup::new(&geometry, HARD_MARGIN_PT, page_number, &JobLog(job))?;
        state.writer.begin_page(&setup.config)?;
        drain(&mut state.writer, device)?;
        let encoder = setup.encoder();
        state.page = Some(PageState {
            setup,
            encoder,
            skip_lines,
            accepted: 0,
        });
        Ok(())
    }

    fn write_line(
        &self,
        job: &Job<'_>,
        _o: &RasterOptions,
        device: &mut Device<'_>,
        y: u32,
        line: &[u8],
    ) -> Result<()> {
        let mut jobs = self.jobs()?;
        let state = jobs
            .get_mut(&job.id())
            .ok_or_else(|| fail("no job in progress"))?;
        let writer = &mut state.writer;
        let page = state
            .page
            .as_mut()
            .ok_or_else(|| fail("no page in progress"))?;

        // Top hard margin: dropped, not encoded.
        if y < page.skip_lines {
            return Ok(());
        }
        // Bottom hard margin: the page is already as tall as it declared.
        if page.accepted as usize >= page.setup.total_lines {
            return Ok(());
        }
        // PAPPL delivers scanlines in order. A gap would silently shift the
        // rest of the page up the sheet, so it fails the job instead.
        if y != page.skip_lines + page.accepted {
            return Err(fail(format!(
                "scanline {y} arrived out of order; expected {}",
                page.skip_lines + page.accepted
            )));
        }

        page.encoder.write_line(writer, line)?;
        page.accepted += 1;
        drain(writer, device)
    }

    /// The page number is deliberately **not** cross-checked here, unlike in
    /// `start_page`. It is the number upstream found wrong and changed — the
    /// 1.4.9 release note is "fixed page number that is passed to the raster
    /// endpage function" — so releases between 1.4.0 and 1.4.8, which this
    /// tree accepts but does not test, may well pass something else. The page
    /// it closes is the one `start_page` opened in this job's state, and that
    /// is not in doubt: `state.page` is `Some` for exactly one page at a time.
    fn end_page(
        &self,
        job: &Job<'_>,
        _o: &RasterOptions,
        device: &mut Device<'_>,
        _page: u32,
    ) -> Result<()> {
        let mut jobs = self.jobs()?;
        let state = jobs
            .get_mut(&job.id())
            .ok_or_else(|| fail("no job in progress"))?;
        let page = state
            .page
            .take()
            .ok_or_else(|| fail("no page in progress"))?;
        let copies = page.setup.copies;
        // `finish` refuses a page that received fewer scanlines than it
        // declared: a short page desynchronises the printer's RLE decoder.
        page.encoder.finish(&mut state.writer)?;
        state.writer.end_page(copies)?;
        drain(&mut state.writer, device)
    }

    fn end_job(&self, job: &Job<'_>, _o: &RasterOptions, device: &mut Device<'_>) -> Result<()> {
        let mut jobs = self.jobs()?;
        let mut state = jobs
            .remove(&job.id())
            .ok_or_else(|| fail("no job in progress"))?;
        if state.page.is_some() {
            return Err(fail("the job ended with a page still open"));
        }
        state.writer.end_job()?;
        drain(&mut state.writer, device)?;
        device.flush();
        Ok(())
    }

    fn abandon_job(&self, job: &Job<'_>) {
        // The stream is already broken and the device is gone, so the buffered
        // tail is dropped with it. `SplStreamWriter::drop` writes its closing
        // UEL into that buffer, which is exactly where it should stop.
        if let Ok(mut jobs) = self.jobs() {
            jobs.remove(&job.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_table::MEDIA_TABLE;
    use spl2_core::engine::PageSetup;
    use spl2_core::geometry::hard_margin_bytes;
    use spl2_core::log::NoLog;

    const RESOLUTIONS: [[i32; 2]; 4] = [[300, 300], [600, 600], [1200, 600], [1200, 1200]];

    /// PWG's own pixel count for a sheet, which truncates rather than rounds.
    /// Confirmed against all 17 measured cases in `docs/P5-MEASUREMENTS.json`
    /// (A4 at 600 dpi is 4960x7015, not 4961x7016).
    fn pwg_pixels(hundredths_mm: i32, dpi: i32) -> u32 {
        (i64::from(hundredths_mm) * i64::from(dpi) / 2540) as u32
    }

    /// What cups-filters hands the classic filter: the printable area, at the
    /// rounding rule `src/golden.rs` verified against real runs.
    fn classic_pixels(points: u32, dpi: i32) -> u32 {
        ((f64::from(points) - 2.0 * HARD_MARGIN_PT) * f64::from(dpi) / 72.0).round() as u32
    }

    fn full_media_options(medium: &crate::media_table::Medium, dpi: [i32; 2]) -> RasterOptions {
        let width = pwg_pixels(medium.pwg.width, dpi[0]);
        RasterOptions {
            copies: 1,
            resolution: dpi,
            media_name: medium.pwg.name.to_string_lossy().into_owned(),
            media_size: [medium.pwg.width, medium.pwg.length],
            media_margins: [441; 4],
            media_source: "auto".to_string(),
            media_type: "stationery".to_string(),
            width,
            height: pwg_pixels(medium.pwg.length, dpi[1]),
            bytes_per_line: width.div_ceil(8),
            header_margins: [0, 0],
        }
    }

    /// The geometry the classic filter would be handed for the same sheet.
    fn classic_geometry(medium: &crate::media_table::Medium, dpi: [i32; 2]) -> PageGeometry {
        let width = classic_pixels(medium.points[0], dpi[0]);
        PageGeometry {
            width,
            height: classic_pixels(medium.points[1], dpi[1]),
            bytes_per_line: width.div_ceil(8),
            hw_resolution: [dpi[0] as u32, dpi[1] as u32],
            page_size_points: medium.points,
            margins: [12, 12],
            bits_per_color: 1,
            bits_per_pixel: 1,
            color_space: CupsColorSpace::K,
            color_order: CupsColorOrder::Chunked,
            num_copies: 1,
            media_position: 1,
            duplex: false,
            tumble: false,
            media_type: "NORMAL".to_string(),
        }
    }

    /// The whole point of the adaptation: a mark at a given place on the sheet
    /// must land in the same band column whether it arrived as printable-area
    /// raster from cups-filters or as full-media raster from PAPPL.
    ///
    /// The band column of sheet byte column `X` is `dst_offset + X - origin -
    /// src_skip`, where `origin` is where the incoming raster starts on the
    /// sheet: the centring term for the classic path, and zero for full media.
    /// If the printer application subtracted the hard margin a second time —
    /// the trap `docs/MARGINS.md` names — these two would differ by exactly
    /// `hard_margin_bytes`.
    #[test]
    fn full_media_and_printable_area_place_the_sheet_identically() {
        for medium in MEDIA_TABLE {
            for dpi in RESOLUTIONS {
                let classic = classic_geometry(medium, dpi);
                let classic_setup = PageSetup::new(&classic, HARD_MARGIN_PT, 1, &NoLog)
                    .unwrap_or_else(|e| panic!("{} classic {dpi:?}: {e}", medium.ppd_key));
                let centred = classic_setup
                    .band_width_bytes
                    .saturating_sub(classic.bytes_per_line as usize)
                    / 2;
                let classic_origin = classic_setup.placement.dst_offset as isize
                    - centred as isize
                    - classic_setup.placement.src_skip as isize;

                let options = full_media_options(medium, dpi);
                let (full, _skip) = page_geometry(&options)
                    .unwrap_or_else(|e| panic!("{} full {dpi:?}: {e}", medium.ppd_key));
                let full_setup = PageSetup::new(&full, HARD_MARGIN_PT, 1, &NoLog)
                    .unwrap_or_else(|e| panic!("{} full {dpi:?}: {e}", medium.ppd_key));
                let full_origin = full_setup.placement.dst_offset as isize
                    - full_setup.placement.src_skip as isize;

                assert_eq!(
                    classic_origin, full_origin,
                    "{} at {dpi:?}: the two paths disagree about where the sheet starts",
                    medium.ppd_key
                );
                assert_eq!(
                    full_origin,
                    -(hard_margin_bytes(HARD_MARGIN_PT, dpi[0] as u32) as isize),
                    "{} at {dpi:?}: full-media placement must be exactly the hard margin",
                    medium.ppd_key
                );
            }
        }
    }

    /// The vertical crop must land within a fraction of a point of what the
    /// classic path receives. A double subtraction, or none at all, would be
    /// out by the whole margin — 12.5 pt, 209 lines for A4 at 600 dpi.
    ///
    /// The tolerance is in points rather than scanlines because the residual
    /// difference is not a rounding artefact of this code: the PPD states
    /// sheets in whole points while PWG states them in hundredths of a
    /// millimetre, so A5 is 595 pt to the PPD and 595.28 pt to PWG. That 0.28
    /// pt shows up as five scanlines at 1200 dpi and as none at 300.
    #[test]
    fn cropped_height_matches_the_printable_area() {
        for medium in MEDIA_TABLE {
            for dpi in RESOLUTIONS {
                let options = full_media_options(medium, dpi);
                let (full, skip) = page_geometry(&options).unwrap();
                assert_eq!(
                    skip,
                    (HARD_MARGIN_PT * f64::from(dpi[1]) / 72.0).round() as u32,
                    "{} at {dpi:?}",
                    medium.ppd_key
                );
                let classic = classic_pixels(medium.points[1], dpi[1]);
                let difference_pt =
                    (f64::from(full.height) - f64::from(classic)) * 72.0 / f64::from(dpi[1]);
                assert!(
                    difference_pt.abs() <= 0.6,
                    "{} at {dpi:?}: cropped to {} lines, the classic path gets {} \
                     ({difference_pt:.2} pt apart)",
                    medium.ppd_key,
                    full.height,
                    classic
                );
            }
        }
    }

    /// Every medium and resolution the capability table publishes must survive
    /// validation; a combination that cannot be encoded must not be offered.
    #[test]
    fn every_published_combination_produces_a_page() {
        for medium in MEDIA_TABLE {
            for dpi in RESOLUTIONS {
                let options = full_media_options(medium, dpi);
                let (geometry, _) = page_geometry(&options)
                    .unwrap_or_else(|e| panic!("{} {dpi:?}: {e}", medium.ppd_key));
                PageSetup::new(&geometry, HARD_MARGIN_PT, 1, &NoLog)
                    .unwrap_or_else(|e| panic!("{} {dpi:?}: {e}", medium.ppd_key));
            }
        }
    }

    /// Unknown vocabulary fails the job; it is never silently defaulted.
    #[test]
    fn unknown_option_values_are_refused() {
        let medium = &MEDIA_TABLE[0];
        for mutate in [
            (|o: &mut RasterOptions| o.media_name = "iso_a3_297x420mm".into())
                as fn(&mut RasterOptions),
            |o: &mut RasterOptions| o.media_source = "tray-9".into(),
            |o: &mut RasterOptions| o.media_type = "screen".into(),
        ] {
            let mut options = full_media_options(medium, [600, 600]);
            mutate(&mut options);
            assert!(page_geometry(&options).is_err());
        }
    }

    /// A page shorter than its own margins is refused rather than wrapped.
    #[test]
    fn a_page_shorter_than_its_margins_is_refused() {
        let mut options = full_media_options(&MEDIA_TABLE[0], [600, 600]);
        options.height = 4;
        assert!(page_geometry(&options).is_err());
    }

    /// Both numberings a supported PAPPL uses are accepted, and the driver's
    /// own page number stays 1-based in either case. 1.3 passes 1 for the
    /// first page, 1.4 passes 0; a four-page job through each is the shape the
    /// check has to hold for.
    #[test]
    fn either_pappl_page_numbering_is_accepted() {
        for base in [0, 1] {
            let mut state = None;
            for page_number in 1..=4 {
                check_pappl_page(&mut state, base + page_number - 1, page_number)
                    .unwrap_or_else(|e| panic!("base {base}, page {page_number}: {e}"));
            }
            assert_eq!(state, Some(base));
        }
    }

    /// A job's base is established by its first page and then enforced: once
    /// PAPPL has started at 0 it may not skip, repeat or restart a page, and
    /// the same holds for a job based at 1. This is the drift the check exists
    /// to catch, and dropping it would put the wrong QPDL page number — and
    /// with it the wrong tumble byte — on the wire.
    #[test]
    fn a_page_out_of_step_with_the_driver_fails_the_job() {
        for base in [0, 1] {
            for wrong in [base, base + 2, base + 7] {
                let mut state = Some(base);
                // The driver has just counted its second page.
                assert!(
                    check_pappl_page(&mut state, wrong, 2).is_err(),
                    "base {base} accepted {wrong} as its second page"
                );
            }
        }
    }

    /// Neither release numbers a first page anything but 0 or 1, so a base
    /// outside that is a numbering nothing here has been checked against and
    /// fails the job instead of being adopted as this job's base.
    #[test]
    fn an_unknown_first_page_number_is_refused() {
        for base in [2, 5, u32::MAX] {
            let mut state = None;
            assert!(
                check_pappl_page(&mut state, base, 1).is_err(),
                "base {base}"
            );
        }
    }
}
