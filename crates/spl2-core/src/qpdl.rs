//! # Samsung Printer Language (SPL2 / QPDL v3) protocol module
//!
//! Fully compatible PJL job control, the 17-byte page header, and Algo 0x11 RLE
//! band encoding (with the 0x09ABCDEF sub-header and a checksum) for the Samsung
//! ML-2160 series and compatible monochrome QPDL/SPL2 laser printers.
//!
//! Source: OpenPrinting SpliX (QPDL v3 / ML-2160 series)
//! Licence: GPLv2 (v2 only — same as the SpliX source)

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// PJL Universal Exit Language (UEL)
pub const PJL_UEL: &[u8] = b"\x1b%-12345X";
pub const PJL_END: &[u8] = b"\t\x1b%-12345X";

/// Sub-header signature (0x09ABCDEF - little endian)
pub const SUBHEADER_SIG_LE: [u8; 4] = [0xEF, 0xCD, 0xAB, 0x09];

/// Algo 0x11 RLE compression constants
pub const COMPRESS_SAMPLE_RATE: usize = 0x800; // 2048 bytes
pub const TABLE_PTR_SIZE: usize = 0x40; // 64 pointers/offsets
pub const MAX_UNCOMPRESSED_BYTES: usize = 0x80; // 128 bytes
pub const MIN_COMPRESSED_BYTES: usize = 2; // > 2 bytes (at least a 3-byte match)
pub const MAX_COMPRESSED_BYTES: usize = 0x1FF + 3; // 514 bytes
pub const COMPRESSION_FLAG: u8 = 0x80;

/// Samsung QPDL paper-size definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplPaperSize {
    Letter = 0,
    Legal = 1,
    #[default]
    A4 = 2,
    Executive = 3,
    Ledger = 4,
    A3 = 5,
    Env10 = 6,
    Monarch = 7,
    C5 = 8,
    Dl = 9,
    B4 = 10,
    B5 = 11,
    EnvIsoB5 = 12,
    Postcard = 14,
    A5 = 16,
    A6 = 17,
    B6 = 18,
    Custom = 21,
    C6 = 23,
    Folio = 24,
    EnvPersonal = 25,
    Env9 = 26,
    Oficio = 28,
}

impl SplPaperSize {
    /// Maps a physical size to a RECOGNISED QPDL paper code; returns `None`
    /// for any size outside the table.
    ///
    /// An unknown size is not coerced to some other paper code: a divergence
    /// between the paper code in the QPDL page header and the pixel geometry
    /// actually sent can cause alignment and feed errors on the printer side.
    /// The caller must handle a `None` result by rejecting the job.
    pub fn from_dimensions_pt_exact(width_pt: u32, height_pt: u32) -> Option<Self> {
        Some(match (width_pt, height_pt) {
            (595, 842) | (842, 595) => SplPaperSize::A4,
            (612, 792) | (792, 612) => SplPaperSize::Letter,
            (612, 1008) | (1008, 612) => SplPaperSize::Legal,
            (420, 595) | (595, 420) => SplPaperSize::A5,
            (297, 420) | (420, 297) => SplPaperSize::A6,
            (522, 756) | (756, 522) => SplPaperSize::Executive,
            // Folio (F4) = 210x330 mm = 595.28 x 935.43 pt -> 595 x 935.
            // This used to be 612 x 936, which is not Folio but 8.5x13 inch,
            // i.e. FanFoldGermanLegal in Adobe's naming — `cupstestppd` warned
            // about the PPD for exactly this reason ("Size "Folio" should be
            // the Adobe standard name "FanFoldGermanLegal""). Upstream SpliX's
            // PPDs for the same engine family (ml1910.ppd, ml2010.ppd,
            // ml2525.ppd, ml1640.ppd, ml2510.ppd) also say
            // `*PaperDimension Folio: "595 935"`. NOTE: there is NO
            // ml2160.ppd/ml2165.ppd file in SpliX; this series is not in the
            // upstream PPD list, so sibling models speaking the same QPDL v3
            // protocol are used as the reference.
            (595, 935) | (935, 595) => SplPaperSize::Folio,
            (516, 729) | (729, 516) => SplPaperSize::B5,
            (297, 684) | (684, 297) => SplPaperSize::Env10,
            (312, 624) | (624, 312) => SplPaperSize::Dl,
            (459, 649) | (649, 459) => SplPaperSize::C5,
            _ => return None,
        })
    }
}

/// Paper source (tray)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplPaperSource {
    #[default]
    Auto = 1,
    Manual = 2,
    Multi = 3,
    Upper = 4,
    Lower = 5,
}

impl SplPaperSource {
    /// Maps the `MediaPosition` field in the CUPS Raster header to a QPDL
    /// paper-source code; returns `None` for any unrecognised value.
    ///
    /// The mapping is one-to-one, because this project's PPD numbers the
    /// `*InputSlot` options directly with the QPDL codes (`<</MediaPosition 1>>`
    /// = Auto, `<</MediaPosition 2>>` = Manual). Upstream SpliX's ml1910.ppd /
    /// ml2010.ppd / ml2525.ppd / ml1640.ppd / ml2510.ppd use the same
    /// numbering. So as not to leave the link as a mere comment,
    /// `test_every_ppd_input_slot_maps_to_its_qpdl_code` parses the PPD and
    /// verifies that every option maps to the expected code.
    ///
    /// `0` means "no source selected" (a raster produced without a PPD, e.g.
    /// without `cupsfilter -p`) and falls back silently to `Auto` — this is not
    /// a deviation but the absence of the field. OTHER unrecognised values
    /// return `None` so the caller can warn.
    pub fn from_media_position(media_position: u32) -> Option<Self> {
        Some(match media_position {
            0 | 1 => SplPaperSource::Auto,
            2 => SplPaperSource::Manual,
            3 => SplPaperSource::Multi,
            4 => SplPaperSource::Upper,
            5 => SplPaperSource::Lower,
            _ => return None,
        })
    }
}

/// Resolution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplResolution {
    Dpi300,
    #[default]
    Dpi600,
    Dpi1200,
}

impl SplResolution {
    #[inline]
    pub fn dpi(&self) -> u32 {
        match self {
            SplResolution::Dpi300 => 300,
            SplResolution::Dpi600 => 600,
            SplResolution::Dpi1200 => 1200,
        }
    }

    /// Converts a DPI value only if it is one of the exact values QPDL
    /// supports in this driver. There is no rounding to a nearby value;
    /// otherwise the raster geometry could be computed with, say, 599 DPI while
    /// the QPDL header says 300 DPI.
    pub fn from_dpi_exact(dpi: u32) -> Option<Self> {
        match dpi {
            300 => Some(SplResolution::Dpi300),
            600 => Some(SplResolution::Dpi600),
            1200 => Some(SplResolution::Dpi1200),
            _ => None,
        }
    }

    /// Checks the resolution pairs offered by the PPD and by the QPDL modes
    /// validated for the ML-2160 family. Validating the axes separately is not
    /// enough: e.g. 600x1200 is made of two recognised axes but is not an
    /// offered printer mode.
    pub fn pair_is_supported(x_dpi: u32, y_dpi: u32) -> bool {
        matches!(
            (x_dpi, y_dpi),
            (300, 300) | (600, 600) | (1200, 600) | (1200, 1200)
        )
    }
}

/// Duplex mode
///
/// SpliX `request.cpp` maps the PPD's `Duplex` option to AUTOMATIC or MANUAL
/// duplex by looking at the PPD's `*QPDL ManualDuplex` attribute:
///
/// ```c
/// manualDuplex = ppd->get("ManualDuplex", "QPDL").isTrue();
/// if (value == "DuplexNoTumble") _duplex = manualDuplex ? ManualLongEdge : LongEdge;
/// else if (value == "DuplexTumble") _duplex = manualDuplex ? ManualShortEdge : ShortEdge;
/// else _duplex = Simplex;
/// ```
///
/// The ML-2160 series has NO automatic-duplex hardware: both upstream SpliX's
/// sibling-model PPDs (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`,
/// `ml1640.ppd`, `ml2510.ppd`) and this project's PPD say
/// `*QPDL ManualDuplex: "On"`. So in this printer family only the `Simplex`
/// and `Manual*` variants are actually triggered; `LongEdge` and `ShortEdge`
/// are kept in the model for automatic-duplex models speaking the same QPDL v3
/// protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplDuplex {
    #[default]
    Simplex,
    LongEdge,
    ShortEdge,
    ManualLongEdge,
    ManualShortEdge,
}

/// Band compression algorithm
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplCompression {
    #[default]
    None = 0x00,
    Rle = 0x11,
}

