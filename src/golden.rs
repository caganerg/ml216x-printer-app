//! # Golden-file harness
//!
//! This module freezes the SPL2/QPDL bytes the 1.x CUPS filter PRODUCES,
//! before the code moves to a PAPPL Printer Application. The acceptance
//! criterion for that migration is byte-for-byte identity (project rule 6);
//! that criterion needs a reference to compare against, and the reference can
//! only be captured NOW, while the 1.x code still runs.
//!
//! ## What is stored
//!
//! Two files per case:
//!
//! * `goldens/<case>.spl`  — the raw SPL2/QPDL stream the filter produced.
//! * `goldens/<case>.json` — the fields of the CLASSIC CUPS page header that
//!   produced that stream, plus the QPDL placement values the filter derived
//!   from it.
//!
//! The JSON sidecar exists for the second half of the migration: the option
//! mapping on the PAPPL side (media size, resolution, tray, media type,
//! margin) will be validated against these files. The classic header is no
//! longer produced on the PAPPL path, so these values, too, can only be
//! captured now.
//!
//! ## Why the inputs are not committed
//!
//! The raster inputs (~16 MB for A4 @1200 DPI) are produced DETERMINISTICALLY
//! by `build_raster` in this file, so they are not stored in the repository.
//! If the generator itself changes, that shows up in the header fields of the
//! JSON sidecar — the drift is not silent, it is visible in review.
//!
//! ## Refreshing
//!
//! ```text
//! UPDATE_GOLDENS=1 cargo test golden
//! ```
//!
//! Goldens may ONLY be refreshed together with a deliberate behaviour change;
//! every diff in a `.spl` file means the bytes going to the printer changed.

use std::fmt::Write as _;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use spl2_core::geometry::{
    band_height_for, band_placement, compute_page_width_pixels, hard_margin_bytes, sanitize_copies,
};
use spl2_core::qpdl::{current_service_date, SplPaperSize, SplPaperSource};
use spl2_core::raster::{CupsRasterVersion, PageHeader};

use crate::{process_with_margin, CupsFilterArgs};

/// The FIXED service date written into the goldens.
///
/// The `@PJL DEFAULT SERVICEDATE` line normally carries today's date; without
/// a fixed value the goldens would break every midnight, and loosening the
/// comparison to "ignore the SERVICEDATE line" would also hide REAL drift on
/// that line. So the date is not loosened, it is pinned (see the
/// `process_cups_raster_to_spl` doc comment).
const GOLDEN_SERVICE_DATE: &str = "20260101";

/// Job title / user name for the goldens — they come from here rather than
/// from argv so the PJL fields stay fixed too.
const GOLDEN_TITLE: &str = "golden";
const GOLDEN_USER: &str = "tester";

/// A medium, as defined by the PPD.
///
/// `dimension_pt` = `*PaperDimension`, `imageable_pt` = `*ImageableArea`
/// (left, bottom, right, top). The values are copied verbatim from the
/// corresponding lines of `ppd/samsung-ml2160.ppd`;
/// `test_golden_media_matches_ppd` turns that link from a comment into a
/// verified fact.
#[derive(Debug, Clone, Copy)]
struct Media {
    /// PPD `*PageSize` keyword.
    ppd_key: &'static str,
    /// `*PaperDimension <key>: "W H"`.
    dimension_pt: (u32, u32),
    /// `*ImageableArea <key>: "L B R T"`.
    imageable_pt: (f64, f64, f64, f64),
}

impl Media {
    /// Width/height of the imageable area, in points.
    fn imageable_size_pt(&self) -> (f64, f64) {
        (
            self.imageable_pt.2 - self.imageable_pt.0,
            self.imageable_pt.3 - self.imageable_pt.1,
        )
    }
}

const A4: Media = Media {
    ppd_key: "A4",
    dimension_pt: (595, 842),
    imageable_pt: (12.5, 12.5, 582.5, 829.5),
};
const LETTER: Media = Media {
    ppd_key: "Letter",
    dimension_pt: (612, 792),
    imageable_pt: (12.5, 12.5, 599.5, 779.5),
};
const A5: Media = Media {
    ppd_key: "A5",
    dimension_pt: (420, 595),
    imageable_pt: (12.5, 12.5, 407.5, 582.5),
};
const ENV_C5: Media = Media {
    ppd_key: "EnvC5",
    dimension_pt: (459, 649),
    imageable_pt: (12.5, 12.5, 446.5, 636.5),
};
const LEGAL: Media = Media {
    ppd_key: "Legal",
    dimension_pt: (612, 1008),
    imageable_pt: (12.5, 12.5, 599.5, 995.5),
};
const FOLIO: Media = Media {
    ppd_key: "Folio",
    dimension_pt: (595, 935),
    imageable_pt: (12.5, 12.5, 582.5, 922.5),
};
const EXECUTIVE: Media = Media {
    ppd_key: "Executive",
    dimension_pt: (522, 756),
    imageable_pt: (12.5, 12.5, 509.5, 743.5),
};
const A6: Media = Media {
    ppd_key: "A6",
    dimension_pt: (297, 420),
    imageable_pt: (12.5, 12.5, 284.5, 407.5),
};
const B5: Media = Media {
    ppd_key: "B5",
    dimension_pt: (516, 729),
    imageable_pt: (12.5, 12.5, 503.5, 716.5),
};
const ENV_10: Media = Media {
    ppd_key: "Env10",
    dimension_pt: (297, 684),
    imageable_pt: (12.5, 12.5, 284.5, 671.5),
};
const ENV_DL: Media = Media {
    ppd_key: "EnvDL",
    dimension_pt: (312, 624),
    imageable_pt: (12.5, 12.5, 299.5, 611.5),
};

