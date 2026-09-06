// SPDX-License-Identifier: GPL-2.0-only

//! Page geometry: the pure half of the 1.x filter, moved here unchanged so
//! both front ends compute identical numbers.
//!
//! Everything in this module was `src/main.rs` before the split, and the
//! diagnostic strings are kept verbatim: the 1.x filter is frozen (decision
//! Q-5), so a reworded message would be a behaviour change. New code added
//! during the split is written in English.
//!
//! The one structural change is the input type. The functions used to take
//! `raster::PageHeader`, which now lives behind the `golden-replay` feature;
//! they take [`PageGeometry`] instead, which both the CUPS filter and the
//! PAPPL raster callbacks can fill in.

use std::fmt;
use std::io;

use crate::qpdl::{self, SplDuplex, SplPaperSize, SplResolution};

/// Diagnostics sink; see [`crate::log`].
use crate::log::{Level, Log};

/// CUPS colour space (`cups_cspace_e`) — the raw numeric code.
///
/// The specification defines more than 40 colour spaces, but this driver works
/// only with `K`: every other value is rejected by `validate_page_header`. So
/// rather than model every space separately, the raw code is stored. Both
/// decision points (the K check and the v2 decoder's blank-colour fill) work
/// with the numeric code anyway; the names appear only in error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CupsColorSpace(pub u32);

impl CupsColorSpace {
    /// Additive black: 0 means no toner (white). The encoder inverts it for Samsung.
    pub const K: CupsColorSpace = CupsColorSpace(3);

    /// The fill used in the `n == 128` record (blank to end of line).
    ///
    /// libcups fills the blank with `0x00` in spaces that ADD toner/ink —
    /// K (3), CMY (4), CMYK (5), White (12), Gold (13), Silver (14) — and with
    /// `0xFF` in the others.
    // Only the CUPS Raster line decoder needs this, and that lives behind
    // `golden-replay`.
    #[cfg_attr(not(feature = "golden-replay"), allow(dead_code))]
    pub(crate) fn blank_fill(self) -> u8 {
        match self.0 {
            3 | 4 | 5 | 12 | 13 | 14 => 0x00,
            _ => 0xFF,
        }
    }
}

impl fmt::Display for CupsColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.0 {
            0 => "W (White=0 Grayscale)",
            1 => "RGB",
            2 => "RGBA",
            3 => "K (Black=0 Grayscale)",
            4 => "CMY",
            5 => "CMYK",
            18 => "sGray (sRGB Grayscale)",
            19 => "sRGB",
            20 => "AdobeRGB",
            other => return write!(f, "Unknown({})", other),
        };
        write!(f, "{}", name)
    }
}

/// CUPS colour order (`cups_order_e`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CupsColorOrder {
    /// Pixel bytes are laid out consecutively (e.g. RGBRGB... or KKKK...)
    Chunked,
    /// Colour planes are separate bands within each line (RR... GG... BB...)
    Banded,
    /// Each colour plane is a separate page across the whole sheet
    Planar,
    Unknown(u32),
}

impl From<u32> for CupsColorOrder {
    fn from(val: u32) -> Self {
        match val {
            0 => CupsColorOrder::Chunked,
            1 => CupsColorOrder::Banded,
            2 => CupsColorOrder::Planar,
            other => CupsColorOrder::Unknown(other),
        }
    }
}

/// Everything the QPDL engine needs to know about one page.
///
/// The 1.x filter passed `raster::PageHeader` around, which tied the protocol
/// engine to the classic CUPS Raster parser. This struct is the subset of that
/// header the engine actually reads, so the PAPPL raster callbacks can fill it
/// in from `pappl_pr_options_t` without a CUPS raster stream existing at all.
/// Field names and units are the CUPS ones, because every rule quoted in this
/// module is written against them.
#[derive(Debug, Clone)]
pub struct PageGeometry {
    /// `cupsWidth`, in pixels.
    pub width: u32,
    /// `cupsHeight`, in scanlines.
    pub height: u32,
    /// `cupsBytesPerLine`.
    pub bytes_per_line: u32,
    /// `HWResolution`, `[x, y]` in dpi.
    pub hw_resolution: [u32; 2],
    /// `PageSize`, `[width, length]` in points. This is the SHEET, not the
    /// printable area, and it selects the QPDL paper code.
    pub page_size_points: [u32; 2],
    /// `Margins`, `[left, bottom]` in points, as the raster stream declares
    /// them. Since 2026-09-06 this is validated but no longer drives
    /// placement; the driver constant does (`crate::media::HARD_MARGIN_PT`).
    pub margins: [u32; 2],
    /// `cupsBitsPerColor`.
    pub bits_per_color: u32,
    /// `cupsBitsPerPixel`.
    pub bits_per_pixel: u32,
    /// `cupsColorSpace`.
    pub color_space: CupsColorSpace,
    /// `cupsColorOrder`.
    pub color_order: CupsColorOrder,
    /// `NumCopies`, before [`sanitize_copies`].
    pub num_copies: u32,
    /// `MediaPosition`, the PPD `*InputSlot` code.
    pub media_position: u32,
    /// `Duplex`.
    pub duplex: bool,
    /// `Tumble` — the binding edge, not the QPDL page-side byte.
    pub tumble: bool,
    /// `MediaType`, mapped to a PJL `PAPERTYPE` at job level.
    pub media_type: String,
}

