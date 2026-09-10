// SPDX-License-Identifier: MIT

//! Mainloop, driver registration and the raster callback boundary.
//!
//! This module owns the C side only. What a page turns into is a
//! [`RasterDriver`], supplied by the binary: the built-in [`GeometryProbe`]
//! writes diagnostic JSON Lines, and the ML-216x application supplies an
//! SPL2 encoder. The split is not cosmetic — decision Q-8a licenses this
//! crate `MIT` while the SPL2 engine is `GPL-2.0-only`, so the
//! protocol may not be linked in here. All C entry points use `guard`.

use crate::{guard, Device, Error, Job, Result};
use pappl_sys as sys;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::Write;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

static MAINLOOP: std::sync::Mutex<()> = std::sync::Mutex::new(());
static FAILED_JOB: u8 = 1;

/// PWG media dimensions in hundredths of a millimetre.
#[derive(Clone, Copy)]
pub struct Media {
    pub name: &'static CStr,
    pub width: i32,
    pub length: i32,
}

pub struct Capabilities {
    pub name: &'static CStr,
    pub description: &'static CStr,
    pub media: &'static [Media],
    /// Supported resolutions. **The order is load-bearing** — see
    /// [`quality_resolutions`].
    pub resolutions: &'static [(i32, i32)],
    pub sources: &'static [&'static CStr],
    pub types: &'static [&'static CStr],
    pub margin: i32,
    /// The resolution a job that names none must run at.
    ///
    /// Declared separately from `resolutions` so that [`Application::run`] can
    /// refuse to start when PAPPL's own choice would differ from it.
    pub default_resolution: (i32, i32),
}

/// What PAPPL picks for a job that carries no `printer-resolution`, by
/// print-quality: `[draft, normal, high]`.
///
/// This is not a guess about PAPPL; it is a transcription of
/// `papplJobCreatePrintOptions` in `pappl/job-process.c` of the 1.3.1 source
/// (`pappl 1.3.1-2.1`), which selects by index into the driver's resolution
/// list:
///
/// ```c
/// else if (options->print_quality == IPP_QUALITY_DRAFT)
///   options->printer_resolution[0] = printer->driver_data.x_resolution[0];
/// else if (options->print_quality == IPP_QUALITY_NORMAL)
///   i = (cups_len_t)printer->driver_data.num_resolution / 2;
/// else  // high
///   i = (cups_len_t)printer->driver_data.num_resolution - 1;
/// ```
///
/// Two consequences drove the way `Capabilities` is validated. `x_default` and
/// `y_default` are **not** consulted here — they are advertised as
/// `printer-resolution-default` and otherwise unused on the job path — so
/// declaring a 600 dpi default does not make a normal-quality job run at
/// 600 dpi. And the list order is the only thing that decides the mapping;
/// nothing else in PAPPL reads it (`printer-resolution-supported` and
/// `pwg-raster-document-resolution-supported` are sets).
///
/// This matters beyond page size. A client that pre-rendered `image/pwg-raster`
/// at one resolution and a job header built at another do not meet: for 1-bit
/// output PAPPL keeps its own header (it adopts the document's only when both
/// sides are >= 8 bits per pixel), pads each short line with white and appends
/// blank lines, and the driver is never told. The page then prints at the
/// wrong scale and the job still reports success. Keeping the normal-quality
/// entry equal to the declared default is what stops that for ordinary jobs.
pub fn quality_resolutions(resolutions: &[(i32, i32)]) -> Option<[(i32, i32); 3]> {
    Some([
        *resolutions.first()?,
        resolutions[resolutions.len() / 2],
        *resolutions.last()?,
    ])
}

pub struct Application {
    pub capabilities: Capabilities,
    /// What a validated page turns into.
    pub driver: Box<dyn RasterDriver>,
    /// Diagnostics mode: create a file-only printer and refuse any device URI
    /// that is not `file:///`, so a probe run cannot reach hardware.
    pub probe: bool,
    pub probe_output: Option<CString>,
    pub port: u16,
    pub spool_directory: CString,
}

/// The application currently running its mainloop.
///
/// `rwriteline_cb` fires once per scanline — 7015 times for A4 at 600 dpi — and
/// the only documented route from a job back to the driver extension is
/// `papplPrinterGetDriverData`, which copies 8728 bytes per call. Publishing
/// the pointer here instead keeps that off the per-scanline path. `run` holds
/// `MAINLOOP` for its whole body, so at most one application is ever published
/// and it outlives every callback that can observe it.
static ACTIVE: AtomicPtr<Application> = AtomicPtr::new(ptr::null_mut());

/// The application a callback belongs to.
fn active<'a>() -> Result<&'a Application> {
    let raw = ACTIVE.load(Ordering::Acquire);
    // SAFETY: `run` publishes `self` before `papplMainloop` and clears it
    // afterwards, holding `MAINLOOP` throughout; callbacks only run in between.
    unsafe { raw.cast_const().as_ref() }.ok_or(Error::NullPointer("application"))
}