/// Every medium the PPD declares. `test_golden_media_matches_ppd` checks each
/// one against `ppd/samsung-ml2160.ppd`, and
/// `test_golden_corpus_covers_every_ppd_medium` checks that each one has a
/// golden case: the hard-margin table is per-size, so an uncovered size is an
/// uncovered margin.
const PPD_MEDIA: &[Media] = &[
    A4, LETTER, LEGAL, EXECUTIVE, A5, A6, B5, ENV_10, ENV_DL, ENV_C5, FOLIO,
];

/// The resolutions the PPD declares.
const PPD_RESOLUTIONS: &[(u32, u32)] = &[(300, 300), (600, 600), (1200, 600), (1200, 1200)];

// --- Synthetic media: the ONLY way to reach `dst_offset > 0` ---------------
//
// `band_placement` positions the CUPS line as `centred - hard_margin`. Every
// medium in this PPD has a 12 pt left margin, which at every supported
// resolution makes the hard margin greater than or equal to the centring
// offset, so `dst_offset` is 0 in every realistic case and only `src_skip`
// varies. The positive branch is therefore unreachable from the shipped PPD —
// and it is exactly the branch that activates the first time PAPPL hands us a
// different margin (R-1). These two media exist to freeze it.
//
// They are FABRICATED: no printer reports these imageable areas. What they do
// keep real is the sheet size — `validate_page_header` rejects any
// `PageSize` that is not an exact known QPDL paper size, so both reuse A4's
// 595 x 842 pt sheet and vary only the imageable area, i.e. exactly the two
// header fields (`Margins[0]`, `cupsWidth`) that PAPPL could plausibly
// deliver differently. `test_golden_synthetic_media_use_a_real_sheet` enforces
// that.

/// A4 sheet, 6 pt left margin, near-full-width imageable area: hard margin 7 B
/// against a centring offset of 21 B, so `dst_offset` = 14.
const SYNTH_A4_NARROW_MARGIN: Media = Media {
    ppd_key: "A4",
    dimension_pt: (595, 842),
    imageable_pt: (6.0, 12.0, 560.0, 830.0),
};

/// A4 sheet, 12 pt left margin but a much narrower imageable area: hard margin
/// 13 B against a centring offset of 107 B, so `dst_offset` = 94.
const SYNTH_A4_NARROW_AREA: Media = Media {
    ppd_key: "A4",
    dimension_pt: (595, 842),
    imageable_pt: (12.0, 12.0, 400.0, 830.0),
};

/// The fabricated media, kept out of `PPD_MEDIA` so the PPD checks stay honest.
const SYNTHETIC_MEDIA: &[Media] = &[SYNTH_A4_NARROW_MARGIN, SYNTH_A4_NARROW_AREA];

/// The pixel rounding `cups-filters` performs: `round(pt * dpi / 72)`.
///
/// This rule is not a guess, it is a MEASUREMENT: all eight real `cupsHeight`
/// values in `test_validate_page_header_accepts_real_cupsfilter_heights` (A4
/// @300/600/1200, Letter @600, Legal @600/1200, A6 @600, Folio @1200) are
/// reproduced exactly by this formula, and
/// `test_golden_geometry_matches_measured_cupsfilter_output` verifies it.
/// `ceil` or `floor` misses at least one of the eight.
fn px_from_pt(pt: f64, dpi: u32) -> u32 {
    (pt * f64::from(dpi) / 72.0).round() as u32
}

/// Page content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// A completely blank page.
    Blank,
    /// A one-pixel registration mark in EACH OF THE FOUR CORNERS of the
    /// imageable area.
    ///
    /// This is the case that makes a margin regression fail at byte level
    /// (see `docs/MIGRATION-PLAN.md` R-1). A corner mark pins the position of
    /// exactly one bit in the band buffer; if horizontal placement shifts by
    /// one byte the compressed payload changes and the golden comparison
    /// breaks without anyone reaching for a ruler.
    ///
    /// NOTE: the left-hand marks do NOT always reach paper. On A4 @600 DPI the
    /// hard margin is 13 bytes and the centring offset is 12, so the net
    /// `src_skip` is 1 — the first byte of the CUPS line (and with it the x=0
    /// mark) is dropped. That is not a bug, it is SpliX's behaviour, and it is
    /// exactly what the golden records. The `src_skip_bytes` field in the
    /// sidecar says how much was dropped in which case.
    RegistrationMarks,
}

/// The CUPS Raster version of the input stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// `RaS3` — uncompressed.
    V3,
    /// `RaS2` — line RLE (the same body encoding PWG Raster uses).
    V2Rle,
}