impl PageGeometry {
    /// Decoded raster volume for this page, in bytes; what [`JobBudget`]
    /// meters. Identical to the former `PageHeader::total_raster_bytes`.
    pub fn total_raster_bytes(&self) -> u64 {
        (self.bytes_per_line as u64) * (self.height as u64)
    }
}

/// Prepares an untrusted string for embedding in a log line.
///
/// The `{:?}` (Debug) format wraps the string in quotes and escapes control
/// characters (like `\n`, `\u{1b}`). This closes two attacks at once:
/// injecting a fake log line into `/var/log/cups/error_log` with an embedded
/// CR/LF, and manipulating the terminal (colour, window title) of an admin
/// watching the log with embedded ANSI/OSC sequences.
///
/// This is the same pattern `main` already applies to the `title`/`user`
/// fields from argv; this helper collects the same policy in one place for the
/// strings and file paths coming from the raster header.
pub fn quote_untrusted(value: &str) -> String {
    format!("{:?}", value)
}

/// Validates that the page-header fields are within sensible bounds.
///
/// A corrupt or malicious CUPS Raster stream can report enormous
/// `bytesPerLine`, height or resolution values; since these are used directly
/// in buffer-size calculations, letting them through unvalidated could cause a
/// huge/excessive memory allocation (OOM) or a silently overflowing
/// computation. These bounds are far above realistic printer hardware; they
/// exist only to weed out plainly absurd values.
/// The highest `*Resolution` option in the PPD (1200 DPI).
pub const MAX_DPI: u32 = 1200;
/// The largest `*PaperDimension` (Legal: 1008 pt) + a sensible margin.
pub const MAX_POINTS: u32 = 1300;
/// ~1300 pt * 1200 dpi / 72 / 8 ≈ 2709 B (1-bit); rounded up with slack.
pub const MAX_BYTES_PER_LINE: u32 = 4096;
/// ~1300 pt * 1200 dpi / 72 ≈ 21,667 lines; rounded up with slack.
pub const MAX_LINES: u32 = 24_000;

/// The rounding slack allowed above the physical page width.
/// See the D-01 explanation in `validate_page_header` for the rationale.
pub const LINE_OVERSHOOT_SLACK_BYTES: u32 = 1;

/// The rounding slack allowed above the physical page height.
/// See the D-02 explanation in `validate_page_header` for the rationale.
pub const HEIGHT_OVERSHOOT_SLACK_LINES: u32 = 8;