/// The vocabulary the printer recognises for `@PJL SET PAPERTYPE`.
///
/// The source is the `*MediaType` option keys in upstream SpliX's PPDs for the
/// same engine family (ml1910.ppd, ml2010.ppd, ml2525.ppd, ml1640.ppd,
/// ml2510.ppd — all five give the exact same list). SpliX reads this key from
/// the PPD and writes it verbatim into the `@PJL SET PAPERTYPE=%s` line
/// (printer.cpp sendPJLHeader), so the key itself is the protocol value; it is
/// not a human-readable label.
///
/// `OFF` = "use the printer's own default" and is the first member of the list;
/// `PJL_PAPERTYPE_DEFAULT` points to it.
pub const PJL_PAPER_TYPES: [&str; 14] = [
    "OFF", "NORMAL", "THICK", "THIN", "BOND", "OHP", "CARD", "LABEL", "USED", "COLOR", "ENV",
    "COTTON", "RECYCLED", "ARCHIVE",
];

/// The PJL value used when the paper type is unknown or unrecognised.
pub const PJL_PAPERTYPE_DEFAULT: &str = PJL_PAPER_TYPES[0];

/// Maps a free-text paper-type name to a PJL value the printer recognises.
///
/// The value comes from the `MediaType` field in the CUPS Raster header, i.e.
/// the client that submitted the job, and is UNTRUSTED; so the text is not
/// written straight into the PJL line, only mapped through the table. Because
/// the return type is `&'static str`, only one of the constants above can enter
/// the PJL line: an arbitrary string (space, CR/LF, ESC) leaking into the line
/// is impossible at the type level. This distinction matters because the
/// `PAPERTYPE` value is not carried in quotes like `JOBNAME`/`USERNAME` — there
/// `sanitize_pjl_field` suffices, whereas here a single space would corrupt the
/// line.
///
/// The comparison is ASCII case-insensitive: the PPD keys are upper-case, but a
/// hand-edited PPD writing `env` should not cause a silent fallback.
pub fn pjl_paper_type(name: &str) -> Option<&'static str> {
    let name = name.trim();
    PJL_PAPER_TYPES
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .copied()
}

/// Converts a whole number of days since the Unix epoch into a Gregorian
/// calendar date. The algorithm uses integer arithmetic only, so the filter
/// carries no clock/date dependency to produce the current service date.
fn service_date_from_unix_days(days_since_epoch: i64) -> String {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };

    format!("{year:04}{month:02}{day:02}")
}

/// Returns the current UTC date in Samsung PJL's `YYYYMMDD` service-date
/// format. If the system clock is before the Unix epoch, a safe and valid base
/// date is used.
pub fn current_service_date() -> String {
    let days_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    service_date_from_unix_days(days_since_epoch)
}

/// Job configuration
#[derive(Debug, Clone)]
pub struct JobConfig {
    pub job_name: String,
    pub user_name: String,
    pub service_date: String,
    pub duplex: SplDuplex,
    /// The `@PJL SET PAPERTYPE` value.
    ///
    /// `&'static str`, not `String`: see `pjl_paper_type`. Because the field can
    /// only carry one of the constants in the `PJL_PAPER_TYPES` table, an
    /// untrusted paper-type name cannot leak into the PJL line.
    pub paper_type: &'static str,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            job_name: "CUPS Document".to_string(),
            user_name: "guest".to_string(),
            service_date: current_service_date(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        }
    }
}

/// Page configuration
#[derive(Debug, Clone)]
pub struct PageConfig {
    pub paper_size: SplPaperSize,
    pub paper_source: SplPaperSource,
    /// Horizontal (X) resolution — `header[0x10]` in the QPDL page header.
    ///
    /// SpliX qpdl.cpp renderPage:
    ///   header[0x1]  = page->yResolution() / 100;
    ///   header[0x10] = page->xResolution() / 100;
    ///
    /// The two axes are SEPARATE fields and their order is counter-intuitive (Y
    /// first, then X). This used to be a single `resolution` field with the
    /// HORIZONTAL resolution written into both bytes; in symmetric modes
    /// (300/600/1200) it went unnoticed, but in a real QPDL mode like
    /// `1200x600dpi` the vertical resolution was reported to the printer as
    /// 1200 instead of 600 and the page was squashed 2x vertically.
    pub resolution_x: SplResolution,
    /// Vertical (Y) resolution — `header[0x1]` in the QPDL page header.
    pub resolution_y: SplResolution,
    pub duplex: SplDuplex,
    /// This page's 1-BASED index within the job.
    ///
    /// Needed only to produce the `tumble` byte (`header[0xC]`): SpliX computes
    /// it as `page->pageNr() % 2`, and `pageNr` starts at 1 (document.cpp
    /// `_currentPage = 1`). Being 1-based is part of the contract — a 0-based
    /// counter inverts the parity and makes pages land on the wrong side in
    /// manual duplex printing.
    pub page_number: u32,
    pub copies: u16,
    /// The width field in the QPDL page header is 16-bit; so is the type.
    ///
    /// These fields used to be `u32`, and `begin_page` truncated them silently
    /// with `>> 8` / `& 0xFF`: a value not fitting 16 bits would make the size
    /// reported to the printer disagree with the actual payload (a DMA/RLE
    /// decode desync). Width had an explicit check, height did not, and height
    /// was only protected INDIRECTLY by the `MAX_LINES` limit in
    /// `validate_page_header`. Making the fields `u16` forces the caller to
    /// handle the conversion (`try_into`) explicitly and makes truncation
    /// impossible at the type level.
    pub width_pixels: u16,
    pub height_pixels: u16,
    pub qpdl_version: u8,
}

impl Default for PageConfig {
    fn default() -> Self {
        Self {
            paper_size: SplPaperSize::A4,
            paper_source: SplPaperSource::Auto,
            resolution_x: SplResolution::Dpi600,
            resolution_y: SplResolution::Dpi600,
            duplex: SplDuplex::Simplex,
            page_number: 1,
            copies: 1,
            width_pixels: 4758,
            height_pixels: 6817,
            qpdl_version: 3,
        }
    }
}

// ============================================================================
// Algo 0x11 RLE compressor (SpliX / Samsung QPDL compatible)
// ============================================================================

pub struct Algo0x11;

impl Algo0x11 {
    pub fn lookup_best_offsets(data: &[u8]) -> [u16; TABLE_PTR_SIZE] {
        // This table holds 2048 x (u32, usize) = 32 KB. It used to be a stack
        // array: fine on the main thread's 8 MB stack, but if the code is later
        // moved to a thread (default 2 MB) it risks a silent stack overflow.
        // One allocation per band is immeasurable next to the cost of the
        // compression itself, so it was moved to the heap.
        let mut occurrences: Vec<(u32, usize)> =
            (0..COMPRESS_SAMPLE_RATE).map(|i| (0u32, i)).collect();

        // If the data is longer than the sampling interval, one sample is
        // taken every 2048 bytes; if it is shorter, the interval never fills so
        // every byte is scanned. The only difference between the two cases is
        // the scan start and step.
        let (start, step) = if data.len() >= COMPRESS_SAMPLE_RATE {
            (COMPRESS_SAMPLE_RATE, COMPRESS_SAMPLE_RATE)
        } else {
            (1, 1)
        };

        let mut i = start;
        while i < data.len() {
            let b = data[i];
            let max_j = COMPRESS_SAMPLE_RATE.min(i);
            for j in 1..max_j {
                if data[i - j] == b {
                    occurrences[j - 1].0 += 1;
                }
            }
            i += step;
        }

        occurrences.sort_unstable_by_key(|o| core::cmp::Reverse(o.0));

        // The 64 most frequent offsets. The table must contain 1 (the
        // previous byte); if it does not, the last slot is reserved for it.
        let mut table = [1u16; TABLE_PTR_SIZE];
        for (slot, &(_, offset)) in table.iter_mut().zip(occurrences.iter()) {
            *slot = (offset + 1) as u16;
        }
        if !table.contains(&1) {
            table[TABLE_PTR_SIZE - 1] = 1;
        }

        table
    }

    pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
        if data.is_empty() {
            return None;
        }

        let ptr_array = Self::lookup_best_offsets(data);
        let max_output_size = data.len() + 256;
        let mut out = Vec::with_capacity(max_output_size);

        // 1. Reserve 4 bytes for uncompressed_initial_size
        out.extend_from_slice(&[0u8; 4]);

        // 2. The table of 64 u16 offsets
        let mut max_offset: usize = 0;
        for &ptr in &ptr_array {
            out.extend_from_slice(&ptr.to_le_bytes());
            if (ptr as usize) > max_offset {
                max_offset = ptr as usize;
            }
        }

