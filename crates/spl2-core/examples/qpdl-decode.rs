// SPDX-License-Identifier: GPL-2.0-only

//! Decode an SPL2/QPDL stream back into page bitmaps.
//!
//! Usage: `qpdl-decode <stream.spl> <out-dir>`
//!
//! Writes `page-<n>.pbm` (raw P4, 1 = toner) per page into the output
//! directory and prints a JSON summary of every page header on stdout.
//!
//! This exists for release gate G-1 (`docs/G1-MEASUREMENT.md`): the gate is a
//! physical measurement, and before anyone measures a sheet of paper the page
//! that was sent has to be the page that was meant. Reading the stream back
//! into a bitmap is what lets `scripts/g1-probe.py` assert that every ruler
//! tick landed on the band column the geometry predicts.
//!
//! It is deliberately built from this crate's own encoder rather than from a
//! description of the format: [`Algo0x11::decompress`] is the inverse the
//! encoder is tested against, and the record layout below is read off
//! `SplStreamWriter::begin_page` and `write_compressed_band`. Nothing here is
//! reconstructed from outside the repository.
//!
//! `Algo0x11::decompress` is gated on `golden-replay` for the reason decision
//! Q-6 gives, so this example is too.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use spl2_core::geometry::band_height_for;
use spl2_core::qpdl::{Algo0x11, PJL_END, PJL_UEL, SUBHEADER_SIG_LE};

/// Where the QPDL records start.
const ENTER_QPDL: &[u8] = b"@PJL ENTER LANGUAGE = QPDL\n";

/// The record signatures `SplStreamWriter` emits.
const PAGE_HEADER: u8 = 0x00;
const BAND_RECORD: u8 = 0x0C;
const PAGE_FOOTER: u8 = 0x01;

struct Page {
    /// The band width in pixels, which is the page width QPDL carries
    /// (`PageSetup::new`: `width_pixels: band_width_u16`).
    width: usize,
    height: usize,
    x_dpi: u32,
    y_dpi: u32,
    copies: u16,
    paper_size: u8,
    band_height: usize,
    bands: usize,
    /// One byte per pixel, 1 = toner, row major, `width * height`.
    pixels: Vec<u8>,
}

