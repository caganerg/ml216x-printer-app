//! # CUPS Raster Parser
//!
//! This module parses the standard CUPS Raster stream that arrives from the
//! Linux CUPS filter chain (`cups-filters` / `libcupsfilters`).
//!
//! Note: it is based on the classic CUPS Raster (`RaSt`, `RaS2`, `RaS3`) and the
//! `cups_page_header2_t` (1796 bytes) data structure, not PWG Raster (`PwgR`
//! magic).
//!
//! All three versions are supported. v1 and v3 carry page data uncompressed;
//! v2 (`RaS2`/`2SaR`, i.e. PWG Raster) uses line-RLE and is decoded
//! transparently by `CupsLineDecoder`. The caller always uses
//! `CupsRasterReader::read_line` and does not see the difference.

use std::io::{self, Read};

// Moved to `geometry` during the crate split: the PAPPL front end needs these
// two types, and `geometry` is compiled whether or not `golden-replay` is.
pub use crate::geometry::{CupsColorOrder, CupsColorSpace};

/// The synchronisation (magic) bytes of the CUPS Raster specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CupsRasterVersion {
    /// CUPS Raster Version 1 - Big Endian (`RaSt`)
    V1Be,
    /// CUPS Raster Version 1 - Little Endian (`tSaR`)
    V1Le,
    /// CUPS Raster Version 2 - Big Endian (`RaS2`)
    V2Be,
    /// CUPS Raster Version 2 - Little Endian (`2SaR`)
    V2Le,
    /// CUPS Raster Version 3 - Big Endian (`RaS3`)
    V3Be,
    /// CUPS Raster Version 3 - Little Endian (`3SaR`)
    V3Le,
}

impl CupsRasterVersion {
    /// Returns whether the stream is big-endian.
    #[inline]
    pub fn is_big_endian(&self) -> bool {
        matches!(
            self,
            CupsRasterVersion::V1Be | CupsRasterVersion::V2Be | CupsRasterVersion::V3Be
        )
    }

    /// The expected byte size of the header structure.
    ///
    /// V1 = 420 bytes (`cups_page_header_t`; last field `cupsRowStep` 416..420),
    /// V2/V3 = 1796 bytes (`cups_page_header2_t`; last field `cupsPageSizeName`
    /// 1732..1796). These values were measured with `sizeof` against CUPS's own
    /// `<cups/raster.h>` header.
    ///
    /// The 436 used previously for V1 was 16 bytes larger than the real struct:
    /// each page-header read swallowed 16 extra bytes of pixel data, so the
    /// first page printed shifted and the next page header was parsed from an
    /// entirely wrong offset (`invalid cupsBytesPerLine value: 0`).
    #[inline]
    pub fn header_size(&self) -> usize {
        match self {
            CupsRasterVersion::V1Be | CupsRasterVersion::V1Le => 420,
            _ => 1796,
        }
    }

    /// Returns whether the stream's PAGE DATA is compressed with CUPS line-RLE.
    ///
    /// CUPS Raster v2 (`RaS2`/`2SaR`) compresses page data with a per-line RLE:
    /// in `<cups/raster.h>` `CUPS_RASTER_SYNC_PWG` is equated directly to
    /// `CUPS_RASTER_SYNCv2` and is compressed by the PWG Raster definition. v1
    /// and v3 are uncompressed. For the same page produced by libcups
    /// (620 B/line x 200 lines = 124,000 bytes of raw data): the `3SaR` file is
    /// 125,800 bytes, the `2SaR`/`RaS2` file 1,811 bytes.
    #[inline]
    pub fn is_compressed(&self) -> bool {
        matches!(self, CupsRasterVersion::V2Be | CupsRasterVersion::V2Le)
    }
}