        // 3. The initial raw bytes
        let mut uncomp_size = max_offset.min(MAX_UNCOMPRESSED_BYTES).min(data.len());
        if uncomp_size == 0 {
            uncomp_size = 1.min(data.len());
        }

        let uncomp_size_u32 = uncomp_size as u32;
        out[0..4].copy_from_slice(&uncomp_size_u32.to_le_bytes());

        out.extend_from_slice(&data[..uncomp_size]);

        let mut r = uncomp_size;
        let mut raw_data_counter: usize = 0;
        let mut raw_data_counter_ptr: usize = 0;

        while r < data.len() {
            let max_comp_size = (data.len() - r).min(MAX_COMPRESSED_BYTES);

            if max_comp_size >= 3 {
                let mut best_comp_counter: usize = 0;
                let mut best_ptr: usize = 0;

                for (i, &offset) in ptr_array.iter().enumerate() {
                    let off = offset as usize;
                    if off > r {
                        continue;
                    }
                    let r_ref = r - off;

                    // If this offset CANNOT BEAT the current best match, do no
                    // comparison at all: a match length is only considered if
                    // it is GREATER than `best_comp_counter`, so if the
                    // `best_comp_counter`-th byte does not match, this candidate
                    // can extend at most that far and cannot change the result.
                    // The classic LZ shortcut; it preserves the selected match
                    // and pointer exactly, and only lowers the worst-case byte
                    // comparison count.
                    //
                    // Index safety: inside the loop `best_comp_counter` is
                    // always LESS than `max_comp_size` (the moment they are
                    // equal, `break` fires below), and since
                    // `max_comp_size <= data.len() - r`,
                    // `r + best_comp_counter < data.len()`; because
                    // `r_ref < r`, the second index is within bounds too.
                    if data[r + best_comp_counter] != data[r_ref + best_comp_counter] {
                        continue;
                    }

                    let mut counter = 0;
                    while counter < max_comp_size && data[r + counter] == data[r_ref + counter] {
                        counter += 1;
                    }
                    if counter > best_comp_counter {
                        best_comp_counter = counter;
                        best_ptr = i;
                        if counter == max_comp_size {
                            break;
                        }
                    }
                }

                if best_comp_counter >= 3 {
                    if raw_data_counter > 0 {
                        out[raw_data_counter_ptr] = (raw_data_counter - 1) as u8;
                        raw_data_counter = 0;
                    }

                    r += best_comp_counter;
                    let comp_len = (best_comp_counter - 3) as u16;

                    let byte0 = COMPRESSION_FLAG | ((comp_len & 0x7F) as u8);
                    let byte1 = (((comp_len >> 1) & 0xC0) as u8) | ((best_ptr & 0x3F) as u8);

                    out.push(byte0);
                    out.push(byte1);
                    continue;
                }
            }

            raw_data_counter += 1;
            if raw_data_counter == 1 {
                raw_data_counter_ptr = out.len();
                out.push(0);
            } else if raw_data_counter == MAX_UNCOMPRESSED_BYTES {
                out[raw_data_counter_ptr] = 0x7F;
                raw_data_counter = 0;
            }

            out.push(data[r]);
            r += 1;
        }

        if raw_data_counter > 0 {
            out[raw_data_counter_ptr] = (raw_data_counter - 1) as u8;
        }

        Some(out)
    }

    pub fn calculate_checksum(data: &[u8]) -> u32 {
        let mut sum: u32 = 0;
        for &b in data {
            sum = sum.wrapping_add(b as u32);
        }
        sum
    }

    /// Decompresses the stream `compress` produced (for tests/diagnostics).
    /// It is the inverse of the format spec (see the real SpliX algo0x11.cpp).
    ///
    /// Exposed under `golden-replay` as well as `test` for the reason Q-6
    /// gives: a `#[cfg(test)]` item is invisible to another crate's tests, and
    /// the filter's band-placement tests decompress the bands they assert on.
    #[cfg(any(test, feature = "golden-replay"))]
    pub fn decompress(data: &[u8]) -> Vec<u8> {
        let uncomp_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let mut ptr_array = [0u16; TABLE_PTR_SIZE];
        for (i, ptr) in ptr_array.iter_mut().enumerate() {
            let off = 4 + i * 2;
            *ptr = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        }
        let table_end = 4 + TABLE_PTR_SIZE * 2;
        let mut out = Vec::new();
        out.extend_from_slice(&data[table_end..table_end + uncomp_size]);

        let mut pos = table_end + uncomp_size;
        while pos < data.len() {
            let b0 = data[pos];
            if b0 & COMPRESSION_FLAG != 0 {
                let b1 = data[pos + 1];
                let comp_len = ((b0 & 0x7F) as u16) | ((((b1 >> 6) & 0x3) as u16) << 7);
                let ptr_idx = (b1 & 0x3F) as usize;
                let offset = ptr_array[ptr_idx] as usize;
                let match_len = comp_len as usize + 3;
                // The reference may overlap the bytes being produced, so the
                // source index is absolute and re-read as `out` grows.
                let ref_start = out.len() - offset;
                for k in 0..match_len {
                    let b = out[ref_start + k];
                    out.push(b);
                }
                pos += 2;
            } else {
                let count = (b0 as usize) + 1;
                pos += 1;
                out.extend_from_slice(&data[pos..pos + count]);
                pos += count;
            }
        }
        out
    }
}

// ============================================================================
// SPL2 / QPDL stream writer (SplStreamWriter)
// ============================================================================

/// The maximum number of BYTES allowed in a PJL field (JOBNAME, USERNAME, etc.).
///
/// PJL interpreters typically use small, fixed-size line buffers; this upper
/// bound stops megabytes of text being sent to the printer firmware (a possible
/// crash or buffer overflow) without cutting off a realistic document
/// title/user name.
///
/// The limit is deliberately in BYTES. It used to count characters, and because
/// `.take(128)` ran over a `char` iterator, multi-byte UTF-8 input could
/// quadruple the real limit: a 400-emoji job name produced a 531-byte PJL line.
/// Since `sanitize_pjl_field`'s output is now pure ASCII, byte and character
/// counts are equal.
const MAX_PJL_FIELD_BYTES: usize = 128;

/// Says whether a character is safe to write directly into a PJL field:
/// printable ASCII (0x20-0x7E), excluding the double quote.
///
/// A double quote could close the quoted (`"..."`) field early; since PJL has
/// no escape mechanism, it is dropped entirely rather than escaped.
#[inline]
fn is_safe_pjl_ascii(c: char) -> bool {
    matches!(c, ' '..='~') && c != '"'
}

