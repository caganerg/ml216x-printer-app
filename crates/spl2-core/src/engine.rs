// SPDX-License-Identifier: GPL-2.0-only

//! The seam both front ends share.
//!
//! [`PageSetup`] turns a validated [`PageGeometry`] into the QPDL page header
//! and the placement the band loop needs; [`BandEncoder`] is the band loop
//! itself, moved out of the 1.x filter's `stream_page_bands`.
//!
//! The 1.x filter pulled scanlines from a `CupsRasterReader`, while PAPPL
//! pushes them one at a time from `rwriteline_cb`. The encoder is therefore
//! push-driven and the filter drives it in a loop, which is a superset of the
//! line-source trait `docs/MIGRATION-PLAN.md` §7 proposed. Band boundaries,
//! zero fill and the polarity inversion are unchanged, so the bytes are too.

use std::io::{self, Write};

use crate::geometry::{
    band_height_for, band_placement, compute_page_width_pixels, duplex_mode, hard_margin_bytes,
    sanitize_copies, validate_page_geometry, BandPlacement, PageGeometry,
};
use crate::log::{Level, Log};
use crate::qpdl::{PageConfig, SplPaperSize, SplPaperSource, SplResolution, SplStreamWriter};

/// Everything one page needs, derived from its geometry.
#[derive(Debug, Clone)]
pub struct PageSetup {
    /// The 17-byte QPDL page header.
    pub config: PageConfig,
    /// Copies, after [`sanitize_copies`]; the same value reaches the page
    /// footer and any accounting.
    pub copies: u16,
    /// Band height in scanlines (64 at 300x300 dpi, otherwise 128).
    pub band_height: usize,
    /// Band buffer width in bytes.
    pub band_width_bytes: usize,
    /// Band width in pixels, as written into each band record.
    pub band_width_pixels: u16,
    /// Where a scanline lands in the band buffer.
    pub placement: BandPlacement,
    /// How many bytes of each scanline survive placement.
    pub bytes_to_copy: usize,
    /// Bytes per incoming scanline.
    pub cups_bytes_per_line: usize,
    /// Scanlines this page must receive.
    pub total_lines: usize,
}