/// One page's options, validated and copied out of the C struct.
///
/// The driver never sees `pappl_pr_options_t`: R-6 requires every field to be
/// checked at this boundary, and a driver that reads the raw struct could skip
/// that. Lengths are PWG units — hundredths of a millimetre — as PAPPL states
/// them in `pappl/printer.h`.
#[derive(Debug, Clone)]
pub struct RasterOptions {
    pub copies: i32,
    pub resolution: [i32; 2],
    pub media_name: String,
    /// `[width, length]` in hundredths of a millimetre.
    pub media_size: [i32; 2],
    /// `[left, right, top, bottom]` in hundredths of a millimetre.
    pub media_margins: [i32; 4],
    pub media_source: String,
    pub media_type: String,
    /// `cupsWidth`; meaningful only for raster callbacks.
    pub width: u32,
    /// `cupsHeight`.
    pub height: u32,
    /// `cupsBytesPerLine`.
    pub bytes_per_line: u32,
    /// The raster header's own `Margins`, which PAPPL leaves at zero on the
    /// BLACK_1 PWG path (`docs/P5-MEASUREMENTS.json`).
    pub header_margins: [u32; 2],
}

impl RasterOptions {
    /// Validates `raw` against the capability table and copies it out.
    fn checked(
        raw: &sys::pappl_pr_options_t,
        c: &Capabilities,
        raster: bool,
    ) -> Result<RasterOptions> {
        validate(raw, c, raster)?;
        Ok(RasterOptions {
            copies: raw.copies,
            resolution: raw.printer_resolution,
            media_name: text(&raw.media.size_name)?,
            media_size: [raw.media.size_width, raw.media.size_length],
            media_margins: [
                raw.media.left_margin,
                raw.media.right_margin,
                raw.media.top_margin,
                raw.media.bottom_margin,
            ],
            media_source: text(&raw.media.source)?,
            media_type: text(&raw.media.type_)?,
            width: raw.header.cupsWidth,
            height: raw.header.cupsHeight,
            bytes_per_line: raw.header.cupsBytesPerLine,
            header_margins: raw.header.Margins,
        })
    }
}

/// What the application does with a page.
///
/// One job runs at a time per printer, but PAPPL may run several printers, so
/// implementations keep their per-job state behind their own lock. Every method
/// returning `Err` fails the job: nothing on this path may fall back to a
/// plausible-looking default.
pub trait RasterDriver: Send + Sync {
    fn start_job(
        &self,
        job: &Job<'_>,
        options: &RasterOptions,
        device: &mut Device<'_>,
    ) -> Result<()>;
    fn start_page(
        &self,
        job: &Job<'_>,
        options: &RasterOptions,
        device: &mut Device<'_>,
        page: u32,
    ) -> Result<()>;
    fn write_line(
        &self,
        job: &Job<'_>,
        options: &RasterOptions,
        device: &mut Device<'_>,
        y: u32,
        line: &[u8],
    ) -> Result<()>;
    fn end_page(
        &self,
        job: &Job<'_>,
        options: &RasterOptions,
        device: &mut Device<'_>,
        page: u32,
    ) -> Result<()>;
    fn end_job(
        &self,
        job: &Job<'_>,
        options: &RasterOptions,
        device: &mut Device<'_>,
    ) -> Result<()>;

    /// Called when a job ends without `end_job` — PAPPL abandoning it, or an
    /// earlier callback failing. An implementation that keeps per-job state
    /// must drop it here, or the next job inherits it.
    fn abandon_job(&self, _job: &Job<'_>) {}
}

/// The P5 diagnostic driver: JSON Lines describing what PAPPL delivered.
///
/// This is the instrument that produced `docs/P5-MEASUREMENTS.json`, so its
/// output format is evidence and must not drift.
pub struct GeometryProbe;

impl RasterDriver for GeometryProbe {
    fn start_job(
        &self,
        _job: &Job<'_>,
        _options: &RasterOptions,
        device: &mut Device<'_>,
    ) -> Result<()> {
        device.write_all(b"{\"event\":\"job-start\",\"mode\":\"geometry-probe\"}\n")
    }

    fn start_page(
        &self,
        _job: &Job<'_>,
        o: &RasterOptions,
        device: &mut Device<'_>,
        page: u32,
    ) -> Result<()> {
        writeln!(device, "{{\"event\":\"page-start\",\"page\":{page},\"width\":{},\"height\":{},\"bytes_per_line\":{},\"dpi\":[{},{}],\"header_margins\":[{},{}],\"media\":[{},{}],\"margin\":{},\"copies\":{}}}",
            o.width, o.height, o.bytes_per_line, o.resolution[0], o.resolution[1],
            o.header_margins[0], o.header_margins[1], o.media_size[0], o.media_size[1],
            o.media_margins[0], o.copies)?;
        Ok(())
    }