/// Folds a non-ASCII letter to a meaning-preserving ASCII equivalent.
///
/// Because the whitelist is pure ASCII, a Turkish job name ("Öğrenci
/// Başvurusu") would otherwise become unreadable ("renci Bavurusu"). Folding
/// preserves readability without giving up any safety: the output still stays
/// entirely in 0x20-0x7E and makes no assumption about the printer firmware's
/// PJL symbol set.
///
/// The table covers Turkish fully; common Western European letters are added
/// too. Every non-ASCII character not in the list (CJK, emoji, symbols) is
/// dropped.
fn ascii_fold(c: char) -> Option<&'static str> {
    Some(match c {
        // --- Turkish ---
        'ç' => "c",
        'Ç' => "C",
        'ğ' => "g",
        'Ğ' => "G",
        'ı' => "i",
        'İ' => "I",
        'ö' => "o",
        'Ö' => "O",
        'ş' => "s",
        'Ş' => "S",
        'ü' => "u",
        'Ü' => "U",
        'â' => "a",
        'Â' => "A",
        'î' => "i",
        'Î' => "I",
        'û' => "u",
        'Û' => "U",
        // --- Common Western European letters ---
        'á' | 'à' | 'ä' | 'ã' | 'å' => "a",
        'Á' | 'À' | 'Ä' | 'Ã' | 'Å' => "A",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'í' | 'ì' | 'ï' => "i",
        'Í' | 'Ì' | 'Ï' => "I",
        'ó' | 'ò' | 'ô' | 'õ' | 'ø' => "o",
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' => "O",
        'ú' | 'ù' => "u",
        'Ú' | 'Ù' => "U",
        'ñ' => "n",
        'Ñ' => "N",
        'ý' | 'ÿ' => "y",
        'Ý' => "Y",
        'ð' => "d",
        'Ð' => "D",
        'þ' => "th",
        'Þ' => "TH",
        'æ' => "ae",
        'Æ' => "AE",
        'ß' => "ss",
        // --- Central/Eastern European (Latin Extended-A/B) ---
        'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'Ć' | 'Č' | 'Ĉ' | 'Ċ' => "C",
        'ś' | 'š' | 'ș' | 'ŝ' => "s",
        'Ś' | 'Š' | 'Ș' | 'Ŝ' => "S",
        'ź' | 'ż' | 'ž' => "z",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ł' | 'ĺ' | 'ľ' => "l",
        'Ł' | 'Ĺ' | 'Ľ' => "L",
        'ń' | 'ň' | 'ņ' => "n",
        'Ń' | 'Ň' | 'Ņ' => "N",
        'đ' | 'ď' => "d",
        'Đ' | 'Ď' => "D",
        'ť' | 'ţ' | 'ț' => "t",
        'Ť' | 'Ţ' | 'Ț' => "T",
        'ř' | 'ŕ' => "r",
        'Ř' | 'Ŕ' => "R",
        'ě' | 'ē' | 'ė' | 'ę' | 'ĕ' => "e",
        'Ě' | 'Ē' | 'Ė' | 'Ę' | 'Ĕ' => "E",
        'ā' | 'ă' | 'ą' => "a",
        'Ā' | 'Ă' | 'Ą' => "A",
        'ī' | 'ĭ' | 'į' | 'ĩ' => "i",
        'Ī' | 'Ĭ' | 'Į' | 'Ĩ' => "I",
        'ō' | 'ŏ' | 'ő' => "o",
        'Ō' | 'Ŏ' | 'Ő' => "O",
        'ū' | 'ů' | 'ű' | 'ų' | 'ũ' | 'ŭ' => "u",
        'Ū' | 'Ů' | 'Ű' | 'Ų' | 'Ũ' => "U",
        'ġ' | 'ģ' => "g",
        'Ġ' | 'Ģ' => "G",
        'ķ' => "k",
        'Ķ' => "K",
        'ŷ' => "y",
        'Ŷ' | 'Ÿ' => "Y",
        'ŵ' => "w",
        'Ŵ' => "W",
        'ĵ' => "j",
        'Ĵ' => "J",
        'ħ' | 'ĥ' => "h",
        'Ħ' | 'Ĥ' => "H",
        'œ' => "oe",
        'Œ' => "OE",
        'ŀ' => "l",
        'Ŀ' => "L",
        'ŉ' => "n",
        'ŧ' => "t",
        'Ŧ' => "T",
        'ĳ' => "ij",
        'Ĳ' => "IJ",
        // --- Typographic punctuation ---
        '\u{2018}' | '\u{2019}' => "'",
        // Curly double quotes fold to the EMPTY string, not to a straight `"`:
        // a straight quote would close the quoted PJL field early (see
        // is_safe_pjl_ascii).
        '\u{201C}' | '\u{201D}' => "",
        '\u{2013}' | '\u{2014}' => "-",
        '\u{2026}' => "...",
        '\u{00A0}' => " ",
        _ => return None,
    })
}

/// Makes the free-text fields embedded in the PJL stream (JOBNAME, USERNAME,
/// etc.) safe.
///
/// PJL is line-based and has no escape mechanism for quoted (`"..."`) strings:
/// an embedded CR/LF or ESC (the start of Universal Exit Language) byte could
/// terminate the field early and make the printer interpret the following text
/// as an entirely new, arbitrary PJL command/job. `job_name`/`user_name` come
/// from the CUPS job title (hence from the client that submitted the job) and
/// must be treated as untrusted.
///
/// The filter is not a BLACKLIST but a byte-level WHITELIST. The previous
/// `char::is_control()`-based blacklist worked at the `char` level, whereas what
/// goes to the printer is a byte sequence: any byte in the C1 range
/// (0x80-0x9F) could pass the filter as a CONTINUATION BYTE of a multi-byte
/// UTF-8 character. For example `U+02DB` is `CB 9B` on the wire, and `0x9B`
/// (C1 CSI) would reach the firmware. The whitelist makes this structurally
/// impossible: every byte of the output is in 0x20-0x7E.
///
/// The bytes critical for injection (`0x1B` ESC, `0x0A`, `0x0D`) cannot appear
/// as either a leading or continuation byte in UTF-8 anyway; the whitelist
/// covers them too and rests the guarantee on the filter itself, not on a
/// property of the UTF-8 encoding.
///
/// The length is bounded in bytes by `MAX_PJL_FIELD_BYTES`, and the bound is
/// applied during construction, accounting for sequences that can expand after
/// folding ("ß" -> "ss").
fn sanitize_pjl_field(input: &str) -> String {
    let mut out = String::with_capacity(MAX_PJL_FIELD_BYTES);

    for c in input.chars() {
        // `piece` is always pure ASCII, so len() == character count.
        let piece: &str = if is_safe_pjl_ascii(c) {
            // Push directly here so a single ASCII character needs no
            // temporary buffer to copy through.
            if out.len() + 1 > MAX_PJL_FIELD_BYTES {
                break;
            }
            out.push(c);
            continue;
        } else if let Some(folded) = ascii_fold(c) {
            folded
        } else {
            // Everything not on the whitelist (control characters, bidi
            // overrides, zero-width characters, emoji, CJK) is dropped.
            continue;
        };

        if out.len() + piece.len() > MAX_PJL_FIELD_BYTES {
            break;
        }
        out.push_str(piece);
    }

    debug_assert!(out.is_ascii(), "sanitize_pjl_field output must be ASCII");
    out
}

pub struct SplStreamWriter<W: Write> {
    writer: W,
    /// The band index within the page. It is an 8-BIT field in the QPDL record
    /// (`0x1`), but the counter is kept as `u16` here so that at band 256 it
    /// gives a clear error via `u8::try_from` instead of silently wrapping (see
    /// `write_compressed_band`).
    current_band: u16,
    /// Has `begin_job` been called but `end_job` not yet?
    ///
    /// The `Drop` impl reads this to guarantee that a half-finished job's
    /// closing UEL is written; see `impl Drop for SplStreamWriter`.
    job_active: bool,
}