/// A Rust model of the `cups_page_header2_t` (CUPS V2 / V3) and
/// `cups_page_header_t` (CUPS V1) page-header structures.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PageHeader {
    // CUPS V1 header fields (0..420 bytes)
    pub media_class: String,
    pub media_color: String,
    pub media_type: String,
    pub output_type: String,

    pub advance_distance: u32,
    pub advance_media: u32,
    pub collate: bool,
    pub cut_media: u32,
    pub duplex: bool,
    pub hw_resolution: [u32; 2],        // [X DPI, Y DPI]
    pub imaging_bounding_box: [u32; 4], // [Left, Bottom, Right, Top] (pt)
    pub insert_sheet: bool,
    pub jog: u32,
    pub leading_edge: u32,
    pub margins: [u32; 2], // [Left, Bottom] (pt)
    pub manual_feed: bool,
    pub media_position: u32,
    pub media_weight: u32,
    pub mirror_print: bool,
    pub negative_print: bool,
    pub num_copies: u32,
    pub orientation: u32,
    pub output_face_up: bool,
    pub page_size_points: [u32; 2], // [Width, Length] (1/72 inch points)
    pub separations: bool,
    pub tray_switch: bool,
    /// `Tumble` — the field where CUPS reports the BINDING EDGE for duplex
    /// printing: `false` = long edge (DuplexNoTumble), `true` = short edge
    /// (DuplexTumble).
    ///
    /// This field used to be parsed under the name `turn_off`; no such CUPS
    /// field exists. In `cups_page_header_t` in `<cups/raster.h>`, the field
    /// immediately before `cupsWidth` (that is, at byte 368) is `Tumble`. The
    /// offset was already correct, only the name was wrong — and since the
    /// field was unused, it went unnoticed.
    ///
    /// NOTE: this `tumble` is NOT THE SAME THING as the `tumble` byte in the
    /// QPDL page header. This field selects the binding edge; the QPDL byte
    /// indicates which side of the sheet the page prints on and, in SpliX, is
    /// computed from the parity of the page number (see spl.rs `begin_page`).
    pub tumble: bool,

    pub width: u32,  // cupsWidth (pixels)
    pub height: u32, // cupsHeight (pixels)
    pub cups_media_type: u32,
    pub bits_per_color: u32,         // cupsBitsPerColor (1, 8, 16)
    pub bits_per_pixel: u32,         // cupsBitsPerPixel (1, 8, 24, 32)
    pub bytes_per_line: u32,         // cupsBytesPerLine
    pub color_order: CupsColorOrder, // cupsColorOrder
    pub color_space: CupsColorSpace, // cupsColorSpace
    pub compression: u32,            // cupsCompression (0 = uncompressed)
    pub row_count: u32,
    pub row_feed: u32,
    pub row_step: u32,

    // CUPS V2 / V3 extended fields (420..1796 bytes)
    pub num_colors: u32,
    pub page_size_f: [f32; 2],
    pub rendering_intent: Option<String>,
    pub page_size_name: Option<String>,
}