    fn write_line(
        &self,
        _job: &Job<'_>,
        _options: &RasterOptions,
        device: &mut Device<'_>,
        y: u32,
        line: &[u8],
    ) -> Result<()> {
        let first = line.iter().position(|v| *v != 0).map_or(-1, |v| v as i32);
        let last = line.iter().rposition(|v| *v != 0).map_or(-1, |v| v as i32);
        let ones: u32 = line.iter().map(|v| v.count_ones()).sum();
        writeln!(device, "{{\"event\":\"line\",\"y\":{y},\"first_nonzero_byte\":{first},\"last_nonzero_byte\":{last},\"ones\":{ones}}}")?;
        Ok(())
    }

    fn end_page(
        &self,
        _job: &Job<'_>,
        _options: &RasterOptions,
        device: &mut Device<'_>,
        page: u32,
    ) -> Result<()> {
        writeln!(device, "{{\"event\":\"page-end\",\"page\":{page}}}")?;
        Ok(())
    }

    fn end_job(
        &self,
        _job: &Job<'_>,
        _options: &RasterOptions,
        device: &mut Device<'_>,
    ) -> Result<()> {
        device.write_all(b"{\"event\":\"job-end\"}\n")?;
        device.flush();
        Ok(())
    }
}

// papplSystemSetPrinterDrivers stores the pointer, it does NOT copy the array
// (PAPPL 1.3.1 system-accessors.c). Keep it in run(), alive through teardown.
struct RunContext<'a> {
    application: &'a Application,
    driver: *mut sys::pappl_pr_driver_t,
}

fn error(message: impl Into<String>) -> Error {
    Error::Driver(message.into())
}

fn copy<const N: usize>(target: &mut [c_char; N], value: &CStr) -> Result<()> {
    let bytes = value.to_bytes_with_nul();
    if bytes.len() > N {
        return Err(error("capability string exceeds PAPPL storage"));
    }
    target.fill(0);
    for (out, input) in target.iter_mut().zip(bytes) {
        *out = *input as c_char;
    }
    Ok(())
}

fn text<const N: usize>(value: &[c_char; N]) -> Result<String> {
    let end = value
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| error("option string is not NUL-terminated"))?;
    let bytes: Vec<u8> = value[..end].iter().map(|b| *b as u8).collect();
    String::from_utf8(bytes).map_err(|_| error("option string is not UTF-8"))
}