impl<W: Write> SplStreamWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            current_band: 0,
            job_active: false,
        }
    }

    /// Borrows the sink this writer wraps.
    ///
    /// The CUPS filter writes straight to stdout, but PAPPL hands its driver a
    /// fresh device handle on every callback, so the printer application wraps
    /// a `Vec<u8>` and drains it here after each call. Nothing else needs it.
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn begin_job(&mut self, config: &JobConfig) -> io::Result<()> {
        // The real SpliX (printer.cpp sendPJLHeader) order: after the UEL it
        // starts directly with "@PJL DEFAULT SERVICEDATE=..."; a separate bare
        // "@PJL\n" line is NOT sent. The PowerSave/JamRecovery lines are always
        // sent too, following this project's PPD defaults (PowerSave=5,
        // JamRecovery=False).
        let mut pjl = Vec::with_capacity(256);
        pjl.extend_from_slice(PJL_UEL);
        pjl.extend_from_slice(
            format!(
                "@PJL DEFAULT SERVICEDATE={}\n",
                sanitize_pjl_field(&config.service_date)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(
            format!(
                "@PJL SET USERNAME=\"{}\"\n",
                sanitize_pjl_field(&config.user_name)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(
            format!(
                "@PJL SET JOBNAME=\"{}\"\n",
                sanitize_pjl_field(&config.job_name)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(b"@PJL DEFAULT POWERSAVE=ON\n");
        pjl.extend_from_slice(b"@PJL DEFAULT POWERSAVETIME=5\n");
        pjl.extend_from_slice(b"@PJL SET JAMRECOVERY=OFF\n");

        // The real SpliX (printer.cpp sendPJLHeader) sends DUPLEX=ON/OFF
        // according to the duplex state and (if on) BINDING=LONGEDGE/SHORTEDGE.
        // SpliX printer.cpp sendPJLHeader sends MANUAL, not `ON`, for MANUAL
        // duplex, and this distinction matters for the ML-2160 family because
        // duplex on these models is always manual (see SplDuplex).
        match config.duplex {
            SplDuplex::Simplex => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=OFF\n");
            }
            SplDuplex::LongEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=ON\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=LONGEDGE\n");
            }
            SplDuplex::ShortEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=ON\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=SHORTEDGE\n");
            }
            SplDuplex::ManualLongEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=MANUAL\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=LONGEDGE\n");
            }
            SplDuplex::ManualShortEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=MANUAL\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=SHORTEDGE\n");
            }
        }

        // SpliX printer.cpp sendPJLHeader, if the PPD has a `*MediaType`
        // option, sends its key as `@PJL SET PAPERTYPE=%s`, otherwise writes
        // `OFF`. This filter reads the option not from the PPD but from the
        // `MediaType` field in the CUPS Raster page header (see main.rs
        // `pjl_paper_type_for`); because `config.paper_type` reaches here as a
        // `&'static str`, no extra filtering is needed.
        pjl.extend_from_slice(format!("@PJL SET PAPERTYPE={}\n", config.paper_type).as_bytes());
        pjl.extend_from_slice(b"@PJL SET ALTITUDE=LOW\n");
        pjl.extend_from_slice(b"@PJL SET DENSITY=3\n");
        pjl.extend_from_slice(b"@PJL SET RET=NORMAL\n");
        pjl.extend_from_slice(b"@PJL ENTER LANGUAGE = QPDL\n");

        self.writer.write_all(&pjl)?;
        self.writer.flush()?;

        // From this point the printer is in QPDL language and the stream must
        // end with a closing UEL.
        self.job_active = true;
        Ok(())
    }

    /// Sends the Samsung QPDL 17-byte page header.
    pub fn begin_page(&mut self, config: &PageConfig) -> io::Result<()> {
        self.current_band = 0;

        let mut header = [0u8; 17];
        header[0x0] = 0x00; // page-header signature
        header[0x1] = (config.resolution_y.dpi() / 100) as u8; // VERTICAL (Y) resolution / 100
        header[0x2] = (config.copies >> 8) as u8;
        header[0x3] = (config.copies & 0xFF) as u8;
        header[0x4] = config.paper_size as u8; // A4 = 2
        header[0x5..0x7].copy_from_slice(&config.width_pixels.to_be_bytes());
        header[0x7..0x9].copy_from_slice(&config.height_pixels.to_be_bytes());
        header[0x9] = config.paper_source as u8; // Auto = 1
        header[0xA] = 0x00; // unknownByte1

        // SpliX qpdl.cpp renderPage, the duplex/tumble bytes:
        //
        //   Simplex         : duplex = 1, tumble = 0
        //   LongEdge        : duplex = 1, tumble = pageNr % 2
        //   ShortEdge       : duplex = 0, tumble = pageNr % 2
        //   ManualLongEdge  : duplex = 0, tumble = pageNr % 2
        //   ManualShortEdge : duplex = 0, tumble = pageNr % 2
        //
        // The `duplex` byte is counter-intuitive: 1 for Simplex, 0 for manual
        // duplex. This value used to be produced correctly, but `tumble` was
        // written as an unconditional 0; in SpliX it is the PARITY OF THE PAGE
        // NUMBER, and `_currentPage` starts at 1 (document.cpp:
        // `_currentPage = 1`), so it is 1 on odd pages and 0 on even ones.
        header[0xB] = match config.duplex {
            SplDuplex::Simplex | SplDuplex::LongEdge => 1,
            SplDuplex::ShortEdge | SplDuplex::ManualLongEdge | SplDuplex::ManualShortEdge => 0,
        };
        header[0xC] = match config.duplex {
            SplDuplex::Simplex => 0,
            _ => (config.page_number % 2) as u8,
        };
        header[0xD] = 0x00; // unknownByte2
        header[0xE] = config.qpdl_version; // 3
        header[0xF] = 0x01; // Colorplanes = 1 (monochrome)
        header[0x10] = (config.resolution_x.dpi() / 100) as u8; // HORIZONTAL (X) resolution / 100

        self.writer.write_all(&header)?;
        Ok(())
    }

    /// Writes a raster band with Algo 0x11 RLE compression, the sub-header and
    /// a checksum.
    ///
    /// `band_width_pixels`: the printer DMA engine's line-stride width. To avoid
    /// line skew (a zebra / staircase pattern) it MUST be the byte-aligned
    /// `bytes_per_line * 8`.
    pub fn write_compressed_band(
        &mut self,
        band_width_pixels: u16,
        band_height_lines: u16,
        raw_bitmap: &[u8],
    ) -> io::Result<()> {
        // The real SpliX (compress.cpp compressPage/_compressBandedPage) never
        // sends a raw/uncompressed (0x00) band: only one of the
        // 0x0D/0x0E/0x11/0x13/0x15 algorithms is used. In this printer family a
        // raw band is not recognised by the firmware (rejected with "INTERNAL
        // ERROR - Please use the proper driver"); so Algo 0x11 RLE is always
        // used here too. NOT `unwrap_or_default()`: when `compress` returns
        // `None` (today only when the input is empty; unreachable in practice
        // because `band_size = bw_bytes * band_height` and both are greater than
        // zero), using an empty `Vec` would produce a CORRUPT band record whose
        // header says "compressed with Algo 0x11" but whose payload is 0 bytes —
        // a silent decode error on the printer side. An error is an error: it is
        // reported upward and the stream is properly ended with a closing UEL
        // via `Drop`.
        let payload_bytes = Algo0x11::compress(raw_bitmap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the Algo 0x11 compressor produced no payload for the band (empty band data).",
            )
        })?;
        let compression_type = SplCompression::Rle;

        // Total data size: payload + 4 (sub-header sig) + 4 (checksum)
        let total_data_size = (payload_bytes.len() + 8) as u32;

        // The QPDL band-order field is 8 bits. Overflow is impossible today: at
        // the largest accepted page (Legal @1200 DPI => 16800 lines) 128-line
        // bands make 132 bands. But it was an IMPLICIT invariant that could
        // silently break if the paper table or `QPDL_BAND_HEIGHT` changed:
        // `wrapping_add` would roll the order back to 0 at band 256 and report
        // the same order number to the printer a second time. The invariant is
        // now enforced rather than implicit.
        let band_index = u8::try_from(self.current_band).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the page holds more than {} bands: the QPDL band-order field is \
                     8 bits, so a page can carry at most {} bands.",
                    u8::MAX as u16 + 1,
                    u8::MAX as u16 + 1
                ),
            )
        })?;

        // 1. Band header (record 0x0C - 11 bytes)
        let mut band_header = [0u8; 11];
        band_header[0x0] = 0x0C; // Signature 12
        band_header[0x1] = band_index;
        band_header[0x2] = (band_width_pixels >> 8) as u8;
        band_header[0x3] = (band_width_pixels & 0xFF) as u8;
        band_header[0x4] = (band_height_lines >> 8) as u8;
        band_header[0x5] = (band_height_lines & 0xFF) as u8;
        band_header[0x6] = compression_type as u8; // 0x11 (Algo 0x11 RLE)
        band_header[0x7] = (total_data_size >> 24) as u8;
        band_header[0x8] = (total_data_size >> 16) as u8;
        band_header[0x9] = (total_data_size >> 8) as u8;
        band_header[0xA] = (total_data_size & 0xFF) as u8;

        self.writer.write_all(&band_header)?;

        // 2. Sub-header signature (0x09ABCDEF LE)
        self.writer.write_all(&SUBHEADER_SIG_LE)?;

        // 3. Compressed (Algo 0x11 RLE) band data
        self.writer.write_all(&payload_bytes)?;

        // 4. Checksum (sum of sub-header + payload)
        let mut checksum = Algo0x11::calculate_checksum(&SUBHEADER_SIG_LE);
        checksum = checksum.wrapping_add(Algo0x11::calculate_checksum(&payload_bytes));

        let checksum_bytes = checksum.to_be_bytes();
        self.writer.write_all(&checksum_bytes)?;

        self.current_band += 1;
        Ok(())
    }

    /// Page end (the 3-byte QPDL page footer: [0x01, copies_msb, copies_lsb])
    pub fn end_page(&mut self, copies: u16) -> io::Result<()> {
        let footer = [
            0x01, // page-footer signature
            (copies >> 8) as u8,
            (copies & 0xFF) as u8,
        ];
        self.writer.write_all(&footer)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Job end (the closing PJL UEL)
    ///
    /// The real SpliX (printer.cpp sendPJLFooter) writes `_endPJL` verbatim and
    /// flushes immediately; it does NOT append an extra "\n".
    ///
    /// The call is idempotent: if `begin_job` was not called or the job is
    /// already closed, it writes nothing. This keeps the safety net in `Drop`
    /// (see below) from producing a second UEL on successful streams.
    pub fn end_job(&mut self) -> io::Result<()> {
        if !self.job_active {
            return Ok(());
        }
        // Clear the flag BEFORE writing: if the write fails (e.g. the backend
        // closed the pipe), `Drop` must not retry the same failed write.
        self.job_active = false;
        self.writer.write_all(PJL_END)?;
        self.writer.flush()?;
        Ok(())
    }
}

