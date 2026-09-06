// SPDX-License-Identifier: GPL-2.0-only

//! # spl2-core — the SPL2 / QPDL v3 protocol engine
//!
//! Everything here is byte-for-byte critical: the golden corpus in `goldens/`
//! freezes what this crate emits, and `goldens/SHA256SUMS` pins it. The crate
//! is pure — no C, no global I/O, no dependencies — so the same engine serves
//! the frozen 1.x CUPS filter and the PAPPL printer application, and the two
//! cannot drift apart without a golden turning red.
//!
//! ## Layout
//!
//! * [`qpdl`] — the wire format itself: PJL envelope, the 17-byte page header,
//!   Algo 0x11 RLE band records and checksums. Moved unchanged from the 1.x
//!   filter's `src/spl.rs`.
//! * [`geometry`] — page validation and placement arithmetic, moved unchanged
//!   from the pure half of the 1.x filter's `src/main.rs`.
//! * [`engine`] — the seam the two front ends share: geometry in, QPDL page
//!   header and band records out.
//! * [`media`] — the driver's hard-margin constant.
//! * [`log`] — the diagnostics sink, because this crate may not own stderr.
//! * [`raster`] — the classic CUPS Raster parser, behind the non-default
//!   `golden-replay` feature (decision Q-6).
//!
//! ## Language note
//!
//! Doc comments and diagnostic strings moved from the 1.x filter are Turkish
//! and are kept verbatim: the filter is frozen (decision Q-5), so rewording a
//! message it emits would be a behaviour change rather than a translation.
//! Code written for the split is English, per decision Q-11.

#![forbid(unsafe_code)]

pub mod engine;
pub mod geometry;
pub mod log;
pub mod media;
pub mod qpdl;

#[cfg(feature = "golden-replay")]
pub mod raster;