impl Application {
    pub fn run(&self, args: &[CString]) -> Result<i32> {
        let _mainloop = MAINLOOP
            .lock()
            .map_err(|_| error("mainloop lock poisoned"))?;
        let c = &self.capabilities;
        if c.media.is_empty()
            || c.media.len() > sys::PAPPL_MAX_MEDIA
            || c.resolutions.is_empty()
            || c.resolutions.len() > sys::PAPPL_MAX_RESOLUTION
            || c.sources.is_empty()
            || c.sources.len() > sys::PAPPL_MAX_SOURCE
            || c.types.is_empty()
            || c.types.len() > sys::PAPPL_MAX_TYPE
            || !c.resolutions.contains(&(600, 600))
            || c.resolutions
                .iter()
                .any(|&(x, y)| !(1..=1200).contains(&x) || !(1..=1200).contains(&y))
            || c.media.iter().any(|m| {
                !(1..=100000).contains(&m.width)
                    || !(1..=100000).contains(&m.length)
                    || i64::from(c.margin) * 2 >= i64::from(m.width.min(m.length))
            })
            || c.margin <= 0
            || self.port == 0
        {
            return Err(error("invalid application capabilities or listen port"));
        }
        // A job that names no resolution must land on the declared default.
        // PAPPL picks by position in the list, so this is a property of the
        // order, and getting it wrong prints every ordinary job at the wrong
        // scale without failing it. See `quality_resolutions`.
        if !c.resolutions.contains(&c.default_resolution)
            || quality_resolutions(c.resolutions).map(|q| q[1]) != Some(c.default_resolution)
        {
            return Err(error(
                "normal-quality jobs would not run at the default resolution: \
                 order the resolution list so its middle entry is the default",
            ));
        }
        // PAPPL accepts mutable argv; own writable NUL-terminated storage.
        let mut storage: Vec<Vec<u8>> = args
            .iter()
            .map(|s| s.as_bytes_with_nul().to_vec())
            .collect();
        let mut argv: Vec<*mut c_char> =
            storage.iter_mut().map(|s| s.as_mut_ptr().cast()).collect();
        let argc = i32::try_from(argv.len()).map_err(|_| error("too many arguments"))?;
        argv.push(ptr::null_mut());
        let mut driver = sys::pappl_pr_driver_t {
            name: c.name.as_ptr(),
            description: c.description.as_ptr(),
            device_id: ptr::null(),
            extension: ptr::null_mut(),
        };
        let mut context = RunContext {
            application: self,
            driver: &mut driver,
        };
        // Published for the raster callbacks; see `ACTIVE`. Cleared below so a
        // second `run` in the same process cannot observe a dead frame.
        ACTIVE.store((self as *const Application).cast_mut(), Ordering::Release);
        let _published = PublishedApplication;
        // SAFETY: all storage and self remain alive until the mainloop and its
        // job threads return. PAPPL owns/deletes the system returned below.
        Ok(unsafe {
            sys::papplMainloop(
                argc,
                argv.as_mut_ptr(),
                c"2.0.0-alpha".as_ptr(),
                ptr::null(),
                1,
                &mut driver,
                // Q-17: `autoadd_cb` stays null on purpose. A device ID is
                // self-reported and unauthenticated over both USB descriptor
                // strings and mDNS, so anything that adds a destination on its
                // own can be told what to be; the README already promises that
                // the person picks the URI. `devices` lists, `add -v` commits.
                None,
                Some(driver_cb),
                ptr::null(),
                None,
                Some(system_cb),
                None,
                (&mut context as *mut RunContext<'_>).cast(),
            )
        })
    }
}

/// Clears [`ACTIVE`] however `run` returns, including on a panic.
struct PublishedApplication;

impl Drop for PublishedApplication {
    fn drop(&mut self) {
        ACTIVE.store(ptr::null_mut(), Ordering::Release);
    }
}

unsafe extern "C" fn system_cb(
    _count: i32,
    _options: *mut sys::cups_option_t,
    data: *mut c_void,
) -> *mut sys::pappl_system_t {
    unsafe {
        guard(ptr::null_mut(), ptr::null_mut(), || {
            let context = data
                .cast::<RunContext<'_>>()
                .as_ref()
                .ok_or(Error::NullPointer("application context"))?;
            let app = context.application;
            let raw = sys::papplSystemCreate(
                sys::PAPPL_SOPTIONS_MULTI_QUEUE
                    | sys::PAPPL_SOPTIONS_WEB_INTERFACE
                    | sys::PAPPL_SOPTIONS_NO_TLS,
                c"ML-216x development".as_ptr(),
                i32::from(app.port),
                ptr::null(),
                app.spool_directory.as_ptr(),
                c"-".as_ptr(),
                sys::PAPPL_LOGLEVEL_DEBUG,
                ptr::null(),
                false,
            );
            let system = System(raw);
            if raw.is_null() {
                return Err(Error::NullPointer("system"));
            }
            if !sys::papplSystemAddListeners(raw, c"127.0.0.1".as_ptr()) {
                return Err(error("could not bind the loopback listener"));
            }
            // Q-15: a harness run persists nothing. PAPPL's mainloop otherwise
            // saves printers to `<base name>.state` and re-creates them at the
            // next startup, which was observed handing a printer created under
            // the geometry probe to the SPL2 driver — the probe's `file:///`
            // guard is only checked when the printer is created, not when it is
            // reloaded. Installing a save callback suppresses the mainloop's
            // state handling entirely, because it only installs its own
            // `if (!system->save_cb)`.
            //
            // The condition is `probe_output`, not `probe`: every development
            // run writes to a file destination, and one harness run must not
            // inherit the printers of the last. Scoping `XDG_CONFIG_HOME`, as
            // each script under `scripts/` does, is not enough, because PAPPL
            // consults it only for a non-root server — as root it writes
            // `/var/lib/<base name>.state` and every run shares it. That is
            // what turned CI's `harnesses` job red: `transport-probe.py
            // --inject truncate` reloaded the `sock` printer the plain run had
            // added and failed with "Printer name 'sock' already exists".
            // A shipped server never passes `--probe-output`, so its state is
            // persisted as before.
            if app.probe || app.probe_output.is_some() {
                sys::papplSystemSetSaveCallback(raw, Some(discard_state), ptr::null_mut());
            }
            // A file destination is how both drivers are exercised without
            // hardware: the probe writes JSON Lines to it, and the SPL2 driver
            // writes a stream that can be diffed against the golden corpus.
            if let Some(output) = &app.probe_output {
                let c = &app.capabilities;
                sys::papplSystemSetPrinterDrivers(
                    raw,
                    1,
                    context.driver,
                    None,
                    None,
                    Some(driver_cb),
                    data,
                );
                let uri = CString::new(format!(
                    "file://{}",
                    output
                        .to_str()
                        .map_err(|_| error("probe output path is not UTF-8"))?
                ))
                .map_err(|_| error("NUL in output URI"))?;
                if sys::papplPrinterCreate(
                    raw,
                    1,
                    c"probe".as_ptr(),
                    c.name.as_ptr(),
                    ptr::null(),
                    uri.as_ptr(),
                )
                .is_null()
                {
                    return Err(error("could not create the file-only probe printer"));
                }
            }
            // Ownership transfers to mainloop only after successful setup.
            std::mem::forget(system);
            Ok(raw)
        })
    }
}

/// A harness run's save callback: succeeds without writing anything.
///
/// PAPPL calls this whenever the system changes. Reporting success is correct
/// here — nothing failed, there is simply nowhere a run driving a file
/// destination should leave state. See Q-15 and the call site in `system_cb`.
unsafe extern "C" fn discard_state(_system: *mut sys::pappl_system_t, _data: *mut c_void) -> bool {
    unsafe { guard(ptr::null_mut(), false, || Ok(true)) }
}

struct System(*mut sys::pappl_system_t);
impl Drop for System {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                sys::papplSystemDelete(self.0);
            }
        }
    }
}