pub fn validate_page_geometry(header: &PageGeometry) -> io::Result<()> {
    // The bounds are not arbitrary: they are derived from the highest
    // resolution and the largest paper that ppd/samsung-ml2160.ppd defines,
    // with a sensible margin left. The old bounds (1,000,000 bytes/line,
    // 10,000 DPI, 100,000 pt) were ~100x above what this hardware can
    // physically produce; a corrupt/malicious header could use that slack to
    // make it allocate enormous band buffers (see stream_page_bands).
    //
    // The link between the PPD and these constants is no longer a comment but a
    // test: `test_limits_cover_every_ppd_option` parses the PPD and verifies
    // that every `*Resolution` and `*PaperDimension` option stays within the
    // bounds. If a larger paper or higher resolution is added to the PPD the
    // test breaks and says the constants must be updated with it.
    let invalid = |msg: String| Err(io::Error::new(io::ErrorKind::InvalidData, msg));

    if header.bytes_per_line == 0 || header.bytes_per_line > MAX_BYTES_PER_LINE {
        return invalid(format!(
            "invalid cupsBytesPerLine value: {}",
            header.bytes_per_line
        ));
    }
    if header.height == 0 || header.height > MAX_LINES {
        return invalid(format!(
            "invalid page height (scanline count): {}",
            header.height
        ));
    }
    if !SplResolution::pair_is_supported(header.hw_resolution[0], header.hw_resolution[1]) {
        return invalid(format!(
            "unsupported resolution: {}x{} DPI (supported: 300x300, 600x600, 1200x600, 1200x1200)",
            header.hw_resolution[0], header.hw_resolution[1]
        ));
    }
    if header.page_size_points[0] == 0
        || header.page_size_points[0] > MAX_POINTS
        || header.page_size_points[1] == 0
        || header.page_size_points[1] > MAX_POINTS
    {
        return invalid(format!(
            "invalid page size (pt): {:?}",
            header.page_size_points
        ));
    }
    if SplPaperSize::from_dimensions_pt_exact(
        header.page_size_points[0],
        header.page_size_points[1],
    )
    .is_none()
    {
        return invalid(format!(
            "unsupported paper size: {} x {} pt; the QPDL paper code and the raster geometry must agree",
            header.page_size_points[0], header.page_size_points[1]
        ));
    }

    // D-07: `Margins[0]` is a geometry field too, and must be validated.
    //
    // Reject a physically impossible integer header margin. Historically this
    // field also drove band placement and could overflow the pixel calculation.
    // Placement now uses the separate 12.5 pt driver constant, preserving the
    // fractional point that the integer CUPS field cannot represent.
    if header.margins[0] >= header.page_size_points[0] {
        return invalid(format!(
            "invalid left margin: {} pt; it must be smaller than the page width ({} pt)",
            header.margins[0], header.page_size_points[0]
        ));
    }

    // The ML-2160 series QPDL engine expects single-plane, 1-bit monochrome
    // (K) raster: stream_page_bands interprets each byte directly as a single
    // black/white plane and inverts it unconditionally (see the polarity
    // explanation in that function). A stream that does not match this
    // assumption (e.g. 24-bit RGB or 32-bit CMYK), if silently mistaken for
    // 1-bit monochrome and sent to the printer, breaks band alignment, loses
    // firmware sync and wastes toner; so we reject it early.
    if header.color_space != CupsColorSpace::K {
        return invalid(format!(
            "unsupported colour space: {} (only 1-bit K/monochrome is supported)",
            header.color_space
        ));
    }
    if header.bits_per_color != 1 || header.bits_per_pixel != 1 {
        return invalid(format!(
            "unsupported bit depth: bitsPerColor={}, bitsPerPixel={} (only 1-bit monochrome is supported)",
            header.bits_per_color, header.bits_per_pixel
        ));
    }
    let expected_bytes_per_line = (header.width as u64 * header.bits_per_pixel as u64).div_ceil(8);
    if expected_bytes_per_line != header.bytes_per_line as u64 {
        return invalid(format!(
            "cupsBytesPerLine ({}) is inconsistent with cupsWidth ({}) (expected: {})",
            header.bytes_per_line, header.width, expected_bytes_per_line
        ));
    }

    // `cupsColorOrder` was never checked until now. For 1-bit single-channel
    // data the Chunked/Banded/Planar layouts are ALL IDENTICAL (one plane, one
    // channel), so all three are accepted; but an unrecognised value is a sign
    // that the producer uses a layout different from what this filter assumes,
    // and must not be silently misinterpreted.
    if let CupsColorOrder::Unknown(order) = header.color_order {
        return invalid(format!(
            "unrecognised cupsColorOrder value: {} (expected: 0=Chunked, 1=Banded, 2=Planar)",
            order
        ));
    }

    // D-01: `cupsBytesPerLine` must fit the PHYSICAL width of the page.
    //
    // The band width is computed from `page_size_points` and `hw_resolution`
    // (see compute_page_width_pixels); if the line is wider than that, the
    // excess used to be silently clipped. The `cupsWidth` consistency check
    // above does not catch it, because a header that is self-consistent but
    // inconsistent with the page (e.g. `PageSize = 1 pt` + `cupsBytesPerLine =
    // 620`) could shrink the band to 1 byte and drop 99.8% of each line.
    //
    // The slack is 1 byte because `page_width_pixels` is aligned up to 8, so
    // `bytes_per_line <= band_width_bytes` normally holds already; the 1-byte
    // slack only covers the producer rounding differently. A deviation within
    // that slack continues to be clipped, with a warning, in the page loop below.
    let band_width_bytes =
        compute_page_width_pixels(header.page_size_points[0], header.hw_resolution[0]).div_ceil(8);
    if header.bytes_per_line > band_width_bytes + LINE_OVERSHOOT_SLACK_BYTES {
        return invalid(format!(
            "cupsBytesPerLine ({}) does not fit the page width: {} pt @ {} DPI => at most {} bytes/line",
            header.bytes_per_line,
            header.page_size_points[0],
            header.hw_resolution[0],
            band_width_bytes
        ));
    }

    // D-02: `cupsHeight` must fit the PHYSICAL height of the page.
    //
    // The vertical counterpart of D-01. Height used to be checked only against
    // the global `MAX_LINES` limit; it was never compared with the page's own
    // size. A header that is self-consistent but inconsistent with the page
    // (e.g. `PageSize = 595 x 1 pt` + `cupsHeight = 24000`) was thus accepted,
    // a height of 24000 lines written into the QPDL page header, and 3-4x more
    // bands sent than fit on the page. The size reported to the printer
    // diverging from the data actually sent means, as in D-01, alignment/sync
    // loss on the printer side and wasted paper/toner.
    //
    // The slack is 8 lines because, unlike the width, there is NO alignment to
    // 8 here, so `compute_page_height_lines` leaves no extra headroom; the
    // slack only covers the producer rounding differently (round instead of
    // ceil, or aligning to a small block). Real cups-filters output stays far
    // below this bound, because it subtracts the PPD's `*ImageableArea`
    // margins: at A4 @600 DPI, against a physical 7017 lines the `cupsHeight`
    // measured on this system is 6817 (a 12 pt top + 12 pt bottom removes about
    // 200 lines). So no legitimate job hits this check.
    let page_height_lines =
        compute_page_height_lines(header.page_size_points[1], header.hw_resolution[1]);
    if header.height > page_height_lines + HEIGHT_OVERSHOOT_SLACK_LINES {
        return invalid(format!(
            "cupsHeight ({}) does not fit the page height: {} pt @ {} DPI => at most {} lines",
            header.height, header.page_size_points[1], header.hw_resolution[1], page_height_lines
        ));
    }

    Ok(())
}