impl PageHeader {
    fn parse_c_string(bytes: &[u8]) -> String {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..len]).trim().to_string()
    }

    #[inline]
    fn read_u32(buf: &[u8], offset: usize, is_be: bool) -> u32 {
        let slice: [u8; 4] = buf[offset..offset + 4]
            .try_into()
            .expect("wrong slice length");
        if is_be {
            u32::from_be_bytes(slice)
        } else {
            u32::from_le_bytes(slice)
        }
    }

    #[inline]
    fn read_f32(buf: &[u8], offset: usize, is_be: bool) -> f32 {
        let slice: [u8; 4] = buf[offset..offset + 4]
            .try_into()
            .expect("wrong slice length");
        if is_be {
            f32::from_be_bytes(slice)
        } else {
            f32::from_le_bytes(slice)
        }
    }

    /// Parses a CUPS Raster page header.
    pub fn parse(buf: &[u8], version: CupsRasterVersion) -> io::Result<Self> {
        let is_be = version.is_big_endian();
        let expected_size = version.header_size();

        if buf.len() < expected_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "the CUPS Raster header is too short (expected: {} bytes, got: {} bytes)",
                    expected_size,
                    buf.len()
                ),
            ));
        }

        // C string fields (0..256)
        let media_class = Self::parse_c_string(&buf[0..64]);
        let media_color = Self::parse_c_string(&buf[64..128]);
        let media_type = Self::parse_c_string(&buf[128..192]);
        let output_type = Self::parse_c_string(&buf[192..256]);

        let advance_distance = Self::read_u32(buf, 256, is_be);
        let advance_media = Self::read_u32(buf, 260, is_be);
        let collate = Self::read_u32(buf, 264, is_be) != 0;
        let cut_media = Self::read_u32(buf, 268, is_be);
        let duplex = Self::read_u32(buf, 272, is_be) != 0;

        let hw_res_x = Self::read_u32(buf, 276, is_be);
        let hw_res_y = Self::read_u32(buf, 280, is_be);

        let img_bbox = [
            Self::read_u32(buf, 284, is_be),
            Self::read_u32(buf, 288, is_be),
            Self::read_u32(buf, 292, is_be),
            Self::read_u32(buf, 296, is_be),
        ];

        let insert_sheet = Self::read_u32(buf, 300, is_be) != 0;
        let jog = Self::read_u32(buf, 304, is_be);
        let leading_edge = Self::read_u32(buf, 308, is_be);
        let margin_x = Self::read_u32(buf, 312, is_be);
        let margin_y = Self::read_u32(buf, 316, is_be);
        let manual_feed = Self::read_u32(buf, 320, is_be) != 0;
        let media_position = Self::read_u32(buf, 324, is_be);
        let media_weight = Self::read_u32(buf, 328, is_be);
        let mirror_print = Self::read_u32(buf, 332, is_be) != 0;
        let negative_print = Self::read_u32(buf, 336, is_be) != 0;
        let num_copies = Self::read_u32(buf, 340, is_be);
        let orientation = Self::read_u32(buf, 344, is_be);
        let output_face_up = Self::read_u32(buf, 348, is_be) != 0;
        let page_sz_w = Self::read_u32(buf, 352, is_be);
        let page_sz_h = Self::read_u32(buf, 356, is_be);
        let separations = Self::read_u32(buf, 360, is_be) != 0;
        let tray_switch = Self::read_u32(buf, 364, is_be) != 0;
        let tumble = Self::read_u32(buf, 368, is_be) != 0;

        let width = Self::read_u32(buf, 372, is_be);
        let height = Self::read_u32(buf, 376, is_be);
        let cups_media_type = Self::read_u32(buf, 380, is_be);
        let bits_per_color = Self::read_u32(buf, 384, is_be);
        let bits_per_pixel = Self::read_u32(buf, 388, is_be);
        let bytes_per_line = Self::read_u32(buf, 392, is_be);
        let color_order_val = Self::read_u32(buf, 396, is_be);
        let color_space_val = Self::read_u32(buf, 400, is_be);
        let compression = Self::read_u32(buf, 404, is_be);
        let row_count = Self::read_u32(buf, 408, is_be);
        let row_feed = Self::read_u32(buf, 412, is_be);
        let row_step = Self::read_u32(buf, 416, is_be);

        // V2 / V3 extended fields
        let (num_colors, page_size_f, rendering_intent, page_size_name) = if expected_size >= 1796 {
            let num_colors = Self::read_u32(buf, 420, is_be);
            let ps_w_f = Self::read_f32(buf, 428, is_be);
            let ps_h_f = Self::read_f32(buf, 432, is_be);

            // cupsRenderingIntent: 1668..1732
            let intent_raw = Self::parse_c_string(&buf[1668..1732]);
            let intent = if intent_raw.is_empty() {
                None
            } else {
                Some(intent_raw)
            };

            // cupsPageSizeName: 1732..1796
            let name_raw = Self::parse_c_string(&buf[1732..1796]);
            let name = if name_raw.is_empty() {
                None
            } else {
                Some(name_raw)
            };

            (num_colors, [ps_w_f, ps_h_f], intent, name)
        } else {
            (0, [page_sz_w as f32, page_sz_h as f32], None, None)
        };

        Ok(Self {
            media_class,
            media_color,
            media_type,
            output_type,
            advance_distance,
            advance_media,
            collate,
            cut_media,
            duplex,
            hw_resolution: [hw_res_x, hw_res_y],
            imaging_bounding_box: img_bbox,
            insert_sheet,
            jog,
            leading_edge,
            margins: [margin_x, margin_y],
            manual_feed,
            media_position,
            media_weight,
            mirror_print,
            negative_print,
            num_copies,
            orientation,
            output_face_up,
            page_size_points: [page_sz_w, page_sz_h],
            separations,
            tray_switch,
            tumble,
            width,
            height,
            cups_media_type,
            bits_per_color,
            bits_per_pixel,
            bytes_per_line,
            color_order: CupsColorOrder::from(color_order_val),
            color_space: CupsColorSpace(color_space_val),
            compression,
            row_count,
            row_feed,
            row_step,
            num_colors,
            page_size_f,
            rendering_intent,
            page_size_name,
        })
    }

    /// The total raw (uncompressed) pixel-data size of the page.
    #[inline]
    pub fn total_raster_bytes(&self) -> u64 {
        (self.bytes_per_line as u64) * (self.height as u64)
    }
}