/// One golden case.
#[derive(Debug, Clone, Copy)]
struct Case {
    name: &'static str,
    media: Media,
    /// (x_dpi, y_dpi)
    resolution: (u32, u32),
    copies: u32,
    /// CUPS `MediaPosition` (PPD `*InputSlot`); 0 = not selected.
    media_position: u32,
    /// CUPS `MediaType` (PPD `*MediaType`); empty = not selected.
    media_type: &'static str,
    pages: u32,
    content: Content,
    encoding: Encoding,
    /// The job title passed as argv[3]. Fixed per case so the PJL job-name
    /// line stays deterministic.
    title: &'static str,
}

impl Case {
    const fn new(name: &'static str, media: Media, resolution: (u32, u32)) -> Self {
        Self {
            name,
            media,
            resolution,
            copies: 1,
            media_position: 0,
            media_type: "",
            pages: 1,
            content: Content::RegistrationMarks,
            encoding: Encoding::V3,
            title: GOLDEN_TITLE,
        }
    }

    /// Pixel width of the CUPS line — derived from the imageable area.
    fn width_px(&self) -> u32 {
        px_from_pt(self.media.imageable_size_pt().0, self.resolution.0)
    }

    fn height_lines(&self) -> u32 {
        px_from_pt(self.media.imageable_size_pt().1, self.resolution.1)
    }

    fn bytes_per_line(&self) -> u32 {
        self.width_px().div_ceil(8)
    }
}

/// The golden corpus.
///
/// Coverage rationale:
///
/// * The `*-marks` cases exist for A4 and Letter at EVERY SUPPORTED
///   RESOLUTION (300x300, 600x600, 1200x600, 1200x1200), which stretches both
///   the margin and the band-height rules. The asymmetric 1200x600 mode is
///   the only case that catches the resolution axes being swapped (R-4).
/// * `a4-300-marks` additionally covers the 64-line band rule (R-3); every
///   other case produces 128-line bands.
/// * `a5` and `envc5` cover small media and envelope geometry.
/// * `*-3copies` freezes the copy count being written in two places at once
///   (page header + end of page) (R-5).
/// * `*-manual-env` freezes the tray and media-type mapping.
/// * `*-v2rle` produces the same page from a line-RLE input; it must come out
///   BYTE-FOR-BYTE identical to `*-600-marks` (`test_golden_v2_and_v3_agree`).
/// * `a4-600-blank` freezes the compressor's behaviour on a completely empty
///   band.
const CASES: &[Case] = &[
    // --- A4, every resolution ---
    Case::new("a4-300-marks", A4, (300, 300)),
    Case::new("a4-600-marks", A4, (600, 600)),
    Case::new("a4-1200x600-marks", A4, (1200, 600)),
    Case::new("a4-1200-marks", A4, (1200, 1200)),
    // --- Letter, every resolution ---
    Case::new("letter-300-marks", LETTER, (300, 300)),
    Case::new("letter-600-marks", LETTER, (600, 600)),
    Case::new("letter-1200x600-marks", LETTER, (1200, 600)),
    Case::new("letter-1200-marks", LETTER, (1200, 1200)),
    // --- other media ---
    Case::new("a5-600-marks", A5, (600, 600)),
    Case::new("envc5-600-marks", ENV_C5, (600, 600)),
    // --- blank page ---
    Case {
        content: Content::Blank,
        ..Case::new("a4-600-blank", A4, (600, 600))
    },
    // --- copies ---
    Case {
        copies: 3,
        ..Case::new("a4-600-marks-3copies", A4, (600, 600))
    },
    // --- tray + media type ---
    Case {
        media_position: 2,
        media_type: "ENV",
        ..Case::new("envc5-600-marks-manual-env", ENV_C5, (600, 600))
    },
    // --- multiple pages ---
    Case {
        pages: 3,
        ..Case::new("a4-600-marks-3pages", A4, (600, 600))
    },
    // --- v2 line-RLE input ---
    Case {
        encoding: Encoding::V2Rle,
        ..Case::new("a4-600-marks-v2rle", A4, (600, 600))
    },
    // --- every remaining PPD medium at the default resolution ---
    Case::new("legal-600-marks", LEGAL, (600, 600)),
    Case::new("folio-600-marks", FOLIO, (600, 600)),
    Case::new("executive-600-marks", EXECUTIVE, (600, 600)),
    Case::new("a6-600-marks", A6, (600, 600)),
    Case::new("b5-600-marks", B5, (600, 600)),
    Case::new("env10-600-marks", ENV_10, (600, 600)),
    Case::new("envdl-600-marks", ENV_DL, (600, 600)),
    // --- the two geometric extremes at every resolution ---
    Case::new("legal-300-marks", LEGAL, (300, 300)),
    Case::new("legal-1200x600-marks", LEGAL, (1200, 600)),
    Case::new("legal-1200-marks", LEGAL, (1200, 1200)),
    Case::new("folio-300-marks", FOLIO, (300, 300)),
    Case::new("folio-1200x600-marks", FOLIO, (1200, 600)),
    Case::new("folio-1200-marks", FOLIO, (1200, 1200)),
    // --- synthetic: the only cases with dst_offset > 0 ---
    Case::new("synth-a4-600-dst14", SYNTH_A4_NARROW_MARGIN, (600, 600)),
    Case::new("synth-a4-600-dst94", SYNTH_A4_NARROW_AREA, (600, 600)),
    // --- copies clamp combined with multiple pages ---
    //
    // 1000 copies is above MAX_REALISTIC_COPIES, so `sanitize_copies` clamps
    // it to 999; the clamped value is written into the page header AND the
    // end-of-page footer, on each of the two pages. That combination is what
    // R-5 warns can silently turn into copies squared.
    Case {
        copies: 1000,
        pages: 2,
        ..Case::new("a4-600-marks-2pages-1000copies", A4, (600, 600))
    },
    // --- non-ASCII job metadata ---
    //
    // PAPPL passes the IPP job-name through as UTF-8; the classic filter path
    // rarely saw anything but ASCII. This freezes what `quote_untrusted` makes
    // of a multi-byte title before that changes.
    Case {
        title: "Örnek Çıktı — ünlü ğüş İŞ",
        ..Case::new("a4-600-marks-utf8-title", A4, (600, 600))
    },
];