/// The Rust counterpart of the `pageWidth` computation in SpliX document.cpp.
///
/// SpliX source:
///   pageWidth = ((unsigned long)ceil(convertToXResolution(
///       request.printer()->pageWidth())) + 7) & ~7;
///
/// page_size_pt: page width (1/72 inch, CUPS header.PageSize[0])
/// x_dpi: horizontal resolution (CUPS header.HWResolution[0])
pub fn compute_page_width_pixels(page_size_pt: u32, x_dpi: u32) -> u32 {
    let px = (page_size_pt as f64 * x_dpi as f64 / 72.0).ceil() as u32;
    (px + 7) & !7u32
}

/// How many raster lines the physical height of the page corresponds to.
///
/// The vertical counterpart of `compute_page_width_pixels`, with two
/// differences: on the vertical axis there is NO rounding to 8, because no
/// band/DMA alignment is needed there, and the resolution is
/// `hw_resolution[1]` — a distinction that really can be triggered, because the
/// PPD offers an asymmetric option like `1200x600dpi`.
///
/// Used only as an UPPER BOUND in `validate_page_header`'s D-02 check; the page
/// loop does not rescale the line count to it (as the width does in D-01),
/// because the number of lines to send is determined by `cupsHeight`.
pub fn compute_page_height_lines(page_size_pt: u32, y_dpi: u32) -> u32 {
    (page_size_pt as f64 * y_dpi as f64 / 72.0).ceil() as u32
}

/// Converts the printer's HARD MARGIN into a band-buffer byte offset.
///
/// SpliX compress.cpp `_compressBandedPage`:
///
/// ```c
/// hardMarginX = ((unsigned long)ceil(page->convertToXResolution(
///     request.printer()->hardMarginX())) + 7) & ~7;
/// hardMarginXInB = hardMarginX / 8;
/// ```
///
/// The source is the driver constant, not integer CUPS Margins[]. This keeps
/// the selected 12.5 pt value intact: CUPS' integer field cannot represent it.
/// See docs/DECISIONS.md (2026-09-06). At 600 dpi it is 14 byte columns.
pub fn hard_margin_bytes(margin_pt: f64, x_dpi: u32) -> usize {
    let px = (margin_pt * x_dpi as f64 / 72.0).ceil() as u32;
    (((px + 7) & !7u32) / 8) as usize
}