/// The parser that reads CUPS Raster data from the stream.
pub struct CupsRasterReader<R: Read> {
    reader: R,
    version: CupsRasterVersion,
    page_count: u32,
    /// The line decoder's per-page state for v2 streams; `None` for v1/v3.
    decoder: Option<CupsLineDecoder>,
}

impl<R: Read> CupsRasterReader<R> {
    /// Starts the reader, validating the 4-byte CUPS Raster synchronisation code.
    pub fn new(mut reader: R) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        if let Err(e) = reader.read_exact(&mut magic) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the input stream is empty (0 bytes). Check that cupsfilter, or the preceding filter stage, produced raster successfully.",
                ));
            }
            return Err(e);
        }

        let version = match &magic {
            b"RaSt" => CupsRasterVersion::V1Be,
            b"tSaR" => CupsRasterVersion::V1Le,
            b"RaS2" => CupsRasterVersion::V2Be,
            b"2SaR" => CupsRasterVersion::V2Le,
            b"RaS3" => CupsRasterVersion::V3Be,
            b"3SaR" => CupsRasterVersion::V3Le,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid CUPS Raster format: {:?} (expected: 'RaSt', 'tSaR', 'RaS2', '2SaR', 'RaS3', '3SaR')",
                        String::from_utf8_lossy(other)
                    ),
                ));
            }
        };

        Ok(Self {
            reader,
            version,
            page_count: 0,
            decoder: None,
        })
    }

    #[inline]
    pub fn version(&self) -> CupsRasterVersion {
        self.version
    }

    #[allow(dead_code)]
    #[inline]
    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Reads the next page header. Returns `Ok(None)` if the stream ends
    /// cleanly between pages (0 bytes read).
    ///
    /// `read_exact` on its own CANNOT DISTINGUISH the stream being cut off
    /// mid-header (corrupt/partial data) from a normal end of stream between
    /// two pages; both produce the same `UnexpectedEof`. To avoid silently
    /// reading a real corruption as "finish the job normally", this reads the
    /// bytes manually, counting them.
    pub fn next_page_header(&mut self) -> io::Result<Option<PageHeader>> {
        let header_len = self.version.header_size();
        let mut buf = vec![0u8; header_len];

        let mut total_read = 0usize;
        while total_read < header_len {
            match self.reader.read(&mut buf[total_read..]) {
                Ok(0) => break,
                Ok(n) => total_read += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }

        if total_read == 0 {
            return Ok(None);
        }
        if total_read < header_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "the CUPS Raster stream ended in the middle of a page header \
                     ({} of {} bytes read). The preceding filter stage may have been \
                     interrupted.",
                    total_read, header_len
                ),
            ));
        }

        self.page_count += 1;
        let header = PageHeader::parse(&buf, self.version)?;

        // The compression state is reset per PAGE: a line-repeat counter left
        // over from one page must not leak into the next.
        self.decoder = if self.version.is_compressed() {
            Some(CupsLineDecoder::new(&header))
        } else {
            None
        };

        Ok(Some(header))
    }

    /// Reads A SINGLE LINE of page raster data and fills `out` completely.
    ///
    /// For uncompressed streams (v1/v3) this is a plain `read_exact`. For
    /// v2/PWG streams the line is encoded with CUPS's PackBits-derived line-RLE
    /// and is decoded here; the caller does not see the difference.
    ///
    /// `out`'s length must be `cupsBytesPerLine` on every call. The decoder
    /// sizes its own buffer from this length — NOT from the (still
    /// unvalidated, untrusted) `bytes_per_line` field in the header. This way a
    /// corrupt header cannot trigger a huge allocation before
    /// `validate_page_header` has even run.
    pub fn read_line(&mut self, out: &mut [u8]) -> io::Result<()> {
        match &mut self.decoder {
            None => self.reader.read_exact(out),
            Some(decoder) => decoder.read_line(&mut self.reader, out),
        }
    }
}