// ============================================================================
// Raster input generator
// ============================================================================

/// Produces a single raster line.
fn build_line(case: &Case, y: u32) -> Vec<u8> {
    let bpl = case.bytes_per_line() as usize;
    let mut line = vec![0u8; bpl];

    if case.content == Content::RegistrationMarks {
        let last_y = case.height_lines() - 1;
        if y == 0 || y == last_y {
            // Left corner: x = 0 -> most significant bit of byte 0.
            line[0] |= 0x80;
            // Right corner: x = width - 1.
            let last_x = case.width_px() - 1;
            let byte = (last_x / 8) as usize;
            let bit = 7 - (last_x % 8);
            line[byte] |= 1 << bit;
        }
    }

    line
}

/// Produces the 1796-byte `cups_page_header2_t` (big endian).
fn build_page_header(case: &Case) -> Vec<u8> {
    let mut buf = vec![0u8; 1796];
    let mut put = |off: usize, val: u32| {
        buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
    };

    let (img_l, img_b, img_r, img_t) = case.media.imageable_pt;

    put(276, case.resolution.0); // HWResolution[0]
    put(280, case.resolution.1); // HWResolution[1]
    put(284, img_l as u32); // ImagingBoundingBox
    put(288, img_b as u32);
    put(292, img_r as u32);
    put(296, img_t as u32);
    put(312, img_l as u32); // Margins[0] — left edge of *ImageableArea
    put(316, img_b as u32); // Margins[1]
    put(324, case.media_position);
    put(340, case.copies);
    put(352, case.media.dimension_pt.0); // PageSize[0]
    put(356, case.media.dimension_pt.1); // PageSize[1]
    put(372, case.width_px()); // cupsWidth
    put(376, case.height_lines()); // cupsHeight
    put(384, 1); // cupsBitsPerColor
    put(388, 1); // cupsBitsPerPixel
    put(392, case.bytes_per_line()); // cupsBytesPerLine
    put(396, 0); // cupsColorOrder = Chunked
    put(400, 3); // cupsColorSpace = K

    let media_type = case.media_type.as_bytes();
    buf[128..128 + media_type.len()].copy_from_slice(media_type);
    let name = case.media.ppd_key.as_bytes();
    buf[1732..1732 + name.len()].copy_from_slice(name); // cupsPageSizeName

    buf
}

/// Encodes one line with CUPS Raster v2 line RLE.
///
/// The encoding is the one `CupsLineDecoder` in `raster.rs` decodes: one
/// `repeat` byte per line (always 0 here, so the line appears once), then
/// repeat records until the line is full. With 1-bit data the bytes-per-pixel
/// count is 1, so splitting equal bytes into groups of at most 128 is enough.
fn encode_v2_line(line: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8]; // repeat = 0 -> line appears once
    let mut i = 0usize;
    while i < line.len() {
        let byte = line[i];
        let mut run = 1usize;
        while i + run < line.len() && line[i + run] == byte && run < 128 {
            run += 1;
        }
        out.push((run - 1) as u8); // n < 128 -> next pixel repeated (n+1) times
        out.push(byte);
        i += run;
    }
    out
}

/// Produces the case's complete CUPS Raster stream.
fn build_raster(case: &Case) -> Vec<u8> {
    let mut stream = match case.encoding {
        Encoding::V3 => b"RaS3".to_vec(),
        Encoding::V2Rle => b"RaS2".to_vec(),
    };

    let header = build_page_header(case);
    let height = case.height_lines();

    for _ in 0..case.pages {
        stream.extend_from_slice(&header);
        for y in 0..height {
            let line = build_line(case, y);
            match case.encoding {
                Encoding::V3 => stream.extend_from_slice(&line),
                Encoding::V2Rle => stream.extend_from_slice(&encode_v2_line(&line)),
            }
        }
    }

    stream
}