/// The vertical counterpart of [`hard_margin_bytes`], in scanlines.
///
/// Two differences from the horizontal rule, and neither is cosmetic:
///
/// * **No 8-alignment.** `compute_page_height_lines` states the reason: the
///   band buffer is byte addressed horizontally, so the horizontal margin is
///   rounded up to a whole 8-pixel column, while the vertical axis addresses
///   scanlines individually and is not aligned.
/// * **`round`, not `ceil`.** The `ceil` in [`hard_margin_bytes`] costs
///   nothing, because the same rounded value is used on both sides of the
///   classic path's placement and cancels out of it. Vertically there is
///   nothing to cancel against: the 1.x filter never subtracts a vertical
///   margin at all, because cups-filters already centred the printable area on
///   the sheet — as `docs/MARGINS.md` puts it, centring and `hardMarginY`
///   "cancel exactly". So this value is not a matching convention but an
///   estimate of a physical distance, and the nearest scanline is the closest
///   the axis can come to it. At 600 dpi 12.5 pt is 104.17 lines: `round`
///   gives 104 and lands 0.17 lines low, `ceil` gives 105 and lands 0.83 lines
///   high, and 104 is also what cups-filters' own centring implies for the
///   classic path, so the two front ends stay within a scanline of each other.
///
/// Like every number on this path it is provisional until release gate G-1
/// measures a printed page; see `docs/GOLDEN-VALIDATION.md` and open question
/// Q-13 in `docs/DECISIONS.md`.
pub fn hard_margin_lines(margin_pt: f64, y_dpi: u32) -> u32 {
    (margin_pt * y_dpi as f64 / 72.0).round() as u32
}

/// The horizontal placement of a CUPS line within the band buffer.
///
/// The two fields together represent a single signed offset: `dst_offset` is a
/// positive shift, `src_skip` a negative one (bytes dropped from the left of
/// the line). Both cannot be greater than zero at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPlacement {
    /// The column (byte) where the content starts in the band buffer.
    pub dst_offset: usize,
    /// The number of bytes to skip from the start of the CUPS line.
    pub src_skip: usize,
}

/// Computes the horizontal position of a CUPS line within the band buffer the
/// same way SpliX does.
///
/// D-06 regression. This used to apply only CENTRING
/// (`(bandWidthInB - lineSize) / 2`) and never subtract the hard margin; but
/// SpliX applies both steps:
///
/// ```c
/// // document.cpp:120 — centre the line within the page width
/// marginWidthInB = (pageWidthInB - lineSize) / 2;
/// // compress.cpp:227 — SKIP the hard margin while filling the band
/// band[x * bandHeight + y] = planes[i][index + x + hardMarginXInB + ...];
/// ```
///
/// The net offset is `centring - hardMarginXInB`. At A4 @600 DPI the centring
/// is `(620 - 595) / 2 = 12` bytes and the hard margin 13 bytes, so the
/// content starts at column 0 of the band. With centring alone the content
/// shifted 12 bytes (96 pixels ≈ 11.5 pt ≈ 4 mm) to the right and ran its
/// right edge past the printable area.
///
/// This also keeps consistency with the vertical axis: vertically there was
/// never any shift (the page loop writes the first line to band line 0) and
/// SpliX's vertical net is zero too — centring `(7017 - 6817) / 2 = 100` lines,
/// hard margin `hardMarginY = 100` lines. The two axes assuming a different
/// origin was the bug itself.
pub fn band_placement(
    band_width_bytes: usize,
    cups_line_bytes: usize,
    hard_margin_bytes: usize,
) -> io::Result<BandPlacement> {
    let centered = band_width_bytes.saturating_sub(cups_line_bytes) / 2;
    let src_skip = hard_margin_bytes.saturating_sub(centered);

    // Skipping the whole line is rejected. There used to be a clamp here
    // (`.min(cups_line_bytes - 1)`) whose stated reason was "do not produce a
    // blank page"; but the clamp itself was what produced the blank page: the
    // single remaining byte landed in an arbitrary column of the line and the
    // rest of the page was silently lost. A hard margin exceeding the line
    // width is a physically inconsistent geometry; the rest of the file (see
    // D-01/D-02) rejects such a geometry rather than silently correcting it, so
    // we reject it here too.
    if src_skip >= cups_line_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the hard margin ({} B) exceeds the line width ({} B): band {} B, \
                 centred at {} B; no content would be left to print",
                hard_margin_bytes, cups_line_bytes, band_width_bytes, centered
            ),
        ));
    }

    Ok(BandPlacement {
        dst_offset: centered.saturating_sub(hard_margin_bytes),
        src_skip,
    })
}