unsafe extern "C" fn driver_cb(
    _system: *mut sys::pappl_system_t,
    name: *const c_char,
    uri: *const c_char,
    _id: *const c_char,
    data: *mut sys::pappl_pr_driver_data_t,
    _attrs: *mut *mut sys::ipp_t,
    context: *mut c_void,
) -> bool {
    unsafe {
        guard(ptr::null_mut(), false, || {
            let app = context
                .cast::<RunContext<'_>>()
                .as_ref()
                .ok_or(Error::NullPointer("application context"))?
                .application;
            let data = data.as_mut().ok_or(Error::NullPointer("driver data"))?;
            if name.is_null() || uri.is_null() {
                return Err(Error::NullPointer("driver name or URI"));
            }
            let c = &app.capabilities;
            if CStr::from_ptr(name) != c.name {
                return Err(error("unknown driver"));
            }
            if app.probe && !CStr::from_ptr(uri).to_bytes().starts_with(b"file:///") {
                return Err(error("geometry probe requires a file:/// output URI"));
            }
            data.extension = (app as *const Application).cast_mut().cast();
            data.rstartjob_cb = Some(start_job);
            data.rstartpage_cb = Some(start_page);
            data.rwriteline_cb = Some(write_line);
            data.rendpage_cb = Some(end_page);
            data.rendjob_cb = Some(end_job);
            data.printfile_cb = Some(reject_raw);
            data.status_cb = Some(status);
            // Q-16: the printer-specific format is the probe instrument's, and
            // only the probe may advertise it. PAPPL publishes this string in
            // `document-format-supported` and in the `CMD:` list of the
            // IEEE-1284 device ID, so leaving it set for the SPL2 driver made
            // a real printer announce a diagnostic MIME type it would then
            // refuse. A null format is supported: `printer-driver.c` guards
            // every use of it, and raw jobs fall back to
            // `application/octet-stream`, which `reject_raw` still refuses.
            data.format = if app.probe {
                c"application/x-pappl-geometry-probe".as_ptr()
            } else {
                c"application/octet-stream".as_ptr()
            };
            copy(&mut data.make_and_model, c.description)?;
            data.ppm = 20;
            data.kind = sys::PAPPL_KIND_DOCUMENT | sys::PAPPL_KIND_ENVELOPE;
            data.orient_default = sys::IPP_ORIENT_NONE as i32;
            data.quality_default = sys::IPP_QUALITY_NORMAL as i32;
            data.color_supported = sys::PAPPL_COLOR_MODE_MONOCHROME;
            data.color_default = sys::PAPPL_COLOR_MODE_MONOCHROME;
            data.raster_types = sys::PAPPL_PWG_RASTER_TYPE_BLACK_1;
            data.force_raster_type = sys::PAPPL_PWG_RASTER_TYPE_BLACK_1;
            data.scaling_default = sys::PAPPL_SCALING_NONE;
            data.sides_supported = sys::PAPPL_SIDES_ONE_SIDED;
            data.sides_default = sys::PAPPL_SIDES_ONE_SIDED;
            data.duplex = sys::PAPPL_DUPLEX_NONE;
            data.num_resolution = c.resolutions.len() as i32;
            for (i, &(x, y)) in c.resolutions.iter().enumerate() {
                data.x_resolution[i] = x;
                data.y_resolution[i] = y;
            }
            data.x_default = c.default_resolution.0;
            data.y_default = c.default_resolution.1;
            data.left_right = c.margin;
            data.bottom_top = c.margin;
            data.borderless = false;
            data.num_media = c.media.len() as i32;
            for (out, m) in data.media.iter_mut().zip(c.media) {
                *out = m.name.as_ptr();
            }
            data.num_source = c.sources.len() as i32;
            for (out, source) in data.source.iter_mut().zip(c.sources) {
                *out = source.as_ptr();
            }
            data.num_type = c.types.len() as i32;
            for (out, kind) in data.type_.iter_mut().zip(c.types) {
                *out = kind.as_ptr();
            }
            for (ready, source) in data.media_ready.iter_mut().zip(c.sources) {
                let m = c.media.first().ok_or_else(|| error("no default medium"))?;
                ready.size_width = m.width;
                ready.size_length = m.length;
                ready.left_margin = c.margin;
                ready.right_margin = c.margin;
                ready.top_margin = c.margin;
                ready.bottom_margin = c.margin;
                copy(&mut ready.size_name, m.name)?;
                copy(&mut ready.source, source)?;
                copy(
                    &mut ready.type_,
                    c.types.first().ok_or_else(|| error("no media type"))?,
                )?;
            }
            // All fields are initialized by PAPPL and contain plain C data.
            data.media_default = ptr::read(&data.media_ready[0]);
            Ok(true)
        })
    }
}