/// The CUPS Raster v2 (`RaS2`/`2SaR`, same as PWG Raster) line-RLE decoder.
///
/// The encoding, per line, is:
///
/// ```text
/// [repeat]  : this line is repeated (repeat + 1) times
/// then, until cupsBytesPerLine is filled:
///   n == 128 : fill to the end of the line with the blank colour
///   n >  128 : a raw (literal) copy of (257 - n) pixels follows
///   n <  128 : the next single pixel is repeated (n + 1) times
/// ```
///
/// The encoding was verified one-to-one against libcups's
/// `cupsRasterWritePixels` output: a 200-line page of 620 zero bytes each is
/// encoded as `c7 7f 00 7f 00 7f 00 7f 00 6b 00` (11 bytes) —
/// that is `[199]` + `[127, 0x00] x4` + `[107, 0x00]` = 200 lines x 620 bytes.
struct CupsLineDecoder {
    /// Bytes per pixel. For `cupsBitsPerPixel < 8` it is taken as 1, like
    /// libcups; the repeat and literal-copy counts are in PIXELS, not bytes.
    bpp: usize,
    /// The fill used for the `n == 128` case (blank to end of line).
    ///
    /// libcups fills the blank with `0x00` in colour spaces that add
    /// toner/ink (K, CMY, CMYK, White, Gold, Silver) and with `0xFF` in the
    /// others.
    blank_fill: u8,
    /// How many more times the last decoded line is to be repeated.
    repeat_remaining: u32,
    /// The last decoded line; repeats are copied from here. Sized from the
    /// buffer length the caller passes on the first `read_line` call.
    last_line: Vec<u8>,
}

impl CupsLineDecoder {
    fn new(header: &PageHeader) -> Self {
        // libcups cups_raster_update(): at depths below 8 bits, bpp is 1.
        let bpp = if header.bits_per_pixel >= 8 {
            (header.bits_per_pixel as usize).div_ceil(8)
        } else {
            1
        };

        Self {
            bpp: bpp.max(1),
            blank_fill: header.color_space.blank_fill(),
            repeat_remaining: 0,
            last_line: Vec::new(),
        }
    }

    fn read_line<R: Read>(&mut self, reader: &mut R, out: &mut [u8]) -> io::Result<()> {
        if out.is_empty() {
            return Ok(());
        }

        // The buffer is set up from the caller's length on first use; on later
        // calls the length cannot change (a change mid-page would be a caller
        // bug, and an error is preferable to silently decoding wrong).
        if self.last_line.is_empty() {
            self.last_line = vec![0u8; out.len()];
        } else if self.last_line.len() != out.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the line length changed in the middle of a page: {} -> {}",
                    self.last_line.len(),
                    out.len()
                ),
            ));
        }

        if self.repeat_remaining > 0 {
            self.repeat_remaining -= 1;
            out.copy_from_slice(&self.last_line);
            return Ok(());
        }

        let repeat = read_u8(reader)?;
        self.repeat_remaining = repeat as u32;
        self.decode_line(reader)?;
        out.copy_from_slice(&self.last_line);
        Ok(())
    }

    fn decode_line<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let line_len = self.last_line.len();
        let bpp = self.bpp;
        let mut pos = 0usize;

        while pos < line_len {
            let n = read_u8(reader)?;

            if n == 128 {
                // Fill to the end of the line with the blank colour.
                self.last_line[pos..].fill(self.blank_fill);
                return Ok(());
            }

            if n > 128 {
                // A literal copy of (257 - n) pixels: n=255 -> 2, n=129 -> 128.
                let count = (257 - n as usize).checked_mul(bpp).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the CUPS v2 literal record length overflowed",
                    )
                })?;
                if count > line_len - pos {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "the CUPS v2 literal record crosses the line boundary: {} bytes left, record {} bytes",
                            line_len - pos,
                            count
                        ),
                    ));
                }
                reader.read_exact(&mut self.last_line[pos..pos + count])?;
                pos += count;
                continue;
            }

            // The next single pixel is repeated (n + 1) times.
            let count = (n as usize + 1).checked_mul(bpp).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the CUPS v2 repeat record length overflowed",
                )
            })?;
            if count > line_len - pos {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the CUPS v2 repeat record crosses the line boundary: {} bytes left, record {} bytes",
                        line_len - pos,
                        count
                    ),
                ));
            }

            reader.read_exact(&mut self.last_line[pos..pos + bpp])?;
            let (written, rest) = self.last_line.split_at_mut(pos + bpp);
            let pixel = &written[pos..pos + bpp];
            let repeats = count / bpp - 1;
            for chunk in rest.chunks_mut(bpp).take(repeats) {
                chunk.copy_from_slice(pixel);
            }
            pos += count;
        }

        Ok(())
    }
}