// ============================================================================
// Sidecar generation
// ============================================================================

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Writes the case's classic CUPS page header, and the QPDL placement values
/// the filter derives from it, as JSON.
///
/// The option mapping on the PAPPL side will be validated against this file:
/// the classic header is no longer produced there, so this is the only place
/// the reference is kept.
fn build_sidecar(case: &Case) -> String {
    let header_bytes = build_page_header(case);
    let header = PageHeader::parse(&header_bytes, CupsRasterVersion::V3Be)
        .expect("the generated header should parse");

    // The placement values the filter derives.
    let page_width_px =
        compute_page_width_pixels(header.page_size_points[0], header.hw_resolution[0]);
    let band_width_bytes = page_width_px.div_ceil(8);
    let hard_margin = hard_margin_bytes(case.media.imageable_pt.0, header.hw_resolution[0]);
    let placement = band_placement(
        band_width_bytes as usize,
        header.bytes_per_line as usize,
        hard_margin,
    )
    .expect("a golden case must produce a valid placement");
    let band_height = band_height_for(header.hw_resolution);
    let paper_size = SplPaperSize::from_dimensions_pt_exact(
        header.page_size_points[0],
        header.page_size_points[1],
    )
    .expect("a golden case must use a recognised paper size");
    let paper_source = SplPaperSource::from_media_position(header.media_position)
        .expect("a golden case must use a recognised tray");

    let (img_l, img_b, img_r, img_t) = case.media.imageable_pt;

    format!(
        r#"{{
  "case": "{name}",
  "note": "The classic CUPS page header, and the QPDL placement the filter derived from it. The option mapping on the PAPPL side is validated against this.",
  "input": {{
    "encoding": "{encoding}",
    "pages": {pages},
    "content": "{content}",
    "job_title": "{job_title}",
    "synthetic": {synthetic}
  }},
  "cups_page_header": {{
    "MediaType": "{media_type}",
    "cupsPageSizeName": "{page_size_name}",
    "HWResolution": [{res_x}, {res_y}],
    "PageSize": [{page_w}, {page_h}],
    "ImagingBoundingBox": [{img_l}, {img_b}, {img_r}, {img_t}],
    "Margins": [{margin_l}, {margin_b}],
    "MediaPosition": {media_position},
    "NumCopies": {num_copies},
    "Duplex": {duplex},
    "Tumble": {tumble},
    "cupsWidth": {width},
    "cupsHeight": {height},
    "cupsBytesPerLine": {bpl},
    "cupsBitsPerColor": {bpc},
    "cupsBitsPerPixel": {bpp},
    "cupsColorOrder": {color_order},
    "cupsColorSpace": {color_space},
    "cupsCompression": {compression}
  }},
  "derived_qpdl": {{
    "page_width_pixels": {page_width_px},
    "band_width_bytes": {band_width_bytes},
    "band_height_lines": {band_height},
    "hard_margin_pt": {driver_margin},
    "hard_margin_bytes": {hard_margin},
    "dst_offset_bytes": {dst_offset},
    "src_skip_bytes": {src_skip},
    "paper_size_code": {paper_code},
    "paper_source_code": {source_code},
    "copies_sent": {copies_sent}
  }},
  "ppd_source": {{
    "PageSize": "{ppd_key}",
    "PaperDimension": "{page_w} {page_h}",
    "ImageableArea": "{img_l} {img_b} {img_r} {img_t}"
  }}
}}
"#,
        name = json_escape(case.name),
        encoding = match case.encoding {
            Encoding::V3 => "RaS3",
            Encoding::V2Rle => "RaS2",
        },
        pages = case.pages,
        job_title = json_escape(case.title),
        synthetic = SYNTHETIC_MEDIA
            .iter()
            .any(|m| m.imageable_pt == case.media.imageable_pt
                && m.dimension_pt == case.media.dimension_pt),
        content = match case.content {
            Content::Blank => "blank",
            Content::RegistrationMarks => "registration-marks",
        },
        media_type = json_escape(&header.media_type),
        page_size_name = json_escape(header.page_size_name.as_deref().unwrap_or("")),
        res_x = header.hw_resolution[0],
        res_y = header.hw_resolution[1],
        page_w = header.page_size_points[0],
        page_h = header.page_size_points[1],
        img_l = img_l,
        img_b = img_b,
        img_r = img_r,
        img_t = img_t,
        margin_l = header.margins[0],
        margin_b = header.margins[1],
        media_position = header.media_position,
        num_copies = header.num_copies,
        duplex = header.duplex,
        tumble = header.tumble,
        width = header.width,
        height = header.height,
        bpl = header.bytes_per_line,
        bpc = header.bits_per_color,
        bpp = header.bits_per_pixel,
        color_order = 0,
        color_space = 3,
        compression = header.compression,
        page_width_px = page_width_px,
        band_width_bytes = band_width_bytes,
        band_height = band_height,
        hard_margin = hard_margin,
        driver_margin = case.media.imageable_pt.0,
        dst_offset = placement.dst_offset,
        src_skip = placement.src_skip,
        paper_code = paper_size as u8,
        source_code = paper_source as u8,
        copies_sent = sanitize_copies(header.num_copies),
        ppd_key = json_escape(case.media.ppd_key),
    )
}

// ============================================================================
// Running and comparing
// ============================================================================

fn goldens_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/goldens"))
}