/// R-6/H: reject invalid option fields; never clamp them into plausible values.
fn validate(options: &sys::pappl_pr_options_t, c: &Capabilities, raster: bool) -> Result<()> {
    macro_rules! require {
        ($condition:expr, $name:expr, $value:expr) => {
            if !$condition {
                return Err(error(format!("invalid {}: {:?}", $name, $value)));
            }
        };
    }
    require!(
        (1..=999).contains(&options.copies),
        "copies",
        options.copies
    );
    require!(
        c.resolutions
            .contains(&(options.printer_resolution[0], options.printer_resolution[1])),
        "printer_resolution",
        options.printer_resolution
    );
    let name = text(&options.media.size_name)?;
    let m = c
        .media
        .iter()
        .find(|m| m.name.to_bytes() == name.as_bytes())
        .ok_or_else(|| error(format!("invalid media.size_name: {name:?}")))?;
    require!(
        options.media.size_width == m.width && options.media.size_length == m.length,
        "media dimensions",
        (options.media.size_width, options.media.size_length)
    );
    for (name, value) in [
        ("left_margin", options.media.left_margin),
        ("right_margin", options.media.right_margin),
        ("top_margin", options.media.top_margin),
        ("bottom_margin", options.media.bottom_margin),
    ] {
        require!(value == c.margin, name, value);
    }
    require!(
        options.sides == sys::PAPPL_SIDES_ONE_SIDED,
        "sides",
        options.sides
    );
    let source = text(&options.media.source)?;
    require!(
        c.sources.iter().any(|s| s.to_bytes() == source.as_bytes()),
        "media.source",
        source
    );
    let kind = text(&options.media.type_)?;
    require!(
        c.types.iter().any(|s| s.to_bytes() == kind.as_bytes()),
        "media.type",
        kind
    );
    if raster {
        let h = &options.header;
        require!(
            h.HWResolution == options.printer_resolution.map(|v| v as u32),
            "header.HWResolution",
            h.HWResolution
        );
        require!(
            h.cupsBitsPerPixel == 1
                && h.cupsBitsPerColor == 1
                && h.cupsColorSpace == sys::CUPS_CSPACE_K
                && h.cupsColorOrder == sys::CUPS_ORDER_CHUNKED,
            "raster format",
            (
                h.cupsBitsPerPixel,
                h.cupsBitsPerColor,
                h.cupsColorSpace,
                h.cupsColorOrder
            )
        );
        let max_w = (i64::from(m.width) * i64::from(options.printer_resolution[0]) + 2539) / 2540;
        let max_h = (i64::from(m.length) * i64::from(options.printer_resolution[1]) + 2539) / 2540;
        require!(
            h.cupsWidth > 0 && i64::from(h.cupsWidth) <= max_w,
            "cupsWidth",
            h.cupsWidth
        );
        require!(
            h.cupsHeight > 0 && i64::from(h.cupsHeight) <= max_h,
            "cupsHeight",
            h.cupsHeight
        );
        require!(
            h.cupsBytesPerLine == h.cupsWidth.div_ceil(8),
            "cupsBytesPerLine",
            h.cupsBytesPerLine
        );
    }
    Ok(())
}

unsafe extern "C" fn start_job(
    job: *mut sys::pappl_job_t,
    options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        job_guard(job, || {
            let app = active()?;
            let raw = options.as_ref().ok_or(Error::NullPointer("options"))?;
            let o = RasterOptions::checked(raw, &app.capabilities, false)?;
            let job = Job::from_raw(job)?;
            app.driver
                .start_job(&job, &o, &mut Device::from_raw(device)?)?;
            Ok(true)
        })
    }
}

unsafe extern "C" fn start_page(
    job: *mut sys::pappl_job_t,
    options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
    page: u32,
) -> bool {
    unsafe {
        job_guard(job, || {
            let app = active()?;
            let raw = options.as_ref().ok_or(Error::NullPointer("options"))?;
            let o = RasterOptions::checked(raw, &app.capabilities, true)?;
            let job_handle = Job::from_raw(job)?;
            app.driver
                .start_page(&job_handle, &o, &mut Device::from_raw(device)?, page)?;
            Ok(true)
        })
    }
}