#[inline]
fn read_u8<R: Read>(reader: &mut R) -> io::Result<u8> {
    let mut b = [0u8; 1];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_cups_sync_words() {
        // All six versions are accepted; v2 (`RaS2`/`2SaR`) carries page data
        // with line-RLE and is decoded transparently by `CupsLineDecoder` (see
        // main.rs `test_v2_and_v3_streams_produce_identical_output`). Here we
        // verify the version/endian mapping through a representative v3 and v1
        // stream, with v2's flags checked separately below.
        let v3_be = b"RaS3";
        let reader = CupsRasterReader::new(Cursor::new(v3_be)).unwrap();
        assert_eq!(reader.version(), CupsRasterVersion::V3Be);
        assert!(reader.version().is_big_endian());
        assert_eq!(reader.version().header_size(), 1796);

        let v1_le = b"tSaR";
        let reader_v1 = CupsRasterReader::new(Cursor::new(v1_le)).unwrap();
        assert_eq!(reader_v1.version(), CupsRasterVersion::V1Le);
        assert!(!reader_v1.version().is_big_endian());
        assert_eq!(reader_v1.version().header_size(), 420);

        // The version mapping must stay correct independently of rejecting the stream.
        assert_eq!(CupsRasterVersion::V2Be.header_size(), 1796);
        assert!(CupsRasterVersion::V2Be.is_compressed());
        assert!(CupsRasterVersion::V2Le.is_compressed());
    }

    #[test]
    fn test_cups_header_parse_canonical() {
        let mut header_buf = vec![0u8; 1796];

        // HWResolution: [600, 600]
        header_buf[276..280].copy_from_slice(&600u32.to_be_bytes());
        header_buf[280..284].copy_from_slice(&600u32.to_be_bytes());

        // PageSize points: [595, 842] (A4)
        header_buf[352..356].copy_from_slice(&595u32.to_be_bytes());
        header_buf[356..360].copy_from_slice(&842u32.to_be_bytes());

        // width: 4960, height: 7016
        header_buf[372..376].copy_from_slice(&4960u32.to_be_bytes());
        header_buf[376..380].copy_from_slice(&7016u32.to_be_bytes());

        // bits_per_color: 1, bits_per_pixel: 1, bytes_per_line: 620
        header_buf[384..388].copy_from_slice(&1u32.to_be_bytes());
        header_buf[388..392].copy_from_slice(&1u32.to_be_bytes());
        header_buf[392..396].copy_from_slice(&620u32.to_be_bytes());

        // color_space: 3 (K/Black)
        header_buf[400..404].copy_from_slice(&3u32.to_be_bytes());

        // cupsPageSizeName at offset 1732: "A4"
        header_buf[1732..1734].copy_from_slice(b"A4");

        let header = PageHeader::parse(&header_buf, CupsRasterVersion::V2Be).unwrap();
        assert_eq!(header.hw_resolution, [600, 600]);
        assert_eq!(header.page_size_points, [595, 842]);
        assert_eq!(header.width, 4960);
        assert_eq!(header.height, 7016);
        assert_eq!(header.bits_per_color, 1);
        assert_eq!(header.bits_per_pixel, 1);
        assert_eq!(header.bytes_per_line, 620);
        assert_eq!(header.color_space, CupsColorSpace::K);
        assert_eq!(header.page_size_name.as_deref(), Some("A4"));
        assert_eq!(header.total_raster_bytes(), 620 * 7016);
    }

    /// The V1 header size must match the real size of CUPS's
    /// `cups_page_header_t` struct (420 bytes). The 436 used previously
    /// swallowed 16 bytes of pixel data per header and shifted the stream.
    #[test]
    fn test_v1_header_size_matches_cups_struct() {
        assert_eq!(CupsRasterVersion::V1Be.header_size(), 420);
        assert_eq!(CupsRasterVersion::V1Le.header_size(), 420);
        // V2/V3 (`cups_page_header2_t`) is unchanged.
        assert_eq!(CupsRasterVersion::V3Be.header_size(), 1796);
    }

    /// Y-02 regression: consecutive V1 pages must stay in sync in the stream.
    ///
    /// If even one byte too many is read for the header size, page 2's fields
    /// are parsed from the wrong offset; with 436 this test produced
    /// `bytes_per_line == 0`.
    #[test]
    fn test_v1_stream_stays_in_sync_across_pages() {
        const V1_HEADER_LEN: usize = 420;

        let mut header = vec![0u8; V1_HEADER_LEN];
        let mut put = |off: usize, val: u32| {
            header[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600); // hw_resolution[0]
        put(280, 600); // hw_resolution[1]
        put(352, 595); // page_size_points[0]
        put(356, 842); // page_size_points[1]
        put(372, 32); // width
        put(376, 3); // height
        put(384, 1); // bits_per_color
        put(388, 1); // bits_per_pixel
        put(392, 4); // bytes_per_line = ceil(32 * 1 / 8)
        put(400, 3); // color_space = K
        put(416, 0xABCD); // row_step: the LAST field, at the 420 boundary

        let pixels = vec![0u8; 4 * 3];
        let mut stream = b"RaSt".to_vec();
        for _ in 0..2 {
            stream.extend_from_slice(&header);
            stream.extend_from_slice(&pixels);
        }

        let mut reader = CupsRasterReader::new(Cursor::new(stream)).unwrap();
        for page in 1..=2 {
            let h = reader
                .next_page_header()
                .unwrap()
                .unwrap_or_else(|| panic!("could not read page {} header", page));
            assert_eq!(h.bytes_per_line, 4, "page {} shifted", page);
            assert_eq!(h.width, 32, "page {} shifted", page);
            assert_eq!(h.height, 3, "page {} shifted", page);
            assert_eq!(h.row_step, 0xABCD, "page {} last field shifted", page);

            // Consume the page data so the next header is read from the right offset.
            let mut line = vec![0u8; 4];
            for _ in 0..3 {
                reader.read_line(&mut line).unwrap();
            }
        }
        assert!(
            reader.next_page_header().unwrap().is_none(),
            "the stream must end cleanly"
        );
    }

    /// All six sync words must be accepted; the compression flag must be set correctly.
    #[test]
    fn test_accepts_all_known_sync_words() {
        for (magic, compressed) in [
            (b"RaSt", false),
            (b"tSaR", false),
            (b"RaS2", true),
            (b"2SaR", true),
            (b"RaS3", false),
            (b"3SaR", false),
        ] {
            let reader = CupsRasterReader::new(Cursor::new(magic))
                .unwrap_or_else(|e| panic!("{:?} reddedildi: {}", magic, e));
            assert_eq!(reader.version().is_compressed(), compressed, "{:?}", magic);
        }
    }

    /// Builds a single-line v2 stream for 1 byte/pixel.
    fn v2_page(bytes_per_line: u32, height: u32, payload: &[u8]) -> Vec<u8> {
        let mut hdr = vec![0u8; 1796];
        let mut put = |off: usize, val: u32| {
            hdr[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600);
        put(280, 600);
        put(352, 595);
        put(356, 842);
        put(372, bytes_per_line * 8);
        put(376, height);
        put(384, 1);
        put(388, 1);
        put(392, bytes_per_line);
        put(400, 3); // K

        let mut stream = b"RaS2".to_vec();
        stream.extend_from_slice(&hdr);
        stream.extend_from_slice(payload);
        stream
    }

    fn decode_v2(bytes_per_line: u32, height: u32, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut reader =
            CupsRasterReader::new(Cursor::new(v2_page(bytes_per_line, height, payload))).unwrap();
        reader.next_page_header().unwrap().unwrap();
        (0..height)
            .map(|_| {
                let mut line = vec![0u8; bytes_per_line as usize];
                reader.read_line(&mut line).unwrap();
                line
            })
            .collect()
    }

    /// The REAL byte sequence libcups produces must decode.
    ///
    /// The whole of a 200-line page of 620 zero bytes each, written with
    /// `cupsRasterWritePixels`, is exactly these 11 bytes; it pins the
    /// bu dosyadan bire bir okudum.
    #[test]
    fn test_v2_decodes_real_libcups_payload() {
        let payload = [
            0xc7, 0x7f, 0x00, 0x7f, 0x00, 0x7f, 0x00, 0x7f, 0x00, 0x6b, 0x00,
        ];
        let lines = decode_v2(620, 200, &payload);
        assert_eq!(lines.len(), 200);
        for (i, line) in lines.iter().enumerate() {
            assert!(line.iter().all(|&b| b == 0), "line {} is not zero", i);
        }
    }

    /// All three record kinds must decode correctly: repeat, literal copy, and
    /// blank-to-end-of-line.
    #[test]
    fn test_v2_decodes_each_record_kind() {
        // [0] no line repeat
        // [2, 0xAB]      -> 0xAB x3
        // [0xFE, 1, 2, 3] -> (257-254)=3 raw bytes
        // [128]          -> blank to end of line (K => 0x00)
        let payload = [0x00, 0x02, 0xAB, 0xFE, 0x01, 0x02, 0x03, 0x80];
        let lines = decode_v2(8, 1, &payload);
        assert_eq!(lines[0], vec![0xAB, 0xAB, 0xAB, 1, 2, 3, 0x00, 0x00]);
    }

    /// Line-repeat counter: an `[n]` header produces the line (n + 1) times.
    #[test]
    fn test_v2_line_repeat_count() {
        // [3] -> 4 lines; each line is [0,0xF0] + [128], i.e. 0xF0 then zeros
        let payload = [0x03, 0x00, 0xF0, 0x80];
        let lines = decode_v2(4, 4, &payload);
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert_eq!(line, &vec![0xF0, 0x00, 0x00, 0x00]);
        }
    }

    /// A line repeat must not cross the PAGE boundary: if a counter left over
    /// from one page leaks into the next page's first line, the whole stream shifts.
    #[test]
    fn test_v2_repeat_state_resets_between_pages() {
        // Each page declares "repeat 10 lines" but the page height is 1.
        let payload = [0x09, 0x00, 0xAA, 0x80];
        let mut stream = v2_page(4, 1, &payload);
        let second = v2_page(4, 1, &[0x09, 0x00, 0xBB, 0x80]);
        stream.extend_from_slice(&second[4..]); // magic'i atla

        let mut reader = CupsRasterReader::new(Cursor::new(stream)).unwrap();
        let mut line = vec![0u8; 4];

        reader.next_page_header().unwrap().unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line[0], 0xAA);

        reader.next_page_header().unwrap().unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line[0], 0xBB, "the previous page's repeat counter leaked");
    }

    /// Counters that exceed the declared line must not be clipped: clipping
    /// mistakes the next record's bytes for a control byte and shifts the whole stream.
    #[test]
    fn test_v2_oversized_counts_are_rejected() {
        for payload in [
            vec![0x00, 0x7F, 0x5A],
            vec![0x00, 0x81, 0x11, 0x22, 0x33, 0x44],
        ] {
            let mut reader = CupsRasterReader::new(Cursor::new(v2_page(4, 1, &payload))).unwrap();
            reader.next_page_header().unwrap().unwrap();

            let mut line = vec![0u8; 4];
            let err = reader
                .read_line(&mut line)
                .expect_err("a v2 record exceeding the line should have been rejected");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert!(
                err.to_string().contains("crosses the line boundary"),
                "{}",
                err
            );
        }
    }

    /// A v2 stream cut off mid-way must return an error, not panic.
    #[test]
    fn test_v2_truncated_payload_errors_cleanly() {
        let mut reader =
            CupsRasterReader::new(Cursor::new(v2_page(620, 10, &[0x00, 0x7F]))).unwrap();
        reader.next_page_header().unwrap().unwrap();
        let mut line = vec![0u8; 620];
        assert!(reader.read_line(&mut line).is_err());
    }
}