/// Runs the case through the filter and returns the SPL stream it produced.
fn run_case(case: &Case) -> Vec<u8> {
    let args = CupsFilterArgs {
        job_id: Some("1".to_string()),
        user: Some(GOLDEN_USER.to_string()),
        title: Some(case.title.to_string()),
        num_copies: None,
        options: None,
        filename: None,
    };

    let raster = build_raster(case);
    dump_raster_if_requested(case, &raster);

    let mut out: Vec<u8> = Vec::new();
    process_with_margin(
        &args,
        Box::new(Cursor::new(raster)),
        &mut out,
        GOLDEN_SERVICE_DATE,
        case.media.imageable_pt.0,
    )
    .unwrap_or_else(|e| panic!("golden case '{}' failed to process: {}", case.name, e));
    out
}

fn update_requested() -> bool {
    std::env::var_os("UPDATE_GOLDENS").is_some()
}

/// If `DUMP_GOLDEN_RASTER=<dir>` is set, writes the case's GENERATED raster
/// input into that directory.
///
/// It serves two purposes: (1) the goldens are committed but the inputs are
/// not, so this is how an input is recovered when drift has to be inspected by
/// hand; (2) this harness calls the filter IN PROCESS, whereas CUPS runs the
/// installed binary — the dumped input makes it possible to verify that the
/// two produce the same bytes:
///
/// ```text
/// DUMP_GOLDEN_RASTER=/tmp/r cargo test golden
/// ./target/release/rastertospl-rust 1 tester golden 1 '' /tmp/r/a4-600-marks.raster > /tmp/out.spl
/// ```
///
/// (The binary's output differs only on the `SERVICEDATE` line; that line is
/// pinned in the goldens and taken from the clock in the binary.)
fn dump_raster_if_requested(case: &Case, raster: &[u8]) {
    let Some(dir) = std::env::var_os("DUMP_GOLDEN_RASTER") else {
        return;
    };
    let dir = PathBuf::from(dir);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = fs::write(dir.join(format!("{}.raster", case.name)), raster);
}

/// Compares the produced content against the golden; refreshes the file if
/// `UPDATE_GOLDENS` is set.
fn compare_or_update(path: PathBuf, produced: &[u8], case_name: &str) {
    if update_requested() {
        fs::create_dir_all(goldens_dir()).expect("could not create goldens/");
        fs::write(&path, produced)
            .unwrap_or_else(|e| panic!("could not write {}: {}", path.display(), e));
        return;
    }

    let expected = fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "could not read golden file: {} ({}). To produce it the first time: UPDATE_GOLDENS=1 cargo test golden",
            path.display(),
            e
        )
    });

    if expected == produced {
        return;
    }

    // Say WHERE the difference starts: comparing two 28 KB streams by eye is
    // not possible, but the first differing byte usually points straight at
    // the stage (PJL header, page header, band record) that changed.
    let first_diff = expected
        .iter()
        .zip(produced.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| expected.len().min(produced.len()));

    let ctx = |data: &[u8]| {
        let start = first_diff.saturating_sub(8);
        let end = (first_diff + 8).min(data.len());
        data[start..end]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ")
    };

    panic!(
        "GOLDEN FILE DRIFT — case '{}'\n\
         file       : {}\n\
         expected   : {} bytes\n\
         produced   : {} bytes\n\
         first diff : offset {}\n\
         expected   : {}\n\
         produced   : {}\n\
         \n\
         The bytes going to the printer changed. If this is a DELIBERATE\n\
         behaviour change, refresh with `UPDATE_GOLDENS=1 cargo test golden`\n\
         and include the diff in review; otherwise it is a regression.",
        case_name,
        path.display(),
        expected.len(),
        produced.len(),
        first_diff,
        ctx(&expected),
        ctx(produced),
    );
}

// ============================================================================
// Tests
// ============================================================================

/// For every case in the corpus, the produced SPL stream and sidecar must be
/// BYTE-FOR-BYTE identical to the committed goldens.
#[test]
fn test_goldens_match() {
    let dir = goldens_dir();
    for case in CASES {
        let produced = run_case(case);
        compare_or_update(dir.join(format!("{}.spl", case.name)), &produced, case.name);

        let sidecar = build_sidecar(case);
        compare_or_update(
            dir.join(format!("{}.json", case.name)),
            sidecar.as_bytes(),
            case.name,
        );
    }
}

/// The same page, produced from a line-RLE (`RaS2`) and from an uncompressed
/// (`RaS3`) input, must give BYTE-FOR-BYTE identical SPL streams.
///
/// This matters specifically for the PAPPL migration: PWG Raster's body
/// encoding is the same as `RaS2`, so if this equality breaks there is drift
/// on the decoder side.
#[test]
fn test_golden_v2_and_v3_agree() {
    let v3 = CASES
        .iter()
        .find(|c| c.name == "a4-600-marks")
        .expect("the a4-600-marks case must be in the corpus");
    let v2 = CASES
        .iter()
        .find(|c| c.name == "a4-600-marks-v2rle")
        .expect("the a4-600-marks-v2rle case must be in the corpus");

    assert_eq!(
        run_case(v3),
        run_case(v2),
        "RaS2 and RaS3 inputs must produce the same SPL stream"
    );
}