fn bad(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn be16(bytes: &[u8]) -> usize {
    ((bytes[0] as usize) << 8) | bytes[1] as usize
}

fn decode(stream: &[u8]) -> io::Result<Vec<Page>> {
    if !stream.starts_with(PJL_UEL) {
        return Err(bad("the stream does not open with the UEL"));
    }
    if !stream.ends_with(PJL_END) {
        return Err(bad("the stream does not close with a TAB and the UEL"));
    }
    let start = stream
        .windows(ENTER_QPDL.len())
        .position(|window| window == ENTER_QPDL)
        .ok_or_else(|| bad("no `@PJL ENTER LANGUAGE = QPDL` in the PJL envelope"))?
        + ENTER_QPDL.len();

    let mut pages: Vec<Page> = Vec::new();
    let mut at = start;
    while at < stream.len() {
        match stream[at] {
            PAGE_HEADER => {
                let header = stream
                    .get(at..at + 17)
                    .ok_or_else(|| bad("truncated page header"))?;
                // The field map is `SplStreamWriter::begin_page`.
                let band_height =
                    band_height_for([header[0x10] as u32 * 100, header[0x1] as u32 * 100]);
                pages.push(Page {
                    width: be16(&header[0x5..0x7]),
                    height: be16(&header[0x7..0x9]),
                    x_dpi: header[0x10] as u32 * 100,
                    y_dpi: header[0x1] as u32 * 100,
                    copies: ((header[0x2] as u16) << 8) | header[0x3] as u16,
                    paper_size: header[0x4],
                    band_height,
                    bands: 0,
                    pixels: Vec::new(),
                });
                at += 17;
            }
            BAND_RECORD => {
                let header = stream
                    .get(at..at + 11)
                    .ok_or_else(|| bad("truncated band header"))?;
                let page = pages
                    .last_mut()
                    .ok_or_else(|| bad("a band before any page"))?;
                let width_pixels = be16(&header[0x2..0x4]);
                let lines = be16(&header[0x4..0x6]);
                if header[0x6] != 0x11 {
                    return Err(bad(format!(
                        "band compression {:#04x} is not Algo 0x11",
                        header[0x6]
                    )));
                }
                if header[0x1] as usize != page.bands {
                    return Err(bad(format!(
                        "band order {} arrived where {} was expected",
                        header[0x1], page.bands
                    )));
                }
                if width_pixels != page.width {
                    return Err(bad(format!(
                        "band is {width_pixels} px wide, the page header says {}",
                        page.width
                    )));
                }
                let total = u32::from_be_bytes([header[0x7], header[0x8], header[0x9], header[0xA]])
                    as usize;
                if total < 8 {
                    return Err(bad("band data size does not cover its own signature"));
                }
                let body = stream
                    .get(at + 11..at + 11 + total)
                    .ok_or_else(|| bad("truncated band payload"))?;
                if body[..4] != SUBHEADER_SIG_LE {
                    return Err(bad("band sub-header signature is wrong"));
                }
                let payload = &body[4..total - 4];
                let want = u32::from_be_bytes([
                    body[total - 4],
                    body[total - 3],
                    body[total - 2],
                    body[total - 1],
                ]);
                let found = Algo0x11::calculate_checksum(&SUBHEADER_SIG_LE)
                    .wrapping_add(Algo0x11::calculate_checksum(payload));
                if found != want {
                    return Err(bad(format!(
                        "band checksum is {found:#010x}, expected {want:#010x}"
                    )));
                }

                let band_bytes = width_pixels / 8;
                let raw = Algo0x11::decompress(payload);
                if raw.len() != band_bytes * lines {
                    return Err(bad(format!(
                        "band decompressed to {} bytes, expected {} ({band_bytes} x {lines})",
                        raw.len(),
                        band_bytes * lines
                    )));
                }
                // `BandEncoder::flush` fills the buffer column-major and
                // inverts it: the engine reads 0 as toner, CUPS K reads 1.
                for y in 0..lines {
                    for column in 0..band_bytes {
                        let byte = !raw[column * lines + y];
                        for bit in 0..8 {
                            page.pixels.push((byte >> (7 - bit)) & 1);
                        }
                    }
                }
                page.bands += 1;
                at += 11 + total;
            }
            PAGE_FOOTER => at += 3,
            _ => {
                if stream[at..] == *PJL_END {
                    break;
                }
                return Err(bad(format!(
                    "unknown record {:#04x} at offset {at}",
                    stream[at]
                )));
            }
        }
    }

    for (index, page) in pages.iter_mut().enumerate() {
        let declared = page.width * page.height;
        // The last band is emitted at the full band height even when the page
        // ends inside it (`BandEncoder::finish`), so the decoded bitmap is at
        // least as tall as the page and is cut back to what the header says.
        if page.pixels.len() < declared {
            return Err(bad(format!(
                "page {} decoded to {} pixels, its header declares {declared}",
                index + 1,
                page.pixels.len()
            )));
        }
        page.pixels.truncate(declared);
    }
    Ok(pages)
}

fn write_pbm(path: &Path, page: &Page) -> io::Result<()> {
    let mut out = Vec::with_capacity(page.width / 8 * page.height + 32);
    out.extend_from_slice(format!("P4\n{} {}\n", page.width, page.height).as_bytes());
    for row in page.pixels.chunks(page.width) {
        for byte in row.chunks(8) {
            let mut packed = 0u8;
            for (bit, &pixel) in byte.iter().enumerate() {
                packed |= pixel << (7 - bit);
            }
            out.push(packed);
        }
    }
    fs::write(path, out)
}

fn main() -> io::Result<()> {
    let arguments: Vec<String> = env::args().collect();
    if arguments.len() != 3 {
        eprintln!("usage: {} <stream.spl> <out-dir>", arguments[0]);
        std::process::exit(2);
    }
    let stream = fs::read(&arguments[1])?;
    let directory = Path::new(&arguments[2]);
    fs::create_dir_all(directory)?;
    let pages = decode(&stream)?;

    let mut json = String::from("{\n  \"bytes\": ");
    json.push_str(&stream.len().to_string());
    json.push_str(",\n  \"pages\": [\n");
    for (index, page) in pages.iter().enumerate() {
        let name = format!("page-{}.pbm", index + 1);
        write_pbm(&directory.join(&name), page)?;
        json.push_str(&format!(
            "    {{\"page\": {}, \"width\": {}, \"height\": {}, \"resolution\": [{}, {}], \
             \"copies\": {}, \"paper_size\": {}, \"band_height\": {}, \"bands\": {}, \
             \"pbm\": \"{}\"}}{}\n",
            index + 1,
            page.width,
            page.height,
            page.x_dpi,
            page.y_dpi,
            page.copies,
            page.paper_size,
            page.band_height,
            page.bands,
            name,
            if index + 1 == pages.len() { "" } else { "," }
        ));
    }
    json.push_str("  ]\n}\n");
    io::stdout().write_all(json.as_bytes())
}