impl PageSetup {
    /// Validates `geometry` and derives the QPDL page header from it.
    ///
    /// `margin_pt` is the driver's hard margin in points, normally
    /// [`crate::media::HARD_MARGIN_PT`]; the golden harness passes synthetic
    /// values to reach placements the PPD cannot produce.
    ///
    /// The geometry is validated here even though the CUPS filter validates it
    /// again a few lines earlier: the filter must validate before it meters the
    /// page against its job budget, and this constructor must not depend on a
    /// caller having remembered to.
    pub fn new(
        geometry: &PageGeometry,
        margin_pt: f64,
        page_number: u32,
        log: &dyn Log,
    ) -> io::Result<Self> {
        validate_page_geometry(geometry)?;
        if !margin_pt.is_finite() || margin_pt <= 0.0 || margin_pt > 36.0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid driver hard margin",
            ));
        }

        // SpliX pageWidth computation: convert the physical page width from points to
        // pixels via the DPI, then align up to 8.
        // SpliX document.cpp: pageWidth = ((ceil(pageSizePt * dpi / 72) + 7) & ~7)
        let page_width_pixels =
            compute_page_width_pixels(geometry.page_size_points[0], geometry.hw_resolution[0]);

        // SpliX compress.cpp (the original pre-M2026 logic):
        //   bandWidthInB = lineWidthInB = (pageWidth + 7) / 8
        //   bandWidth = bandWidthInB * 8
        // The ML-2160 series does not use 256-alignment.
        let band_width_bytes = page_width_pixels.div_ceil(8);
        let band_width_pixels = band_width_bytes * 8;

        // The width and height fields in QPDL page/band records are 16-bit.
        // Truncating the values silently with `as u16` would make the size
        // reported to the printer disagree with the actual payload (a DMA/RLE
        // decode desync); instead we return an early, explicit error via
        // `try_into`.
        let to_u16 = |value: u32, field: &str| -> io::Result<u16> {
            u16::try_from(value).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} does not fit QPDL's 16-bit field: {} px", field, value),
                )
            })
        };
        let band_width_u16 = to_u16(band_width_pixels, "Band width")?;
        let page_height_u16 = to_u16(geometry.height, "Page height")?;

        // The CUPS raster data (595 B) is centred in the band width (620 B),
        // then the printer's hard margin is subtracted; see `band_placement`.
        let cups_line_bytes = geometry.bytes_per_line as usize;
        let hard_margin = hard_margin_bytes(margin_pt, geometry.hw_resolution[0]);
        let placement = band_placement(band_width_bytes as usize, cups_line_bytes, hard_margin)?;
        if (band_width_bytes as usize) < cups_line_bytes {
            // The warning is printed AFTER placement, because which edge is
            // clipped depends on `src_skip`: when the band is narrower than the
            // line and there is also a hard margin, bytes are dropped from the
            // left of the line too, so it is not only the right edge. Both are
            // stated so the operator is not pointed at the wrong edge.
            let left_note = if placement.src_skip > 0 {
                format!(
                    " and {} B will be dropped from their left edge for the hard margin",
                    placement.src_skip
                )
            } else {
                String::new()
            };
            log.log(
                Level::Warning,
                &format!(
                    "The computed band width ({} B) is narrower than the CUPS line \
                     width ({} B); lines will be clipped on the right{}.",
                    band_width_bytes, cups_line_bytes, left_note
                ),
            );
        }

        log.log(
            Level::Debug,
            &format!(
                "QPDL width: cupsWidth={}, pageWidthPx={}, bandWidthPx={}, bandWidthB={}, \
                 hardMarginB={}, dstOffsetB={}, srcSkipB={}",
                geometry.width,
                page_width_pixels,
                band_width_pixels,
                band_width_bytes,
                hard_margin,
                placement.dst_offset,
                placement.src_skip
            ),
        );

        // `validate_page_geometry` rejects unknown sizes. This second check
        // stops a silent fallback to A4 from re-appearing even if this
        // conversion is later moved to a call path separate from validation.
        let paper_size = SplPaperSize::from_dimensions_pt_exact(
            geometry.page_size_points[0],
            geometry.page_size_points[1],
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the validated paper size has no QPDL code",
            )
        })?;

        let paper_source = match SplPaperSource::from_media_position(geometry.media_position) {
            Some(source) => source,
            None => {
                log.log(
                    Level::Warning,
                    &format!(
                        "Unrecognised MediaPosition value: {}; sending Auto as the \
                         QPDL paper source. The PPD's *InputSlot choices must be \
                         numbered with the QPDL codes themselves (1=Auto, 2=Manual, \
                         3=Multi, 4=Upper, 5=Lower).",
                        geometry.media_position
                    ),
                );
                SplPaperSource::Auto
            }
        };

        let resolution_x =
            SplResolution::from_dpi_exact(geometry.hw_resolution[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the horizontal resolution has no QPDL code",
                )
            })?;
        let resolution_y =
            SplResolution::from_dpi_exact(geometry.hw_resolution[1]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the vertical resolution has no QPDL code",
                )
            })?;

        let copies = sanitize_copies(geometry.num_copies);
        let band_height = band_height_for(geometry.hw_resolution);
        let bytes_to_copy = (cups_line_bytes - placement.src_skip)
            .min(band_width_bytes as usize - placement.dst_offset);

        Ok(Self {
            config: PageConfig {
                paper_size,
                paper_source,
                // The axes are SEPARATE: QPDL `header[0x1]` carries the vertical
                // and `header[0x10]` the horizontal resolution (see qpdl.rs
                // PageConfig).
                resolution_x,
                resolution_y,
                duplex: duplex_mode(geometry.duplex, geometry.tumble),
                page_number,
                copies,
                // SpliX qpdl.cpp renderPage: width = page->width() = pageWidth
                width_pixels: band_width_u16,
                height_pixels: page_height_u16,
                qpdl_version: 3,
            },
            copies,
            band_height,
            band_width_bytes: band_width_bytes as usize,
            band_width_pixels: band_width_u16,
            placement,
            bytes_to_copy,
            cups_bytes_per_line: cups_line_bytes,
            total_lines: geometry.height as usize,
        })
    }

    /// A band encoder for this page. One per page; it owns the band buffer.
    pub fn encoder(&self) -> BandEncoder {
        BandEncoder {
            band: vec![0u8; self.band_width_bytes * self.band_height],
            band_height: self.band_height,
            band_width_pixels: self.band_width_pixels,
            placement: self.placement,
            bytes_to_copy: self.bytes_to_copy,
            cups_bytes_per_line: self.cups_bytes_per_line,
            total_lines: self.total_lines,
            lines_written: 0,
            lines_in_band: 0,
        }
    }
}