/// `px_from_pt` must reproduce real `cups-filters` output.
///
/// The measurements come from the table in
/// `test_validate_page_header_accepts_real_cupsfilter_heights`; those
/// `cupsHeight` values come from real runs. The rounding rule (`round`, not
/// `ceil`/`floor`) is what keeps the corpus geometry realistic.
#[test]
fn test_golden_geometry_matches_measured_cupsfilter_output() {
    // (imageable height pt, y_dpi, measured cupsHeight)
    let measured = [
        (818u32, 300u32, 3408u32), // A4   (830 - 12)
        (818, 600, 6817),          // A4
        (818, 1200, 13633),        // A4
        (768, 600, 6400),          // Letter (780 - 12)
        (984, 600, 8200),          // Legal  (996 - 12)
        (984, 1200, 16400),        // Legal
        (396, 600, 3300),          // A6     (408 - 12)
        (911, 1200, 15183),        // Folio  (923 - 12)
    ];
    for (imageable_pt, dpi, expected) in measured {
        assert_eq!(
            px_from_pt(f64::from(imageable_pt), dpi),
            expected,
            "{} pt @ {} DPI",
            imageable_pt,
            dpi
        );
    }
}

/// The corpus's media definitions must agree with the PPD itself.
///
/// The goldens' geometry is derived from the PPD; if the PPD changed and this
/// table did not, the goldens would silently keep freezing an unrealistic
/// geometry.
#[test]
fn test_golden_media_matches_ppd() {
    let ppd = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/ppd/samsung-ml2160.ppd"
    ))
    .expect("could not read the PPD");

    for media in PPD_MEDIA.iter().copied() {
        assert_eq!(media.imageable_pt.0, crate::media::HARD_MARGIN_PT);
        let dim_line = format!("*PaperDimension {}/", media.ppd_key);
        let dim = ppd
            .lines()
            .find(|l| l.starts_with(&dim_line))
            .unwrap_or_else(|| panic!("no *PaperDimension {} in the PPD", media.ppd_key));
        let dim_values: Vec<u32> = dim
            .split('"')
            .nth(1)
            .expect("*PaperDimension value must be quoted")
            .split_whitespace()
            .map(|v| v.parse().expect("*PaperDimension must be numeric"))
            .collect();
        assert_eq!(
            dim_values,
            vec![media.dimension_pt.0, media.dimension_pt.1],
            "{} *PaperDimension does not match",
            media.ppd_key
        );

        let area_line = format!("*ImageableArea {}/", media.ppd_key);
        let area = ppd
            .lines()
            .find(|l| l.starts_with(&area_line))
            .unwrap_or_else(|| panic!("no *ImageableArea {} in the PPD", media.ppd_key));
        let area_values: Vec<f64> = area
            .split('"')
            .nth(1)
            .expect("*ImageableArea value must be quoted")
            .split_whitespace()
            .map(|v| v.parse().expect("*ImageableArea must be numeric"))
            .collect();
        let (l, b, r, t) = media.imageable_pt;
        assert_eq!(
            area_values,
            vec![l, b, r, t],
            "{} *ImageableArea does not match",
            media.ppd_key
        );
    }
}

/// The corpus must actually provide the coverage that was asked for: a
/// registration-mark case for A4 and Letter at EVERY SUPPORTED RESOLUTION.
///
/// This test audits the corpus itself; it breaks if a case is deleted by
/// accident or a new resolution is added to the PPD.
#[test]
fn test_golden_corpus_covers_marks_on_a4_and_letter_at_every_resolution() {
    let resolutions = [(300, 300), (600, 600), (1200, 600), (1200, 1200)];
    for media in [A4, LETTER] {
        for res in resolutions {
            let found = CASES.iter().any(|c| {
                c.media.ppd_key == media.ppd_key
                    && c.resolution == res
                    && c.content == Content::RegistrationMarks
                    && c.encoding == Encoding::V3
            });
            assert!(
                found,
                "no registration-mark case for {} @ {}x{} in the corpus",
                media.ppd_key, res.0, res.1
            );
        }
    }
}

/// The pinned service date must actually take effect: the golden stream must
/// carry `GOLDEN_SERVICE_DATE`, NOT today's date.
///
/// Otherwise the goldens would break at midnight, creating pressure to loosen
/// the comparison — which would hide real drift as well.
#[test]
fn test_golden_service_date_is_pinned() {
    let case = CASES
        .iter()
        .find(|c| c.name == "a4-600-blank")
        .expect("the a4-600-blank case must be in the corpus");
    let out = run_case(case);

    let expected = format!("@PJL DEFAULT SERVICEDATE={}\n", GOLDEN_SERVICE_DATE);
    assert!(
        out.windows(expected.len())
            .any(|w| w == expected.as_bytes()),
        "the golden stream must carry the pinned service date"
    );

    let today = format!("@PJL DEFAULT SERVICEDATE={}\n", current_service_date());
    if today != expected {
        assert!(
            !out.windows(today.len()).any(|w| w == today.as_bytes()),
            "the golden stream must not carry today's date"
        );
    }
}