unsafe extern "C" fn write_line(
    job: *mut sys::pappl_job_t,
    options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
    y: u32,
    line: *const u8,
) -> bool {
    unsafe {
        job_guard(job, || {
            let job_handle = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Ok(false);
            }
            if job_handle.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let app = active()?;
            let raw = options.as_ref().ok_or(Error::NullPointer("options"))?;
            if line.is_null() {
                return Err(Error::NullPointer("raster line"));
            }
            // start_page validated the header. Recheck bounds before making a slice.
            if y >= raw.header.cupsHeight
                || raw.header.cupsBytesPerLine > 4096
                || raw.header.cupsBytesPerLine == 0
            {
                return Err(error(format!(
                    "invalid line bounds: y={y}, bytes={}",
                    raw.header.cupsBytesPerLine
                )));
            }
            let bytes = std::slice::from_raw_parts(line, raw.header.cupsBytesPerLine as usize);
            // Checked again per scanline rather than cached: R-6 is that no
            // unvalidated field reaches a driver, and PAPPL owns this struct
            // for the whole page. The cost is a handful of string compares
            // against the encoder's own work on the same line.
            let o = RasterOptions::checked(raw, &app.capabilities, true)?;
            app.driver
                .write_line(&job_handle, &o, &mut Device::from_raw(device)?, y, bytes)?;
            Ok(true)
        })
    }
}

unsafe extern "C" fn end_page(
    job: *mut sys::pappl_job_t,
    options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
    page: u32,
) -> bool {
    unsafe {
        job_guard(job, || {
            let job_handle = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Err(error("an earlier raster callback failed"));
            }
            let app = active()?;
            let raw = options.as_ref().ok_or(Error::NullPointer("options"))?;
            let o = RasterOptions::checked(raw, &app.capabilities, true)?;
            app.driver
                .end_page(&job_handle, &o, &mut Device::from_raw(device)?, page)?;
            // PAPPL 1.3.1 increments impressions itself in its PWG reader.
            Ok(true)
        })
    }
}
unsafe extern "C" fn end_job(
    job: *mut sys::pappl_job_t,
    options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        job_guard(job, || {
            let job_handle = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Err(error("an earlier raster callback failed"));
            }
            let app = active()?;
            let raw = options.as_ref().ok_or(Error::NullPointer("options"))?;
            let o = RasterOptions::checked(raw, &app.capabilities, false)?;
            app.driver
                .end_job(&job_handle, &o, &mut Device::from_raw(device)?)?;
            Ok(true)
        })
    }
}
unsafe extern "C" fn reject_raw(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    _device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        job_guard(job, || {
            Err(error(
                "raw printing is unavailable in the P5 geometry probe",
            ))
        })
    }
}
unsafe extern "C" fn status(_printer: *mut sys::pappl_printer_t) -> bool {
    unsafe { guard(ptr::null_mut(), false, || Ok(true)) }
}