/// Accumulates scanlines into QPDL band records.
///
/// Moved from the 1.x filter's `stream_page_bands`, turned inside out so the
/// caller pushes scanlines instead of the encoder pulling them. What is
/// emitted is unchanged: each band is zero filled, written column-major,
/// inverted whole, and declared at the full band height even when the last
/// band is short.
pub struct BandEncoder {
    band: Vec<u8>,
    band_height: usize,
    band_width_pixels: u16,
    placement: BandPlacement,
    bytes_to_copy: usize,
    cups_bytes_per_line: usize,
    total_lines: usize,
    lines_written: usize,
    lines_in_band: usize,
}

impl BandEncoder {
    /// Scanlines accepted so far.
    pub fn lines_written(&self) -> usize {
        self.lines_written
    }

    /// Places one scanline, emitting a band record once the band is full.
    ///
    /// A short line or an extra line is an error rather than something to pad
    /// or drop: a page whose height and payload disagree desynchronises the
    /// printer's RLE decoder, which is the failure this whole path exists to
    /// avoid.
    pub fn write_line<W: Write>(
        &mut self,
        writer: &mut SplStreamWriter<W>,
        line: &[u8],
    ) -> io::Result<()> {
        if line.len() != self.cups_bytes_per_line {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "scanline is {} bytes, the page header declared {}",
                    line.len(),
                    self.cups_bytes_per_line
                ),
            ));
        }
        if self.lines_written >= self.total_lines {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "more scanlines than the page declared: {} already written",
                    self.total_lines
                ),
            ));
        }
        if self.lines_in_band == 0 {
            // The last band may have fewer lines, so zeroing it is required.
            self.band.fill(0);
        }

        // SpliX algo0x11.h: Algo0x11::reverseLineColumn() == true, so
        // _compressBandedPage in compress.cpp fills the band buffer
        // COLUMN-MAJOR (transposed):
        //   band[x * bandHeight + y] = planes[i][x + hardMarginXInB + ...]
        // A row-major fill makes the printer decode a transposed/noisy image
        // even when the compression itself is correct.
        let y = self.lines_in_band;
        for (c, &byte) in line[self.placement.src_skip..]
            .iter()
            .take(self.bytes_to_copy)
            .enumerate()
        {
            let col = self.placement.dst_offset + c;
            self.band[col * self.band_height + y] = byte;
        }

        self.lines_in_band += 1;
        self.lines_written += 1;
        if self.lines_in_band == self.band_height {
            self.flush(writer)?;
        }
        Ok(())
    }

    /// Emits the trailing partial band and checks the page is complete.
    pub fn finish<W: Write>(mut self, writer: &mut SplStreamWriter<W>) -> io::Result<()> {
        if self.lines_written != self.total_lines {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "page ended after {} of {} scanlines",
                    self.lines_written, self.total_lines
                ),
            ));
        }
        if self.lines_in_band > 0 {
            self.flush(writer)?;
        }
        Ok(())
    }

    fn flush<W: Write>(&mut self, writer: &mut SplStreamWriter<W>) -> io::Result<()> {
        // The Samsung ML-2160 series QPDL laser engine works with the OPPOSITE
        // polarity to the CUPS K colour space:
        //   CUPS K:     0 = white (no toner),  1 = black (toner)
        //   Samsung:    0 = black (lay toner), 1 = white (no toner)
        // Empirically: without inversion the page comes out solid black.
        for b in &mut self.band {
            *b = !*b;
        }
        writer.write_compressed_band(
            self.band_width_pixels,
            self.band_height as u16,
            &self.band,
        )?;
        self.lines_in_band = 0;
        Ok(())
    }
}