/// Every medium the PPD declares must have at least one golden case.
///
/// The hard-margin table is per-size, so a size with no golden is a margin
/// with no golden — which is the R-1 failure mode one size at a time.
#[test]
fn test_golden_corpus_covers_every_ppd_medium() {
    for media in PPD_MEDIA.iter().copied() {
        let found = CASES.iter().any(|c| {
            c.media.ppd_key == media.ppd_key && c.media.imageable_pt == media.imageable_pt
        });
        assert!(found, "no golden case for PPD medium {}", media.ppd_key);
    }
}

/// The two geometric extremes must be covered at every supported resolution.
///
/// Legal is the tallest sheet and Folio the second tallest, so they produce
/// the largest line counts, the most bands, and the widest hard margins.
#[test]
fn test_golden_corpus_covers_the_extremes_at_every_resolution() {
    for media in [LEGAL, FOLIO] {
        for res in PPD_RESOLUTIONS.iter().copied() {
            let found = CASES
                .iter()
                .any(|c| c.media.ppd_key == media.ppd_key && c.resolution == res);
            assert!(
                found,
                "no golden case for {} @ {}x{}",
                media.ppd_key, res.0, res.1
            );
        }
    }
}

/// The synthetic media must fabricate only the imageable area, never the
/// sheet.
///
/// `validate_page_header` rejects any `PageSize` that is not an exact known
/// QPDL paper size, so a fully invented sheet would be rejected by the filter
/// rather than exercising the placement branch we are trying to freeze.
#[test]
fn test_golden_synthetic_media_use_a_real_sheet() {
    for synth in SYNTHETIC_MEDIA.iter().copied() {
        let sheet_is_real = PPD_MEDIA
            .iter()
            .any(|m| m.dimension_pt == synth.dimension_pt);
        assert!(
            sheet_is_real,
            "synthetic medium {:?} does not use a real PPD sheet size",
            synth.dimension_pt
        );
        assert!(
            SplPaperSize::from_dimensions_pt_exact(synth.dimension_pt.0, synth.dimension_pt.1)
                .is_some(),
            "synthetic medium {:?} is not a recognised QPDL paper size",
            synth.dimension_pt
        );
    }
}

/// The `dst_offset > 0` branch of `band_placement` must be covered by at
/// least two cases with DISTINCT offsets.
///
/// One case would prove the branch runs; two different values prove the
/// offset is actually derived rather than constant.
#[test]
fn test_golden_corpus_covers_positive_dst_offset() {
    let mut offsets: Vec<usize> = Vec::new();
    for case in CASES {
        let header_bytes = build_page_header(case);
        let header = PageHeader::parse(&header_bytes, CupsRasterVersion::V3Be)
            .expect("the generated header should parse");
        let band_width_bytes =
            compute_page_width_pixels(header.page_size_points[0], header.hw_resolution[0])
                .div_ceil(8);
        let placement = band_placement(
            band_width_bytes as usize,
            header.bytes_per_line as usize,
            hard_margin_bytes(case.media.imageable_pt.0, header.hw_resolution[0]),
        )
        .expect("a golden case must produce a valid placement");
        if placement.dst_offset > 0 {
            offsets.push(placement.dst_offset);
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    assert!(
        offsets.len() >= 2,
        "the corpus must cover dst_offset > 0 with at least two distinct values, found {:?}",
        offsets
    );
}

/// The QPDL band-order field is 8 bits wide, so a page may carry at most 256
/// bands. Prove the ceiling across the WHOLE matrix, not one sampled case.
///
/// For every PPD medium at every supported resolution, this computes the band
/// count the filter would send — the imageable line count divided by the band
/// height `band_height_for` chooses — and asserts it stays inside the field.
/// The worst case is Legal at 1200x1200: 16400 lines in 128-line bands, so
/// 129 bands, which is roughly half the ceiling.
///
/// The 64-line band height never combines with a high line count, which is
/// what keeps the margin so wide: it is selected only when BOTH axes are 300
/// dpi, and at 300 dpi the same sheet produces a quarter of the lines.
///
/// A header outside this matrix cannot reach the ceiling either: the
/// compile-time assertion next to `band_height_for` bounds it from the
/// validator's own limits, and `write_compressed_band` fails the job with a
/// specific error rather than wrapping the band index.
#[test]
fn test_band_count_stays_inside_the_qpdl_band_order_field() {
    const CEILING: u32 = u8::MAX as u32 + 1;
    let mut worst = (0u32, "", (0u32, 0u32));

    for media in PPD_MEDIA.iter().copied() {
        for res in PPD_RESOLUTIONS.iter().copied() {
            let case = Case::new("matrix-probe", media, res);
            let header_bytes = build_page_header(&case);
            let header = PageHeader::parse(&header_bytes, CupsRasterVersion::V3Be)
                .expect("the generated header should parse");
            let band_height = band_height_for(header.hw_resolution) as u32;
            let bands = header.height.div_ceil(band_height);

            assert!(
                bands <= CEILING,
                "{} @ {}x{}: {} lines / {} = {} bands, over the {}-band ceiling",
                media.ppd_key,
                res.0,
                res.1,
                header.height,
                band_height,
                bands,
                CEILING
            );
            if bands > worst.0 {
                worst = (bands, media.ppd_key, res);
            }
        }
    }

    assert_eq!(
        worst,
        (128, "Legal", (1200, 1200)),
        "the worst case in the matrix moved; re-check the ceiling argument"
    );
}