// PAPPL 1.3.1 ignores rwriteline's return value. Remember failure without
// allocating job state, then fail rendpage/rendjob (which it DOES check).
// The marker is never dereferenced and the static outlives every job.
unsafe fn job_guard<F>(job: *mut sys::pappl_job_t, body: F) -> bool
where
    F: FnOnce() -> Result<bool>,
{
    let success = unsafe { guard(job, false, body) };
    if !success && !job.is_null() {
        unsafe {
            sys::papplJobSetData(job, (&FAILED_JOB as *const u8).cast_mut().cast());
            sys::papplJobSetReasons(
                job,
                sys::PAPPL_JREASON_ERRORS_DETECTED,
                sys::PAPPL_JREASON_NONE,
            );
        }
        // The job is over; a driver holding state for it will never see
        // `end_job`, and state left behind would follow the next job.
        if let (Ok(app), Ok(handle)) = (active(), unsafe { Job::from_raw(job) }) {
            app.driver.abandon_job(&handle);
        }
    }
    success
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities() -> Capabilities {
        Capabilities {
            name: c"test",
            description: c"test",
            media: &[Media {
                name: c"iso_a4_210x297mm",
                width: 21000,
                length: 29700,
            }],
            resolutions: &[(300, 300), (1200, 600), (600, 600), (1200, 1200)],
            sources: &[c"auto"],
            types: &[c"auto"],
            margin: 441,
            default_resolution: (600, 600),
        }
    }

    #[test]
    fn a_job_that_names_no_resolution_lands_on_the_declared_default() {
        let c = capabilities();
        let q = quality_resolutions(c.resolutions).unwrap();
        assert_eq!(q[1], c.default_resolution, "normal quality");
        assert_eq!(q[0], (300, 300), "draft quality");
        assert_eq!(q[2], (1200, 1200), "high quality");
    }

    #[test]
    fn a_resolution_list_whose_middle_is_not_the_default_is_refused() {
        // The order this replaces is the one that shipped through P6, and it
        // is what sent a 600 dpi document through a 1200x600 job (Q-14).
        struct Never;
        impl RasterDriver for Never {
            fn start_job(&self, _: &Job<'_>, _: &RasterOptions, _: &mut Device<'_>) -> Result<()> {
                unreachable!()
            }
            fn start_page(
                &self,
                _: &Job<'_>,
                _: &RasterOptions,
                _: &mut Device<'_>,
                _: u32,
            ) -> Result<()> {
                unreachable!()
            }
            fn write_line(
                &self,
                _: &Job<'_>,
                _: &RasterOptions,
                _: &mut Device<'_>,
                _: u32,
                _: &[u8],
            ) -> Result<()> {
                unreachable!()
            }
            fn end_page(
                &self,
                _: &Job<'_>,
                _: &RasterOptions,
                _: &mut Device<'_>,
                _: u32,
            ) -> Result<()> {
                unreachable!()
            }
            fn end_job(&self, _: &Job<'_>, _: &RasterOptions, _: &mut Device<'_>) -> Result<()> {
                unreachable!()
            }
        }
        let mut c = capabilities();
        c.resolutions = &[(300, 300), (600, 600), (1200, 600), (1200, 1200)];
        let app = Application {
            capabilities: c,
            driver: Box::new(Never),
            probe: false,
            probe_output: None,
            port: 8631,
            spool_directory: CString::new("/tmp").unwrap(),
        };
        // Rejected before the mainloop is entered, so no server is started.
        let message = app.run(&[]).unwrap_err().to_string();
        assert!(message.contains("normal-quality"), "{message}");
    }
    fn options() -> sys::pappl_pr_options_t {
        // Plain C fields, zero is a valid initialized representation.
        let mut o: sys::pappl_pr_options_t = unsafe { std::mem::zeroed() };
        o.copies = 1;
        o.printer_resolution = [600, 600];
        o.media.size_width = 21000;
        o.media.size_length = 29700;
        o.media.left_margin = 441;
        o.media.right_margin = 441;
        o.media.top_margin = 441;
        o.media.bottom_margin = 441;
        copy(&mut o.media.size_name, c"iso_a4_210x297mm").unwrap();
        copy(&mut o.media.source, c"auto").unwrap();
        copy(&mut o.media.type_, c"auto").unwrap();
        o.sides = sys::PAPPL_SIDES_ONE_SIDED;
        o.header.HWResolution = [600, 600];
        o.header.cupsWidth = 4960;
        o.header.cupsHeight = 7015;
        o.header.cupsBytesPerLine = 620;
        o.header.cupsBitsPerPixel = 1;
        o.header.cupsBitsPerColor = 1;
        o.header.cupsColorSpace = sys::CUPS_CSPACE_K;
        o
    }
    #[test]
    fn full_media_with_zero_header_margins_is_valid() {
        assert!(validate(&options(), &capabilities(), true).is_ok());
    }
    #[test]
    fn implausible_options_fail_with_field_and_value_without_clamping() {
        for copies in [0, -1, 1000, i32::MAX] {
            let mut o = options();
            o.copies = copies;
            let message = validate(&o, &capabilities(), true).unwrap_err().to_string();
            assert!(message.contains("copies") && message.contains(&copies.to_string()));
            assert_eq!(o.copies, copies);
        }
        let mut o = options();
        o.printer_resolution = [600, 1200];
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("printer_resolution"));
        let mut o = options();
        o.media.size_length = 0;
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("media dimensions"));
        let mut o = options();
        o.media.left_margin = 0;
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("left_margin"));
        let mut o = options();
        o.header.cupsBytesPerLine = 619;
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("cupsBytesPerLine"));
        let mut o = options();
        o.header.cupsHeight = u32::MAX;
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("cupsHeight"));
    }
    #[test]
    fn unterminated_strings_are_rejected_without_reading_past_the_array() {
        let mut o = options();
        o.media.size_name.fill(b'x' as c_char);
        assert!(validate(&o, &capabilities(), true)
            .unwrap_err()
            .to_string()
            .contains("NUL-terminated"));
    }
    #[test]
    fn geometry_probe_cannot_register_a_hardware_destination() {
        let app = Application {
            capabilities: capabilities(),
            driver: Box::new(GeometryProbe),
            probe: true,
            probe_output: None,
            port: 8631,
            spool_directory: CString::new("/tmp").unwrap(),
        };
        let mut data: sys::pappl_pr_driver_data_t = unsafe { std::mem::zeroed() };
        let mut context = RunContext {
            application: &app,
            driver: ptr::null_mut(),
        };
        for uri in [c"usb://Samsung/ML-2160", c"socket://127.0.0.1:9100"] {
            assert!(!unsafe {
                driver_cb(
                    ptr::null_mut(),
                    c"test".as_ptr(),
                    uri.as_ptr(),
                    ptr::null(),
                    &mut data,
                    ptr::null_mut(),
                    (&mut context as *mut RunContext<'_>).cast(),
                )
            });
        }
    }
}