/// Converts the duplex information in the CUPS Raster page header into a QPDL
/// duplex mode.
///
/// SpliX `request.cpp` makes this decision through the PPD:
///
/// ```c
/// manualDuplex = ppd->get("ManualDuplex", "QPDL").isTrue();
/// if (value == "DuplexNoTumble") _duplex = manualDuplex ? ManualLongEdge : LongEdge;
/// else if (value == "DuplexTumble") _duplex = manualDuplex ? ManualShortEdge : ShortEdge;
/// else _duplex = Simplex;
/// ```
///
/// The PPD option (`DuplexNoTumble`/`DuplexTumble`) reaches us as the
/// `Duplex` + `Tumble` pair in the CUPS raster header: `Duplex=false` ->
/// Simplex, `Duplex=true, Tumble=false` -> long edge, `Duplex=true,
/// Tumble=true` -> short edge.
///
/// This project's PPD, like upstream SpliX's sibling-model PPDs
/// (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`), declares
/// `*QPDL ManualDuplex: "On"`, and the ML-2160 series has no automatic-duplex
/// hardware; so `manualDuplex` is always true in this family and the result is
/// always one of the `Manual*` variants.
///
/// WARNING: this path is CURRENTLY UNREACHABLE. Because the project's PPD has
/// no `*OpenUI *Duplex` block, the `Duplex` field in the CUPS raster header is
/// never set. The mapping is kept correct anyway so the protocol side is ready
/// when a duplex option is added to the PPD. For the two gaps to know about
/// before it is added, see the note below `stream_page_bands`.
/// MANUAL-DUPLEX GAP (2/2): manual duplex requires the job to be printed in
/// two passes — one side first, then the other after the operator flips and
/// reloads the paper. This filter sends pages in a single pass in the order
/// they arrive in the stream; it does not split the page order into passes. If
/// an `*OpenUI *Duplex` block is added to the PPD, this flow (and item 1/2
/// above) must be resolved too, or duplex jobs print in the wrong order.
pub fn duplex_mode(duplex: bool, tumble: bool) -> SplDuplex {
    if !duplex {
        SplDuplex::Simplex
    } else if tumble {
        SplDuplex::ManualShortEdge
    } else {
        SplDuplex::ManualLongEdge
    }
}

/// Converts the `MediaType` field in the CUPS Raster page header into the
/// printer's PJL `PAPERTYPE` value; every unrecognised value falls back to
/// `OFF`.
///
/// This field comes from the PPD's `*MediaType` option (`<</MediaType(ENV)>>
/// setpagedevice` -> `MediaType = "ENV"` in the header) and used to be ignored
/// entirely: the filter wrote `@PJL SET PAPERTYPE=OFF` unconditionally on
/// every job, so a user selecting envelope/label/card stock printed with
/// plain-paper fuser settings.
///
/// The fallback is NOT silent: if the PPD's and the filter's vocabularies
/// diverge (e.g. an old PPD carrying readable names is still installed), this
/// line says that the user's choice did not reach the printer.
pub fn pjl_paper_type_for(media_type: &str, log: &dyn Log) -> &'static str {
    if media_type.is_empty() {
        return qpdl::PJL_PAPERTYPE_DEFAULT;
    }
    match qpdl::pjl_paper_type(media_type) {
        Some(paper_type) => paper_type,
        None => {
            // `MediaType` is a free 64-byte C string coming from the client
            // that submitted the job; it is as untrusted as `title`/`user` in
            // argv, so it is printed escaped.
            log.log(
                Level::Warning,
                &format!(
                    "Unrecognised MediaType {}; sending @PJL SET PAPERTYPE={}. The \
                     PPD's *MediaType keys must come from the printer's own PJL \
                     vocabulary: {}.",
                    quote_untrusted(media_type),
                    qpdl::PJL_PAPERTYPE_DEFAULT,
                    qpdl::PJL_PAPER_TYPES.join(", ")
                ),
            );
            qpdl::PJL_PAPERTYPE_DEFAULT
        }
    }
}

/// The base value of the QPDL band height, in lines.
///
/// SpliX reads this from the PPD (`*QPDL BandSize: "128"`); both upstream
/// SpliX's sibling-model PPDs (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`,
/// `ml1640.ppd`, `ml2510.ppd`) and this project's PPD say 128.
pub const QPDL_BAND_HEIGHT: usize = 128;

/// The band height to use for a page.
///
/// SpliX `compress.cpp` `_compressBandedPage` (Algo 0x11 goes down this path;
/// see the `compressPage` dispatcher in the same file, 0x0D/0x0E/0x11 ->
/// banded):
///
/// ```c
/// bandHeight = request.printer()->bandHeight();   // PPD: *QPDL BandSize
/// if (page->xResolution() == 300 && page->yResolution() == 300)
///     bandHeight /= 2;
/// ```
///
/// So at 300x300 DPI the band height is 64, not 128. The rule is
/// unconditional and affects three places at once: the size of the band buffer
/// (`bandWidthInB * bandHeight`), the transposed indexing
/// (`band[x * bandHeight + y]`) and the height field written into the band
/// record. This filter used to use 128 at every resolution; when the PPD's
/// `300dpi` option was selected, the printer expected 64-line bands but was
/// sent data transposed for 128.
///
/// Note: the asymmetric `1200x600dpi` mode falls OUTSIDE this rule — the
/// condition requires both axes to be 300 — and stays at 128.
pub fn band_height_for(hw_resolution: [u32; 2]) -> usize {
    if hw_resolution[0] == 300 && hw_resolution[1] == 300 {
        QPDL_BAND_HEIGHT / 2
    } else {
        QPDL_BAND_HEIGHT
    }
}