/// The safety net that ends a half-finished job with a closing UEL.
///
/// The moment `begin_job` writes `@PJL ENTER LANGUAGE = QPDL`, the printer
/// switches to QPDL. If the stream ends without a closing UEL, the printer
/// hangs in that language waiting for an incomplete band record, and the next
/// job enters this stale state too. `end_job` used to be called only when the
/// page loop finished SUCCESSFULLY; every `?` in between (a header validation
/// error, a short read, a corrupt page) left the stream half-finished.
///
/// NOTE: `std::process::exit` does NOT run the `Drop`s on the stack. For this
/// net to work, the writer must have been dropped before the error reaches
/// `main` and `exit` is called — which is why the writer lives locally in
/// `process_cups_raster_to_spl` and the error is returned with `?`.
impl<W: Write> Drop for SplStreamWriter<W> {
    fn drop(&mut self) {
        if self.job_active {
            // There is nowhere to report the error; trying to close the stream
            // is still better than not trying at all.
            let _ = self.end_job();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The QPDL band-order field is 8 bits; the 257th band must error rather
    /// than silently wrap to 0. Unreachable today (worst case 170 bands), but
    /// pinned so the invariant does not break if `MAX_POINTS`/`QPDL_BAND_HEIGHT`
    /// change.
    #[test]
    fn test_band_index_beyond_255_is_rejected_not_wrapped() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = SplStreamWriter::new(&mut out);
        let band = vec![0u8; 8];
        for i in 0..=255u16 {
            w.write_compressed_band(64, 1, &band)
                .unwrap_or_else(|e| panic!("band {} rejected: {}", i, e));
        }
        let err = w
            .write_compressed_band(64, 1, &band)
            .expect_err("the 257th band must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("8 bits"), "{}", err);
    }

    /// The counter resets per page: consecutive pages must not consume each
    /// other's band order.
    #[test]
    fn test_band_index_resets_on_each_page() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = SplStreamWriter::new(&mut out);
        let band = vec![0u8; 8];
        for _ in 0..2 {
            w.begin_page(&PageConfig::default()).unwrap();
            for _ in 0..200 {
                w.write_compressed_band(64, 1, &band).unwrap();
            }
        }
    }

    /// An unrecognised size must not fall back to A4 or any other QPDL code.
    #[test]
    fn test_unknown_paper_size_is_rejected_instead_of_falling_back_to_a4() {
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(595, 842),
            Some(SplPaperSize::A4)
        );
        assert_eq!(SplPaperSize::from_dimensions_pt_exact(612, 936), None);
    }

    /// The DPI conversion must not silently round nearby values to
    /// 300/600/1200; only the axis values the PPD offers and the validated
    /// pairs are valid.
    #[test]
    fn test_resolution_mapping_is_exact_and_pair_aware() {
        assert_eq!(
            SplResolution::from_dpi_exact(300),
            Some(SplResolution::Dpi300)
        );
        assert_eq!(
            SplResolution::from_dpi_exact(600),
            Some(SplResolution::Dpi600)
        );
        assert_eq!(
            SplResolution::from_dpi_exact(1200),
            Some(SplResolution::Dpi1200)
        );
        for unsupported in [0, 299, 599, 601, 1199, 1201] {
            assert_eq!(SplResolution::from_dpi_exact(unsupported), None);
        }

        assert!(SplResolution::pair_is_supported(300, 300));
        assert!(SplResolution::pair_is_supported(600, 600));
        assert!(SplResolution::pair_is_supported(1200, 600));
        assert!(SplResolution::pair_is_supported(1200, 1200));
        assert!(!SplResolution::pair_is_supported(600, 1200));
        assert!(!SplResolution::pair_is_supported(300, 600));
    }

    #[test]
    fn test_service_date_conversion_handles_epoch_and_leap_day() {
        assert_eq!(service_date_from_unix_days(-1), "19691231");
        assert_eq!(service_date_from_unix_days(0), "19700101");
        assert_eq!(service_date_from_unix_days(11_016), "20000229");
    }

