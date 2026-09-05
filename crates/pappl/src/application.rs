// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal mainloop and a file-only raster geometry probe. This deliberately
//! emits diagnostic JSON Lines, not a printer language. Actual print jobs are
//! refused until an encoder is connected. All C entry points use `guard`.

use crate::{guard, Device, Error, Job, Result};
use pappl_sys as sys;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::Write;
use std::ptr;

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
    pub resolutions: &'static [(i32, i32)],
    pub sources: &'static [&'static CStr],
    pub types: &'static [&'static CStr],
    pub margin: i32,
}

pub struct Application {
    pub capabilities: Capabilities,
    pub probe: bool,
    pub probe_output: Option<CString>,
    pub port: u16,
    pub spool_directory: CString,
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
            if let Some(output) = &app.probe_output {
                if !app.probe {
                    return Err(error("--probe-output requires --probe"));
                }
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
            data.format = c"application/x-pappl-geometry-probe".as_ptr();
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
            data.x_default = 600;
            data.y_default = 600;
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

// The returned borrow never leaves a callback; the Application outlives mainloop.
unsafe fn application<'a>(job: *mut sys::pappl_job_t) -> Result<&'a Application> {
    if job.is_null() {
        return Err(Error::NullPointer("job"));
    }
    unsafe {
        let printer = sys::papplJobGetPrinter(job);
        if printer.is_null() {
            return Err(Error::NullPointer("printer"));
        }
        let mut data: sys::pappl_pr_driver_data_t = std::mem::zeroed();
        if sys::papplPrinterGetDriverData(printer, &mut data).is_null() {
            return Err(Error::NullPointer("driver data"));
        }
        data.extension
            .cast::<Application>()
            .as_ref()
            .ok_or(Error::NullPointer("application"))
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
            let app = application(job)?;
            if !app.probe {
                return Err(error("SPL2 callbacks are not connected yet; use --probe with a file URI for P5 diagnostics"));
            }
            let o = options.as_ref().ok_or(Error::NullPointer("options"))?;
            validate(o, &app.capabilities, false)?;
            Device::from_raw(device)?
                .write_all(b"{\"event\":\"job-start\",\"mode\":\"geometry-probe\"}\n")?;
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
            let app = application(job)?;
            let o = options.as_ref().ok_or(Error::NullPointer("options"))?;
            validate(o, &app.capabilities, true)?;
            let h = &o.header;
            let mut device = Device::from_raw(device)?;
            writeln!(device, "{{\"event\":\"page-start\",\"page\":{page},\"width\":{},\"height\":{},\"bytes_per_line\":{},\"dpi\":[{},{}],\"header_margins\":[{},{}],\"media\":[{},{}],\"margin\":{},\"copies\":{}}}",
            h.cupsWidth,h.cupsHeight,h.cupsBytesPerLine,h.HWResolution[0],h.HWResolution[1],h.Margins[0],h.Margins[1],o.media.size_width,o.media.size_length,o.media.left_margin,o.copies)?;
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
            let _job = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Ok(false);
            }
            if Job::from_raw(job)?.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let o = options.as_ref().ok_or(Error::NullPointer("options"))?;
            if line.is_null() {
                return Err(Error::NullPointer("raster line"));
            }
            // start_page validated the header. Recheck bounds before making a slice.
            if y >= o.header.cupsHeight
                || o.header.cupsBytesPerLine > 4096
                || o.header.cupsBytesPerLine == 0
            {
                return Err(error(format!(
                    "invalid line bounds: y={y}, bytes={}",
                    o.header.cupsBytesPerLine
                )));
            }
            let bytes = std::slice::from_raw_parts(line, o.header.cupsBytesPerLine as usize);
            let first = bytes.iter().position(|v| *v != 0).map_or(-1, |v| v as i32);
            let last = bytes.iter().rposition(|v| *v != 0).map_or(-1, |v| v as i32);
            let ones: u32 = bytes.iter().map(|v| v.count_ones()).sum();
            let mut device = Device::from_raw(device)?;
            writeln!(device,"{{\"event\":\"line\",\"y\":{y},\"first_nonzero_byte\":{first},\"last_nonzero_byte\":{last},\"ones\":{ones}}}")?;
            Ok(true)
        })
    }
}

unsafe extern "C" fn end_page(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
    page: u32,
) -> bool {
    unsafe {
        job_guard(job, || {
            let _job = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Err(error("an earlier raster callback failed"));
            }
            let mut device = Device::from_raw(device)?;
            writeln!(device, "{{\"event\":\"page-end\",\"page\":{page}}}")?;
            // PAPPL 1.3.1 increments impressions itself in its PWG reader.
            Ok(true)
        })
    }
}
unsafe extern "C" fn end_job(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        job_guard(job, || {
            let _job = Job::from_raw(job)?;
            if !sys::papplJobGetData(job).is_null() {
                return Err(error("an earlier raster callback failed"));
            }
            let mut device = Device::from_raw(device)?;
            device.write_all(b"{\"event\":\"job-end\"}\n")?;
            device.flush();
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
            resolutions: &[(300, 300), (600, 600), (1200, 600), (1200, 1200)],
            sources: &[c"auto"],
            types: &[c"auto"],
            margin: 441,
        }
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