/// The QPDL band-order field is 8 bits wide, so a page may carry at most 256
/// bands; `write_compressed_band` fails the job rather than wrapping the
/// index. This assertion proves the job can never legitimately be refused for
/// that reason, from the validator's own limits rather than from the media
/// table:
///
/// * A page is at most `MAX_POINTS` tall and its vertical resolution at most
///   `MAX_DPI` (`validate_page_header`), so it carries at most
///   `MAX_POINTS * MAX_DPI / 72` lines — 21667, or 170 bands of 128.
/// * The halved band height is selected only when BOTH axes are 300 dpi, so
///   the 64-line band can only ever pair with a 300 dpi line count: 5417
///   lines, or 85 bands.
///
/// Both are inside the field with room to spare; the worst case reachable
/// from the current 12.5 pt PPD is Legal at 1200x1200, 128 bands
/// (`test_band_count_stays_inside_the_qpdl_band_order_field`). If the paper
/// table, the resolution list or `QPDL_BAND_HEIGHT` ever change enough to
/// break that, this fails at compile time instead of on paper.
const _: () = {
    const CEILING: usize = u8::MAX as usize + 1;
    // `usize::div_ceil` is not const, so the rounding is spelled out here.
    #[allow(clippy::manual_div_ceil)]
    const fn div_ceil(a: usize, b: usize) -> usize {
        (a + b - 1) / b
    }

    let lines_at_max_dpi = div_ceil(MAX_POINTS as usize * MAX_DPI as usize, 72);
    assert!(div_ceil(lines_at_max_dpi, QPDL_BAND_HEIGHT) <= CEILING);

    let lines_at_300_dpi = div_ceil(MAX_POINTS as usize * 300, 72);
    assert!(div_ceil(lines_at_300_dpi, QPDL_BAND_HEIGHT / 2) <= CEILING);
};

/// The maximum number of pages processed in a single print job.
///
/// The page loop had no upper bound: the longer the stream, the more pages
/// were produced. This left paper/toner consumption unbounded (even more so
/// multiplied by `MAX_REALISTIC_COPIES`), and made the compressor's per-page
/// cost unbounded in total. Because the filter processes the CUPS queue with a
/// single thread, a long job holds up every job behind it.
///
/// The limit was LOWERED from 5,000 to 1,000. The rationale is the target
/// hardware itself: the ML-2160 series prints ~20 pages/minute, so a
/// 1,000-page job already keeps the printer busy ~50 minutes and consumes two
/// reams. 5,000 pages (~4 hours of continuous printing) is not a real document
/// on a personal printer of this class, only the ceiling of an abuse scenario.
/// Lowering the limit lowers the filter's worst-case CPU cost by the same
/// proportion (see `MAX_JOB_RASTER_BYTES`).
pub const MAX_PAGES_PER_JOB: u32 = 1_000;

/// The maximum RAW RASTER volume (bytes) processed in a single print job.
///
/// `MAX_PAGES_PER_JOB` alone is insufficient, because it is RESOLUTION-BLIND:
/// the cost of processing a page scales with byte count, not page count.
/// Measured values (on this machine, a release build, at a real band size of
/// 346,752 bytes):
///
/// * compressible (zero-filled) band: ~166 MB/s
/// * INCOMPRESSIBLE noise: ~6.65 MB/s
///
/// The ~25x gap between them means a byte budget can only crudely bound CPU
/// time; so the budget must be chosen so the worst case stays acceptable.
///
/// INDEPENDENCE FROM INPUT SIZE: this budget counts the DECODED raster volume,
/// not the input volume. With CUPS Raster v2's line-RLE, at the largest
/// geometry the validator accepts (Legal @1200 DPI, 1276 B/line x 16,808 lines
/// with the rounding slack), an expansion of about 11,100x is possible. Even a
/// fully white stream of about 0.74 MiB can spawn more than 8 GiB of raster
/// work; the only real defence is to bound the decoded data directly.
///
/// 8 GiB was chosen so both limits stay meaningful:
///
/// * the A4 page measured @600 DPI is 595 x 6817 = ~3.87 MiB;
///   `MAX_PAGES_PER_JOB` of them (1,000 pages) is ~3.78 GiB, i.e. under half
///   the budget. No normal-resolution job up to the page limit HITS this
///   check.
/// * at the largest acceptable page (~20.45 MiB) the budget engages at page
///   401 and, at the measured worst-case compression speed, bounds the job to
///   about 22 minutes.
pub const MAX_JOB_RASTER_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// The maximum number of SHEETS (pages x copies) a single print job can produce.
///
/// `MAX_PAGES_PER_JOB` and `MAX_REALISTIC_COPIES` were each justified
/// separately, but their PRODUCT was bounded nowhere: with the old values a
/// single job could produce 5,000 x 999 = 4,995,000 sheet-print commands.
/// Because `num_copies` comes from the page header, i.e. the untrusted side,
/// this is the only limit that is meaningful for paper/toner consumption — not
/// the page count, the sheet count.
///
/// 10,000 sheets is ~8 hours of continuous printing at ~20 pages/minute:
/// generous enough that no legitimate job comes near it, yet five hundred
/// times short of millions of sheets.
pub const MAX_JOB_IMPRESSIONS: u64 = 10_000;