    /// If the compressor produces no payload, the band record must not be
    /// written with an EMPTY payload.
    #[test]
    fn test_empty_band_data_is_an_error_not_an_empty_record() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            let err = w
                .write_compressed_band(64, 1, &[])
                .expect_err("empty band data must error");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
        // Since `begin_job` was not called, `Drop` must not write anything either.
        assert!(out.is_empty(), "no bytes must be written on error");
    }

    #[test]
    fn test_sanitize_pjl_field_strips_injection_chars() {
        assert_eq!(sanitize_pjl_field("Normal Title"), "Normal Title");
        // Double quote: could close the quoted field early.
        assert_eq!(sanitize_pjl_field("a\"b"), "ab");
        // CR/LF: could start a new PJL command line.
        assert_eq!(sanitize_pjl_field("a\r\nb"), "ab");
        // ESC: Universal Exit Language (ends the @PJL envelope early).
        assert_eq!(sanitize_pjl_field("a\x1bb"), "ab");
        // A full injection attempt: tries to add a new @PJL command.
        let injected = sanitize_pjl_field("x\"\n@PJL DEFAULT PASSWORD=1234\n");
        assert!(!injected.contains('"'));
        assert!(!injected.contains('\n'));
        assert!(!injected.contains('\r'));
    }

    #[test]
    fn test_sanitize_pjl_field_strips_unicode_line_separators() {
        // U+2028/U+2029: is_control() does not catch these, but some text
        // handlers may treat them as line breaks.
        assert_eq!(sanitize_pjl_field("a\u{2028}b"), "ab");
        assert_eq!(sanitize_pjl_field("a\u{2029}b"), "ab");
    }

    #[test]
    fn test_sanitize_pjl_field_strips_bidi_override_spoofing() {
        // A filename-spoofing technique using U+202E (RLO) to make something
        // like "evil.exe" appear different must be prevented from showing a
        // different user/job name in PJL fields.
        let spoofed = sanitize_pjl_field("Invoice\u{202E}cod.exe");
        assert!(!spoofed.contains('\u{202E}'));
        assert_eq!(spoofed, "Invoicecod.exe");

        // Other bidi embedding/isolation and zero-width characters too.
        for c in ['\u{202A}', '\u{2066}', '\u{200B}', '\u{FEFF}'] {
            let s = format!("a{}b", c);
            assert_eq!(
                sanitize_pjl_field(&s),
                "ab",
                "char {:?} was not filtered",
                c
            );
        }
    }

    /// Because the whitelist is pure ASCII, Turkish text is preserved by
    /// folding; letters are not silently dropped (no "renci Bavurusu").
    #[test]
    fn test_sanitize_pjl_field_folds_turkish_to_ascii() {
        assert_eq!(sanitize_pjl_field("Öğrenci Başvurusu"), "Ogrenci Basvurusu");
        assert_eq!(sanitize_pjl_field("Çiğdem ÜNLÜ"), "Cigdem UNLU");
        assert_eq!(sanitize_pjl_field("ışık İSTANBUL"), "isik ISTANBUL");
    }

    /// Non-ASCII characters not in the fold table are dropped; the legitimate
    /// text around them is preserved.
    #[test]
    fn test_sanitize_pjl_field_drops_unmappable_non_ascii() {
        assert_eq!(sanitize_pjl_field("Rapor \u{1F600} 2026"), "Rapor  2026");
        assert_eq!(sanitize_pjl_field("会議 notes"), " notes");
    }

    /// The fold table must cover ALL letters in the Latin-1 Supplement.
    ///
    /// The whitelist guarantees safety independently of the table (an uncovered
    /// character is dropped, not leaked); the table's coverage only determines
    /// READABILITY. This test pins that no letter in the Western European
    /// alphabet is silently dropped.
    #[test]
    fn test_ascii_fold_covers_all_latin1_letters() {
        let mut missing = Vec::new();
        for cp in 0x00C0u32..=0x00FF {
            let c = char::from_u32(cp).unwrap();
            if c == '\u{00D7}' || c == '\u{00F7}' {
                continue; // × and ÷ are not letters, but multiply/divide signs
            }
            match ascii_fold(c) {
                Some(folded) if !folded.is_empty() && folded.is_ascii() => {}
                _ => missing.push(format!("U+{:04X} ({})", cp, c)),
            }
        }
        assert!(
            missing.is_empty(),
            "unfoldable Latin-1 letters: {:?}",
            missing
        );
    }

    /// EVERY output of the fold table must be printable ASCII and contain no
    /// character that would corrupt the field.
    #[test]
    fn test_ascii_fold_outputs_are_always_safe() {
        for cp in 0u32..0x2500 {
            if let Some(c) = char::from_u32(cp) {
                if let Some(folded) = ascii_fold(c) {
                    assert!(
                        folded.bytes().all(|b| (0x20..=0x7E).contains(&b)),
                        "U+{:04X} folded to a non-printable byte: {:?}",
                        cp,
                        folded
                    );
                    assert!(
                        !folded.contains('"'),
                        "U+{:04X} folded to a double quote; corrupts the quoted field",
                        cp
                    );
                }
            }
        }
    }

    /// O-03 regression: EVERY byte of the output must be in 0x20-0x7E.
    ///
    /// Because the old `char::is_control()` blacklist worked at the `char`
    /// level, bytes in the C1 range could leak as continuation bytes of
    /// multi-byte UTF-8: `U+02DB` is `CB 9B` on the wire, and `0x9B` (C1 CSI)
    /// would reach the firmware.
    #[test]
    fn test_sanitize_pjl_field_output_is_always_printable_ascii() {
        let hostile = "A\u{02DB}B\u{0219}C\u{1E9B}D\u{FF02}E\u{202E}F\u{1F4A9}";
        let out = sanitize_pjl_field(hostile);
        assert!(
            out.bytes().all(|b| (0x20..=0x7E).contains(&b)),
            "a non-printable byte leaked: {:?}",
            out.as_bytes()
        );
        assert!(!out.as_bytes().contains(&0x9B), "a C1 CSI byte leaked");
        assert!(!out.contains('"'));

        // The same guarantee must hold across a wide Unicode sweep too.
        for cp in (0u32..0x3000).chain(0x1F300..0x1F400) {
            if let Some(c) = char::from_u32(cp) {
                let s = format!("x{}y", c);
                let out = sanitize_pjl_field(&s);
                assert!(
                    out.bytes().all(|b| (0x20..=0x7E).contains(&b)) && !out.contains('"'),
                    "U+{:04X} produced an unsafe byte: {:?}",
                    cp,
                    out.as_bytes()
                );
            }
        }
    }

    /// O-02 regression: the limit must be in BYTES.
    ///
    /// The old character-counting limit could quadruple the real line length
    /// with multi-byte UTF-8 (a 400-emoji title -> a 531-byte line).
    #[test]
    fn test_sanitize_pjl_field_enforces_max_byte_length() {
        let huge = "A".repeat(1_000_000);
        assert_eq!(sanitize_pjl_field(&huge).len(), MAX_PJL_FIELD_BYTES);

        // Multi-byte input must obey the same BYTE limit too.
        for probe in ["Ö", "\u{1F600}", "ß", "ğ"] {
            let s = probe.repeat(4000);
            let out = sanitize_pjl_field(&s);
            assert!(
                out.len() <= MAX_PJL_FIELD_BYTES,
                "{:?} exceeded the limit: {} bytes",
                probe,
                out.len()
            );
        }

        let short = "short title";
        assert_eq!(sanitize_pjl_field(short), short);
    }

    /// Sequences that expand when folded ("ß" -> "ss") must not overrun the limit.
    #[test]
    fn test_sanitize_pjl_field_respects_limit_for_expanding_folds() {
        let out = sanitize_pjl_field(&"ß".repeat(200));
        assert!(out.len() <= MAX_PJL_FIELD_BYTES);
        assert!(out.bytes().all(|b| b == b's'));
    }

    /// The whole produced PJL line must stay within a reasonable multiple of the byte limit.
    #[test]
    fn test_pjl_lines_stay_short_with_multibyte_input() {
        let cfg = JobConfig {
            job_name: "\u{1F600}".repeat(400),
            user_name: "Ö".repeat(400),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&cfg).unwrap();
            w.end_job().unwrap();
        }
        for line in out.split(|&b| b == b'\n') {
            assert!(
                line.len() <= MAX_PJL_FIELD_BYTES + 64,
                "PJL line too long: {} bytes",
                line.len()
            );
        }
    }

    #[test]
    fn test_begin_job_blocks_pjl_line_injection() {
        // Malicious input: with a quote + CR/LF it tries to escape the quoted
        // field and inject a new "@PJL DEFAULT PASSWORD=..." line, and with ESC
        // (UEL) it tries to end the PJL envelope early and start a new
        // envelope/job.
        let malicious = JobConfig {
            job_name: "Evil\"\n@PJL DEFAULT PASSWORD=1234".to_string(),
            user_name: "attacker\x1b%-12345X@PJL SET JOBNAME=\"hijacked".to_string(),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };
        let benign = JobConfig {
            job_name: "Benign".to_string(),
            user_name: "benign".to_string(),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };

        // The jobs are closed explicitly: because `Drop` writes the closing
        // UEL when the writer is dropped (see
        // test_drop_writes_closing_uel_for_unfinished_job), both streams should
        // be complete jobs so the UEL count below measures only INJECTED
        // envelopes.
        let mut out_malicious: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out_malicious);
            w.begin_job(&malicious).unwrap();
            w.end_job().unwrap();
        }
        let mut out_benign: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out_benign);
            w.begin_job(&benign).unwrap();
            w.end_job().unwrap();
        }

        // The malicious input must produce the SAME number of PJL lines as the
        // benign one: if sanitisation failed, the embedded "\n" would inject
        // extra line(s).
        let lines_malicious = out_malicious.iter().filter(|&&b| b == b'\n').count();
        let lines_benign = out_benign.iter().filter(|&&b| b == b'\n').count();
        assert_eq!(
            lines_malicious, lines_benign,
            "the input was able to inject an extra PJL command line"
        );

        // The stream must contain only the job's own opening and closing UEL;
        // the input must not be able to add a third envelope (a new job start).
        let uel_count = count_uel(&out_malicious);
        assert_eq!(uel_count, count_uel(&out_benign));
        assert_eq!(
            uel_count, 2,
            "the input was able to inject a new UEL envelope"
        );
    }

    /// Returns the number of UELs (new PJL envelopes) in the stream.
    fn count_uel(stream: &[u8]) -> usize {
        stream
            .windows(PJL_UEL.len())
            .filter(|w| *w == PJL_UEL)
            .count()
    }

    /// Y-03 regression: if the writer is dropped after `begin_job` without
    /// `end_job`, `Drop` must write the closing UEL.
    ///
    /// `begin_job` puts the printer into QPDL with `@PJL ENTER LANGUAGE = QPDL`;
    /// if the stream ends without a UEL the printer hangs in that language and
    /// the next job is corrupted too.
    #[test]
    fn test_drop_writes_closing_uel_for_unfinished_job() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&JobConfig::default()).unwrap();
            w.begin_page(&PageConfig::default()).unwrap();
            // NO end_job: we imitate the error path.
        }
        assert!(
            out.ends_with(PJL_END),
            "the half-finished job ended without a closing UEL"
        );
        assert_eq!(count_uel(&out), 2, "an opening + closing UEL is expected");
    }

    /// If `end_job` was called explicitly, `Drop` must not write a second UEL.
    #[test]
    fn test_drop_does_not_duplicate_uel_after_end_job() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&JobConfig::default()).unwrap();
            w.end_job().unwrap();
        }
        assert_eq!(count_uel(&out), 2, "Drop wrote an extra UEL");
    }

    /// If `begin_job` was never called, `Drop` must write nothing: the printer
    /// never entered QPDL, so sending a lone UEL is meaningless.
    #[test]
    fn test_drop_writes_nothing_when_job_never_started() {
        let mut out: Vec<u8> = Vec::new();
        {
            let _w = SplStreamWriter::new(&mut out);
        }
        assert!(
            out.is_empty(),
            "output was produced before the job started: {:?}",
            out
        );
    }

    /// D-02 regression: the 16-bit width/height fields in the QPDL page header
    /// must be written big-endian, without truncation.
    ///
    /// Because the fields are `u16`, a silent narrowing from `u32` is now
    /// impossible at the type level; this test pins the encoding itself.
    #[test]
    fn test_begin_page_encodes_16bit_dimensions() {
        let cfg = PageConfig {
            width_pixels: u16::MAX,
            height_pixels: 0xABCD,
            ..PageConfig::default()
        };
        let mut out: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out).begin_page(&cfg).unwrap();

        assert_eq!(&out[0x5..0x7], &[0xFF, 0xFF], "width truncated");
        assert_eq!(&out[0x7..0x9], &[0xAB, 0xCD], "height truncated");
        assert_eq!(out.len(), 17);
    }

    /// D-05 regression: in the QPDL page header `header[0x1]` carries the
    /// VERTICAL (Y) and `header[0x10]` the HORIZONTAL (X) resolution — in that
    /// order.
    ///
    /// Source, OpenPrinting SpliX qpdl.cpp renderPage:
    ///   header[0x1]  = page->yResolution() / 100;
    ///   header[0x10] = page->xResolution() / 100;
    ///
    /// Because the order is counter-intuitive ("Y first, then X") it is easy to
    /// write reversed. This used to be a single field with the HORIZONTAL
    /// resolution written into both bytes; a bug invisible in symmetric modes
    /// that squashed the page 2x vertically at `1200x600dpi`.
    #[test]
    fn test_begin_page_maps_resolution_axes_to_correct_bytes() {
        // Asymmetric: X = 1200, Y = 600 (a real QPDL mode).
        let cfg = PageConfig {
            resolution_x: SplResolution::Dpi1200,
            resolution_y: SplResolution::Dpi600,
            ..PageConfig::default()
        };
        let mut out: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out).begin_page(&cfg).unwrap();

        assert_eq!(
            out[0x1], 6,
            "header[0x1] must be the VERTICAL (Y) resolution: 600/100"
        );
        assert_eq!(
            out[0x10], 12,
            "header[0x10] must be the HORIZONTAL (X) resolution: 1200/100"
        );

        // Reversed direction: proves the axes are really carried separately.
        let swapped = PageConfig {
            resolution_x: SplResolution::Dpi600,
            resolution_y: SplResolution::Dpi1200,
            ..PageConfig::default()
        };
        let mut out2: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out2)
            .begin_page(&swapped)
            .unwrap();
        assert_eq!(out2[0x1], 12);
        assert_eq!(out2[0x10], 6);

        // In the symmetric case the two bytes are equal (compatible with the old behaviour).
        for dpi in [
            SplResolution::Dpi300,
            SplResolution::Dpi600,
            SplResolution::Dpi1200,
        ] {
            let sym = PageConfig {
                resolution_x: dpi,
                resolution_y: dpi,
                ..PageConfig::default()
            };
            let mut o: Vec<u8> = Vec::new();
            SplStreamWriter::new(&mut o).begin_page(&sym).unwrap();
            assert_eq!(o[0x1], o[0x10]);
            assert_eq!(o[0x1] as u32, dpi.dpi() / 100);
        }
    }

    /// The QPDL paper-source codes (SpliX printer.cpp) must be carried
    /// one-to-one; `0` means "not selected" and falls back to Auto, while
    /// unrecognised values return `None` rather than silently becoming a code.
    #[test]
    fn test_paper_source_from_media_position() {
        assert_eq!(
            SplPaperSource::from_media_position(0),
            Some(SplPaperSource::Auto)
        );
        assert_eq!(
            SplPaperSource::from_media_position(1),
            Some(SplPaperSource::Auto)
        );
        assert_eq!(
            SplPaperSource::from_media_position(2),
            Some(SplPaperSource::Manual)
        );
        assert_eq!(
            SplPaperSource::from_media_position(3),
            Some(SplPaperSource::Multi)
        );
        assert_eq!(
            SplPaperSource::from_media_position(4),
            Some(SplPaperSource::Upper)
        );
        assert_eq!(
            SplPaperSource::from_media_position(5),
            Some(SplPaperSource::Lower)
        );
        for unknown in [6u32, 7, 99, u32::MAX] {
            assert_eq!(
                SplPaperSource::from_media_position(unknown),
                None,
                "{}",
                unknown
            );
        }
        // Because the codes are written raw into the QPDL page header, pin
        // their numeric values too.
        assert_eq!(SplPaperSource::Auto as u8, 1);
        assert_eq!(SplPaperSource::Manual as u8, 2);
    }

    /// Nothing outside the PJL vocabulary can become a `PAPERTYPE` value.
    #[test]
    fn test_pjl_paper_type_only_accepts_printer_vocabulary() {
        for known in PJL_PAPER_TYPES {
            assert_eq!(pjl_paper_type(known), Some(known));
            // An ASCII case difference must not cause a fallback.
            assert_eq!(pjl_paper_type(&known.to_ascii_lowercase()), Some(known));
            // Leading/trailing spaces (a 64-byte C string) must be tolerated.
            assert_eq!(pjl_paper_type(&format!("  {}  ", known)), Some(known));
        }
        // Old, human-readable PPD names are no longer protocol values.
        for unknown in ["", "Plain", "Envelope", "CardStock", "NORMAL\nEVIL", "EN V"] {
            assert_eq!(pjl_paper_type(unknown), None, "{:?} was accepted", unknown);
        }
    }

    /// The `PAPERTYPE` line is UNQUOTED: because only the table's constants can
    /// enter the value, the line cannot be corrupted by any input.
    #[test]
    fn test_begin_job_emits_papertype_from_config() {
        for paper_type in PJL_PAPER_TYPES {
            let cfg = JobConfig {
                paper_type,
                ..JobConfig::default()
            };
            let mut out: Vec<u8> = Vec::new();
            {
                let mut w = SplStreamWriter::new(&mut out);
                w.begin_job(&cfg).unwrap();
                w.end_job().unwrap();
            }
            let text = String::from_utf8_lossy(&out).into_owned();
            assert!(
                text.contains(&format!("@PJL SET PAPERTYPE={}\n", paper_type)),
                "{} was not written: {}",
                paper_type,
                text
            );
            // The line count must stay fixed: the value never splits a line.
            // A simplex job has 12 lines: SERVICEDATE, USERNAME, JOBNAME,
            // POWERSAVE, POWERSAVETIME, JAMRECOVERY, DUPLEX, PAPERTYPE,
            // ALTITUDE, DENSITY, RET, ENTER LANGUAGE.
            assert_eq!(
                out.iter().filter(|&&b| b == b'\n').count(),
                12,
                "the PJL line count changed ({})",
                paper_type
            );
        }
    }

    #[test]
    fn test_algo0x11_compression() {
        let mut sample = vec![0x00u8; 620 * 64];
        sample[100..500].fill(0xAA);
        let comp = Algo0x11::compress(&sample).unwrap();
        assert!(comp.len() < sample.len());
    }

    #[test]
    fn test_algo0x11_roundtrip_synthetic() {
        // Non-random but repetitive+varying content: text-like data.
        let mut sample = vec![0u8; 620 * 128];
        for (i, b) in sample.iter_mut().enumerate() {
            *b = ((i * 37 + (i / 620) * 13) % 251) as u8;
            if (i / 620) % 5 == 0 && (i % 620) > 50 && (i % 620) < 400 {
                *b = 0xFF;
            }
        }
        let comp = Algo0x11::compress(&sample).unwrap();
        let decomp = Algo0x11::decompress(&comp);
        assert_eq!(decomp.len(), sample.len(), "decompressed length mismatch");
        assert_eq!(decomp, sample, "decompressed content mismatch");
    }

    #[test]
    fn test_algo0x11_roundtrip_real_band0() {
        use crate::raster::CupsRasterReader;
        use std::fs::File;
        use std::io::BufReader;

        let path = "target/test_output/test.raster";
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("SKIP: {} does not exist, test skipped", path);
                return;
            }
        };
        let mut reader = CupsRasterReader::new(BufReader::new(file)).unwrap();
        let header = reader.next_page_header().unwrap().unwrap();

        let page_width_pixels =
            (((header.page_size_points[0] as f64 * header.hw_resolution[0] as f64 / 72.0).ceil())
                as u32
                + 7)
                & !7u32;
        let band_width_bytes = page_width_pixels.div_ceil(8) as usize;
        let cups_line_bytes = header.bytes_per_line as usize;
        let margin_bytes = (band_width_bytes - cups_line_bytes) / 2;
        let bytes_to_copy = cups_line_bytes.min(band_width_bytes - margin_bytes);
        let band_height = 128usize;

        let mut line_buffer = vec![0u8; cups_line_bytes];
        let mut band_data = vec![0u8; band_width_bytes * band_height];
        let mut found_band = None;

        // The top of the page is usually blank margin; advance until the first
        // band holding real (non-uniform) content is found.
        for band_idx in 0..(header.height as usize / band_height + 1) {
            for b in band_data.iter_mut() {
                *b = 0;
            }
            let mut lines_read = 0;
            for y in 0..band_height {
                if reader.read_line(&mut line_buffer).is_err() {
                    break;
                }
                lines_read += 1;
                let dst = y * band_width_bytes + margin_bytes;
                band_data[dst..dst + bytes_to_copy].copy_from_slice(&line_buffer[..bytes_to_copy]);
            }
            if lines_read == 0 {
                break;
            }
            let mut inverted = band_data.clone();
            for b in &mut inverted {
                *b = !*b;
            }
            if inverted.iter().any(|&b| b != inverted[0]) {
                found_band = Some((band_idx, inverted));
                break;
            }
        }

        let (band_idx, band_data) = found_band.expect("no non-uniform band found on the page");
        eprintln!("Band selected for the test: {}", band_idx);

        let comp = Algo0x11::compress(&band_data).unwrap();
        let decomp = Algo0x11::decompress(&comp);
        assert_eq!(
            decomp.len(),
            band_data.len(),
            "decompressed length mismatch: {} != {}",
            decomp.len(),
            band_data.len()
        );
        let first_diff = decomp
            .iter()
            .zip(band_data.iter())
            .position(|(a, b)| a != b);
        assert_eq!(
            first_diff, None,
            "decompressed content differs from original at byte offset {:?}",
            first_diff
        );
    }
}