/// The combined accounting of the resources a job consumes.
///
/// All three counters live in one place because they cover each other's
/// blindness: page count is resolution-blind, raster volume is copy-blind, and
/// sheet count is page-size-blind. Applied separately, the multiplicative gaps
/// between them (see `MAX_JOB_IMPRESSIONS`) went unnoticed.
#[derive(Debug, Default)]
pub struct JobBudget {
    pub pages: u32,
    pub raster_bytes: u64,
    pub impressions: u64,
}

impl JobBudget {
    /// Accounts a validated page against the budget and returns the page's
    /// 1-BASED index. If any of the limits is exceeded, the job stops here.
    ///
    /// `copies` must be the value that has PASSED through `sanitize_copies`:
    /// counting with the raw `num_copies` would charge the budget for copies
    /// that are never actually sent to the printer.
    ///
    /// The additions use `saturating_add`: since the per-page volume is bounded
    /// to ~59 MB by `validate_page_header` and the page count by
    /// `MAX_PAGES_PER_JOB`, a `u64` overflow is already impossible — but if the
    /// limits change, being counted as over budget is the right behaviour
    /// rather than silently wrapping.
    pub fn account_page(&mut self, page_raster_bytes: u64, copies: u16) -> io::Result<u32> {
        let exceeded = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);

        self.pages += 1;
        if self.pages > MAX_PAGES_PER_JOB {
            return Err(exceeded(format!(
                "the job exceeded the page limit: no more than {} pages are processed. \
                 If the document really is that long, split the job.",
                MAX_PAGES_PER_JOB
            )));
        }

        self.raster_bytes = self.raster_bytes.saturating_add(page_raster_bytes);
        if self.raster_bytes > MAX_JOB_RASTER_BYTES {
            return Err(exceeded(format!(
                "the job exceeded the raster volume limit: no more than {} bytes are \
                 processed ({} so far). If the document really is that large, split \
                 the job or choose a lower resolution.",
                MAX_JOB_RASTER_BYTES, self.raster_bytes
            )));
        }

        self.impressions = self.impressions.saturating_add(copies as u64);
        if self.impressions > MAX_JOB_IMPRESSIONS {
            return Err(exceeded(format!(
                "the job exceeded the sheet limit: no more than {} sheets are printed \
                 ({} so far = pages x copies). Lower the copy count or split the job.",
                MAX_JOB_IMPRESSIONS, self.impressions
            )));
        }

        Ok(self.pages)
    }
}

/// The maximum copy count considered reasonable for a realistic print job.
///
/// QPDL's copy field is 16-bit (theoretical ceiling 65535), but no real job
/// asks for anything near that limit; 999 leaves an extra safety margin
/// against a corrupt/excessive header that would waste paper/toner or make the
/// printer physically print for hours on end.
pub const MAX_REALISTIC_COPIES: u16 = 999;

/// Normalises the `num_copies` (u32) field in the CUPS Raster header so it
/// safely fits QPDL's 16-bit copy-count field.
///
/// The previous `header.num_copies.max(1) as u16` expression silently
/// overflowed to 0 at values like 65536 (2^16) and its multiples
/// (`u16::MAX + 1 == 0`), which would send the printer an effective "print 0
/// copies" command. `clamp(1, MAX_REALISTIC_COPIES)` guarantees both the lower
/// and upper bound at once: 0 never passes, and excessive values are pinned to
/// a realistic ceiling rather than silently overflowing (even though they
/// would technically fit the 16-bit field).
pub fn sanitize_copies(num_copies: u32) -> u16 {
    num_copies.clamp(1, MAX_REALISTIC_COPIES as u32) as u16
}
