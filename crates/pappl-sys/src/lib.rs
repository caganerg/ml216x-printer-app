// SPDX-License-Identifier: MIT

//! Raw FFI declarations for libpappl 1.3.
//!
//! This crate is the unsafe boundary and nothing more: `#[repr(C)]` types,
//! function declarations, and the integer constants the C enums define. There
//! is no safety, no RAII and no abstraction here — those belong to the `pappl`
//! wrapper crate, which also owns the `catch_unwind` shim that every callback
//! must pass through (project rule 5: unwinding across `extern "C"` is
//! undefined behaviour).
//!
//! # Every declaration is transcribed from the installed headers
//!
//! Project rule 2 forbids inventing PAPPL signatures. Every item below carries
//! the real declaration from `/usr/include/pappl/*.h` (PAPPL 1.3.1, Debian
//! trixie `libpappl-dev` 1.3.1-2.1+b2) in its doc comment, so a reviewer can
//! compare the two without opening the header.
//!
//! Because the bindings are hand written rather than generated, nothing checks
//! the transcription except a program compiled against the same headers:
//!
//! * `probe/layout_probe.c` prints the size and alignment of every type
//!   declared here, the offset and size of every field, and the value of every
//!   enum constant.
//! * `tests/layout.rs` asserts the Rust side against that output, and fails if
//!   any probed record is left unchecked — every field, not a sample.
//! * `tests/symbols.rs` asserts that every function declared here is actually
//!   exported by the installed `libpappl`.
//!
//! A libpappl upgrade that moves a field therefore breaks the build instead of
//! silently corrupting memory.
//!
//! # Version
//!
//! Only symbols present in the 1.3 headers are bound (decision Q-1); the build
//! script enforces `>= 1.3, < 2.0`. See `docs/PAPPL-SYMBOLS.md` for the table
//! asserting that nothing bound here is newer than 1.3.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use core::ffi::{c_char, c_int, c_uchar, c_uint, c_ushort, c_void};

// ============================================================================
// Opaque objects — `base.h`
// ============================================================================
//
// ```c
// typedef struct _pappl_client_s pappl_client_t;
// typedef struct _pappl_device_s pappl_device_t;
// typedef struct _pappl_job_s pappl_job_t;
// typedef struct _pappl_loc_s pappl_loc_t;
// typedef struct _pappl_printer_s pappl_printer_t;
// typedef struct _pappl_subscription_s pappl_subscription_t;
// typedef struct _pappl_system_s pappl_system_t;
// ```
//
// The definitions are private to libpappl, so these are declared as opaque
// types that can only be handled behind a pointer.

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {
        $(
            #[repr(C)]
            pub struct $name {
                _data: [u8; 0],
                _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
            }
        )*
    };
}

opaque!(
    pappl_client_t,
    pappl_device_t,
    pappl_job_t,
    pappl_loc_t,
    pappl_printer_t,
    pappl_subscription_t,
    pappl_system_t,
);

/// `cups_option_t` from `<cups/cups.h>`.
///
/// Declared opaque deliberately. PAPPL only ever hands us pointers to these
/// (vendor options, mainloop options), and binding its fields means
/// transcribing a CUPS header as carefully as a PAPPL one — with its own probe
/// entries. Until a caller actually needs to read an option, an opaque pointer
/// is both sufficient and honest.
#[repr(C)]
pub struct cups_option_t {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

/// In-memory CUPS 2.4 `cups_page_header2_t`, transcribed from
/// `/usr/include/cups/raster.h`. CUPS' nonnegative enum fields use unsigned
/// storage; every field offset and size is checked by the C layout probe.
/// This is native-endian memory, not a raster stream header.
#[repr(C)]
#[allow(non_snake_case)]
#[derive(Clone, Copy)]
pub struct cups_page_header2_t {
    pub MediaClass: [c_char; 64],
    pub MediaColor: [c_char; 64],
    pub MediaType: [c_char; 64],
    pub OutputType: [c_char; 64],
    pub AdvanceDistance: c_uint,
    pub AdvanceMedia: c_uint,
    pub Collate: c_uint,
    pub CutMedia: c_uint,
    pub Duplex: c_uint,
    pub HWResolution: [c_uint; 2],
    pub ImagingBoundingBox: [c_uint; 4],
    pub InsertSheet: c_uint,
    pub Jog: c_uint,
    pub LeadingEdge: c_uint,
    pub Margins: [c_uint; 2],
    pub ManualFeed: c_uint,
    pub MediaPosition: c_uint,
    pub MediaWeight: c_uint,
    pub MirrorPrint: c_uint,
    pub NegativePrint: c_uint,
    pub NumCopies: c_uint,
    pub Orientation: c_uint,
    pub OutputFaceUp: c_uint,
    pub PageSize: [c_uint; 2],
    pub Separations: c_uint,
    pub TraySwitch: c_uint,
    pub Tumble: c_uint,
    pub cupsWidth: c_uint,
    pub cupsHeight: c_uint,
    pub cupsMediaType: c_uint,
    pub cupsBitsPerColor: c_uint,
    pub cupsBitsPerPixel: c_uint,
    pub cupsBytesPerLine: c_uint,
    pub cupsColorOrder: c_uint,
    pub cupsColorSpace: c_uint,
    pub cupsCompression: c_uint,
    pub cupsRowCount: c_uint,
    pub cupsRowFeed: c_uint,
    pub cupsRowStep: c_uint,
    pub cupsNumColors: c_uint,
    pub cupsBorderlessScalingFactor: f32,
    pub cupsPageSize: [f32; 2],
    pub cupsImagingBBox: [f32; 4],
    pub cupsInteger: [c_uint; 16],
    pub cupsReal: [f32; 16],
    pub cupsString: [[c_char; 64]; 16],
    pub cupsMarkerType: [c_char; 64],
    pub cupsRenderingIntent: [c_char; 64],
    pub cupsPageSizeName: [c_char; 64],
}

/// Size of `cups_page_header2_t` as measured against CUPS
/// [`CUPS_ABI_MAJOR`].[`CUPS_ABI_MINOR`].
///
/// Hardcoded here and checked by `tests/layout.rs` against the probe.
pub const CUPS_PAGE_HEADER2_SIZE: usize = 1796;

/// The CUPS major version this crate's `cups_page_header2_t` was measured
/// against. See risk R-6: the struct belongs to CUPS, not PAPPL, and PAPPL
/// embeds it by value at the start of `pappl_pr_options_t`, so a libcups
/// release that changed its layout would move every field after it — without
/// changing any symbol, and therefore without changing any dependency the
/// linker can record.
pub const CUPS_ABI_MAJOR: u32 = 2;
/// See [`CUPS_ABI_MAJOR`].
pub const CUPS_ABI_MINOR: u32 = 4;

// ============================================================================
// Scalar typedefs — `base.h`, `printer.h`, `device.h`, `job.h`, `system.h`
// ============================================================================

/// ```c
/// typedef unsigned char pappl_dither_t[16][16];   // 16x16 dither array
/// ```
pub type pappl_dither_t = [[c_uchar; 16]; 16];

/// ```c
/// typedef unsigned pappl_color_mode_t;    // Bitfield for IPP "print-color-mode" values
/// ```
pub type pappl_color_mode_t = c_uint;
/// ```c
/// typedef unsigned pappl_content_t;       // Bitfield for IPP "print-content-optimize" values
/// ```
pub type pappl_content_t = c_uint;
/// ```c
/// typedef unsigned pappl_finishings_t;    // Bitfield for IPP "finishings" values
/// ```
pub type pappl_finishings_t = c_uint;
/// ```c
/// typedef unsigned pappl_identify_actions_t;
/// ```
pub type pappl_identify_actions_t = c_uint;
/// ```c
/// typedef unsigned pappl_kind_t;          // Bitfield for IPP "printer-kind" values
/// ```
pub type pappl_kind_t = c_uint;
/// ```c
/// typedef unsigned short pappl_label_mode_t;
/// ```
pub type pappl_label_mode_t = c_ushort;
/// ```c
/// typedef unsigned short pappl_media_tracking_t;
/// ```
pub type pappl_media_tracking_t = c_ushort;
/// ```c
/// typedef unsigned int pappl_preason_t;   // Bitfield for IPP "printer-state-reasons" values
/// ```
pub type pappl_preason_t = c_uint;
/// ```c
/// typedef unsigned pappl_raster_type_t;
/// ```
pub type pappl_raster_type_t = c_uint;
/// ```c
/// typedef unsigned pappl_scaling_t;       // Bitfield for IPP "print-scaling" values
/// ```
pub type pappl_scaling_t = c_uint;
/// ```c
/// typedef unsigned pappl_sides_t;         // Bitfield for IPP "sides" values
/// ```
pub type pappl_sides_t = c_uint;
/// ```c
/// typedef unsigned pappl_soptions_t;      // Bitfield for system options
/// ```
pub type pappl_soptions_t = c_uint;
/// ```c
/// typedef unsigned pappl_devtype_t;       // Device type bitfield
/// ```
pub type pappl_devtype_t = c_uint;
/// ```c
/// typedef unsigned int pappl_jreason_t;   // Bitfield for IPP "job-state-reasons" values
/// ```
pub type pappl_jreason_t = c_uint;
/// ```c
/// typedef unsigned short pappl_loptions_t;// Bitfield for link options
/// ```
pub type pappl_loptions_t = c_ushort;

/// ```c
/// typedef enum pappl_duplex_e { ... } pappl_duplex_t;
/// ```
///
/// A C enum with only non-negative values; GCC gives it `int` layout here, and
/// the probe checks that assumption through the struct fields that use it.
pub type pappl_duplex_t = c_int;
/// ```c
/// typedef enum pappl_loglevel_e { PAPPL_LOGLEVEL_UNSPEC = -1, ... } pappl_loglevel_t;
/// ```
pub type pappl_loglevel_t = c_int;
/// ```c
/// typedef enum pappl_supply_color_e { ... } pappl_supply_color_t;
/// ```
pub type pappl_supply_color_t = c_int;
/// ```c
/// typedef enum pappl_supply_type_e { ... } pappl_supply_type_t;
/// ```
pub type pappl_supply_type_t = c_int;

/// `ipp_orient_t`, `ipp_quality_t`, `ipp_jstate_t` and `ipp_pstate_t` from
/// `<cups/ipp.h>` are C enums used by value in the structs below.
pub type ipp_orient_t = c_int;
/// See [`ipp_orient_t`].
pub type ipp_quality_t = c_int;
/// See [`ipp_orient_t`].
pub type ipp_jstate_t = c_int;
/// See [`ipp_orient_t`].
pub type ipp_pstate_t = c_int;
/// `ipp_t` from `<cups/ipp.h>`, always handled behind a pointer.
#[repr(C)]
pub struct ipp_t {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}
/// `ipp_attribute_t` from `<cups/ipp.h>`, always handled behind a pointer.
#[repr(C)]
pub struct ipp_attribute_t {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

// ============================================================================
// Limits — `printer.h`
// ============================================================================
//
// These size arrays inside `pappl_pr_driver_data_t`, so getting one wrong
// moves every field after it. The probe checks each value.

/// `#define PAPPL_MAX_BIN 16`
pub const PAPPL_MAX_BIN: usize = 16;
/// `#define PAPPL_MAX_MEDIA 256`
pub const PAPPL_MAX_MEDIA: usize = 256;
/// `#define PAPPL_MAX_RASTER_TYPE 16`
pub const PAPPL_MAX_RASTER_TYPE: usize = 16;
/// `#define PAPPL_MAX_RESOLUTION 4`
pub const PAPPL_MAX_RESOLUTION: usize = 4;
/// `#define PAPPL_MAX_SOURCE 16`
pub const PAPPL_MAX_SOURCE: usize = 16;
/// `#define PAPPL_MAX_SUPPLY 32`
pub const PAPPL_MAX_SUPPLY: usize = 32;
/// `#define PAPPL_MAX_TYPE 32`
pub const PAPPL_MAX_TYPE: usize = 32;
/// `#define PAPPL_MAX_VENDOR 32`
pub const PAPPL_MAX_VENDOR: usize = 32;

// ============================================================================
// Enum constants
// ============================================================================

// `pappl_loglevel_e` — log.h
pub const PAPPL_LOGLEVEL_UNSPEC: pappl_loglevel_t = -1;
pub const PAPPL_LOGLEVEL_DEBUG: pappl_loglevel_t = 0;
pub const PAPPL_LOGLEVEL_INFO: pappl_loglevel_t = 1;
pub const PAPPL_LOGLEVEL_WARN: pappl_loglevel_t = 2;
pub const PAPPL_LOGLEVEL_ERROR: pappl_loglevel_t = 3;
pub const PAPPL_LOGLEVEL_FATAL: pappl_loglevel_t = 4;

// `pappl_raster_type_e` — printer.h
pub const PAPPL_PWG_RASTER_TYPE_NONE: pappl_raster_type_t = 0x0000;
pub const PAPPL_PWG_RASTER_TYPE_BLACK_1: pappl_raster_type_t = 0x0004;
pub const PAPPL_PWG_RASTER_TYPE_BLACK_8: pappl_raster_type_t = 0x0008;
pub const PAPPL_PWG_RASTER_TYPE_SGRAY_8: pappl_raster_type_t = 0x0200;

// `pappl_color_mode_e` — printer.h
pub const PAPPL_COLOR_MODE_AUTO: pappl_color_mode_t = 0x0001;
pub const PAPPL_COLOR_MODE_AUTO_MONOCHROME: pappl_color_mode_t = 0x0002;
pub const PAPPL_COLOR_MODE_BI_LEVEL: pappl_color_mode_t = 0x0004;
pub const PAPPL_COLOR_MODE_MONOCHROME: pappl_color_mode_t = 0x0010;

// `pappl_content_e` — printer.h
pub const PAPPL_CONTENT_AUTO: pappl_content_t = 0x01;
pub const PAPPL_CONTENT_GRAPHIC: pappl_content_t = 0x02;
pub const PAPPL_CONTENT_PHOTO: pappl_content_t = 0x04;
pub const PAPPL_CONTENT_TEXT: pappl_content_t = 0x08;
pub const PAPPL_CONTENT_TEXT_AND_GRAPHIC: pappl_content_t = 0x10;

// `pappl_duplex_e` — printer.h
pub const PAPPL_DUPLEX_NONE: pappl_duplex_t = 0;
pub const PAPPL_DUPLEX_NORMAL: pappl_duplex_t = 1;

// `pappl_sides_e` — printer.h
pub const PAPPL_SIDES_ONE_SIDED: pappl_sides_t = 0x01;
pub const PAPPL_SIDES_TWO_SIDED_LONG_EDGE: pappl_sides_t = 0x02;
pub const PAPPL_SIDES_TWO_SIDED_SHORT_EDGE: pappl_sides_t = 0x04;

// `pappl_scaling_e` — printer.h
pub const PAPPL_SCALING_AUTO: pappl_scaling_t = 0x01;
pub const PAPPL_SCALING_AUTO_FIT: pappl_scaling_t = 0x02;
pub const PAPPL_SCALING_FILL: pappl_scaling_t = 0x04;
pub const PAPPL_SCALING_FIT: pappl_scaling_t = 0x08;
pub const PAPPL_SCALING_NONE: pappl_scaling_t = 0x10;

// `pappl_kind_e` — printer.h
pub const PAPPL_KIND_DOCUMENT: pappl_kind_t = 0x0002;
pub const PAPPL_KIND_ENVELOPE: pappl_kind_t = 0x0004;

// `pappl_identify_actions_e` — printer.h
pub const PAPPL_IDENTIFY_ACTIONS_NONE: pappl_identify_actions_t = 0x0000;
pub const PAPPL_IDENTIFY_ACTIONS_DISPLAY: pappl_identify_actions_t = 0x0001;
pub const PAPPL_IDENTIFY_ACTIONS_SOUND: pappl_identify_actions_t = 0x0004;

// `pappl_media_tracking_e` — printer.h
pub const PAPPL_MEDIA_TRACKING_CONTINUOUS: pappl_media_tracking_t = 0x0001;
pub const PAPPL_MEDIA_TRACKING_GAP: pappl_media_tracking_t = 0x0002;
pub const PAPPL_MEDIA_TRACKING_MARK: pappl_media_tracking_t = 0x0004;
pub const PAPPL_MEDIA_TRACKING_WEB: pappl_media_tracking_t = 0x0008;

// `pappl_soptions_e` — system.h
pub const PAPPL_SOPTIONS_NONE: pappl_soptions_t = 0x0000;
pub const PAPPL_SOPTIONS_MULTI_QUEUE: pappl_soptions_t = 0x0002;
pub const PAPPL_SOPTIONS_RAW_SOCKET: pappl_soptions_t = 0x0004;
pub const PAPPL_SOPTIONS_USB_PRINTER: pappl_soptions_t = 0x0008;
pub const PAPPL_SOPTIONS_WEB_INTERFACE: pappl_soptions_t = 0x0010;
pub const PAPPL_SOPTIONS_WEB_LOG: pappl_soptions_t = 0x0020;
pub const PAPPL_SOPTIONS_WEB_NETWORK: pappl_soptions_t = 0x0040;
pub const PAPPL_SOPTIONS_WEB_REMOTE: pappl_soptions_t = 0x0080;
pub const PAPPL_SOPTIONS_WEB_SECURITY: pappl_soptions_t = 0x0100;
pub const PAPPL_SOPTIONS_WEB_TLS: pappl_soptions_t = 0x0200;
/// `@since PAPPL 1.1@` — the only version annotation in the 1.3.1 headers.
pub const PAPPL_SOPTIONS_NO_TLS: pappl_soptions_t = 0x0400;

// `pappl_devtype_e` — device.h
pub const PAPPL_DEVTYPE_FILE: pappl_devtype_t = 0x01;
pub const PAPPL_DEVTYPE_USB: pappl_devtype_t = 0x02;
pub const PAPPL_DEVTYPE_SOCKET: pappl_devtype_t = 0x10;
pub const PAPPL_DEVTYPE_DNS_SD: pappl_devtype_t = 0x20;
pub const PAPPL_DEVTYPE_LOCAL: pappl_devtype_t = 0x0f;
pub const PAPPL_DEVTYPE_NETWORK: pappl_devtype_t = 0xf0;
pub const PAPPL_DEVTYPE_ALL: pappl_devtype_t = 0xff;

// `pappl_preason_e` — printer.h
pub const PAPPL_PREASON_NONE: pappl_preason_t = 0x0000;
pub const PAPPL_PREASON_OTHER: pappl_preason_t = 0x0001;
pub const PAPPL_PREASON_COVER_OPEN: pappl_preason_t = 0x0002;
pub const PAPPL_PREASON_MEDIA_EMPTY: pappl_preason_t = 0x0080;
pub const PAPPL_PREASON_MEDIA_JAM: pappl_preason_t = 0x0100;
pub const PAPPL_PREASON_MEDIA_LOW: pappl_preason_t = 0x0200;
pub const PAPPL_PREASON_MEDIA_NEEDED: pappl_preason_t = 0x0400;
pub const PAPPL_PREASON_TONER_EMPTY: pappl_preason_t = 0x2000;
pub const PAPPL_PREASON_TONER_LOW: pappl_preason_t = 0x4000;
pub const PAPPL_PREASON_DOOR_OPEN: pappl_preason_t = 0x8000;

// `pappl_jreason_e` — job.h
pub const PAPPL_JREASON_NONE: pappl_jreason_t = 0x00000000;
pub const PAPPL_JREASON_ABORTED_BY_SYSTEM: pappl_jreason_t = 0x00000001;
pub const PAPPL_JREASON_DOCUMENT_FORMAT_ERROR: pappl_jreason_t = 0x00000004;
pub const PAPPL_JREASON_DOCUMENT_UNPRINTABLE_ERROR: pappl_jreason_t = 0x00000020;
pub const PAPPL_JREASON_ERRORS_DETECTED: pappl_jreason_t = 0x00000040;
pub const PAPPL_JREASON_JOB_CANCELED_BY_USER: pappl_jreason_t = 0x00000100;
pub const PAPPL_JREASON_JOB_COMPLETED_SUCCESSFULLY: pappl_jreason_t = 0x00000200;
pub const PAPPL_JREASON_JOB_COMPLETED_WITH_ERRORS: pappl_jreason_t = 0x00000400;
pub const PAPPL_JREASON_WARNINGS_DETECTED: pappl_jreason_t = 0x00200000;

// ============================================================================
// Callback types — `printer.h`, `system.h`, `mainloop.h`, `device.h`
// ============================================================================
//
// Every one of these is called BY libpappl. Rule 5 applies to all of them: the
// Rust side must not unwind across the boundary, which is what the wrapper
// crate's `catch_unwind` shim exists to guarantee.

/// ```c
/// typedef void (*pappl_pr_delete_cb_t)(pappl_printer_t *printer, pappl_pr_driver_data_t *data);
/// ```
pub type pappl_pr_delete_cb_t =
    Option<unsafe extern "C" fn(printer: *mut pappl_printer_t, data: *mut pappl_pr_driver_data_t)>;

/// ```c
/// typedef void (*pappl_pr_identify_cb_t)(pappl_printer_t *printer, pappl_identify_actions_t actions, const char *message);
/// ```
pub type pappl_pr_identify_cb_t = Option<
    unsafe extern "C" fn(
        printer: *mut pappl_printer_t,
        actions: pappl_identify_actions_t,
        message: *const c_char,
    ),
>;

/// ```c
/// typedef bool (*pappl_pr_printfile_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device);
/// ```
pub type pappl_pr_printfile_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_rendjob_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device);
/// ```
pub type pappl_pr_rendjob_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_rendpage_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device, unsigned page);
/// ```
pub type pappl_pr_rendpage_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
        page: c_uint,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_rstartjob_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device);
/// ```
pub type pappl_pr_rstartjob_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_rstartpage_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device, unsigned page);
/// ```
pub type pappl_pr_rstartpage_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
        page: c_uint,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_rwriteline_cb_t)(pappl_job_t *job, pappl_pr_options_t *options, pappl_device_t *device, unsigned y, const unsigned char *line);
/// ```
///
/// This is the hot path: PAPPL calls it once per raster line.
pub type pappl_pr_rwriteline_cb_t = Option<
    unsafe extern "C" fn(
        job: *mut pappl_job_t,
        options: *mut pappl_pr_options_t,
        device: *mut pappl_device_t,
        y: c_uint,
        line: *const c_uchar,
    ) -> bool,
>;

/// ```c
/// typedef bool (*pappl_pr_status_cb_t)(pappl_printer_t *printer);
/// ```
pub type pappl_pr_status_cb_t = Option<unsafe extern "C" fn(printer: *mut pappl_printer_t) -> bool>;

/// ```c
/// typedef const char *(*pappl_pr_testpage_cb_t)(pappl_printer_t *printer, char *buffer, size_t bufsize);
/// ```
pub type pappl_pr_testpage_cb_t = Option<
    unsafe extern "C" fn(
        printer: *mut pappl_printer_t,
        buffer: *mut c_char,
        bufsize: usize,
    ) -> *const c_char,
>;

/// ```c
/// typedef ssize_t (*pappl_pr_usb_cb_t)(pappl_printer_t *printer, pappl_device_t *device, void *buffer, size_t bufsize, size_t bytes, void *data);
/// ```
pub type pappl_pr_usb_cb_t = Option<
    unsafe extern "C" fn(
        printer: *mut pappl_printer_t,
        device: *mut pappl_device_t,
        buffer: *mut c_void,
        bufsize: usize,
        bytes: usize,
        data: *mut c_void,
    ) -> isize,
>;

/// ```c
/// typedef bool (*pappl_save_cb_t)(pappl_system_t *system, void *data);
/// ```
pub type pappl_save_cb_t =
    Option<unsafe extern "C" fn(system: *mut pappl_system_t, data: *mut c_void) -> bool>;

/// ```c
/// typedef const char *(*pappl_pr_autoadd_cb_t)(const char *device_info, const char *device_uri, const char *device_id, void *data);
/// ```
pub type pappl_pr_autoadd_cb_t = Option<
    unsafe extern "C" fn(
        device_info: *const c_char,
        device_uri: *const c_char,
        device_id: *const c_char,
        data: *mut c_void,
    ) -> *const c_char,
>;

/// ```c
/// typedef void (*pappl_pr_create_cb_t)(pappl_printer_t *printer, void *data);
/// ```
pub type pappl_pr_create_cb_t =
    Option<unsafe extern "C" fn(printer: *mut pappl_printer_t, data: *mut c_void)>;

/// ```c
/// typedef bool (*pappl_pr_driver_cb_t)(pappl_system_t *system, const char *driver_name, const char *device_uri, const char *device_id, pappl_pr_driver_data_t *driver_data, ipp_t **driver_attrs, void *data);
/// ```
pub type pappl_pr_driver_cb_t = Option<
    unsafe extern "C" fn(
        system: *mut pappl_system_t,
        driver_name: *const c_char,
        device_uri: *const c_char,
        device_id: *const c_char,
        driver_data: *mut pappl_pr_driver_data_t,
        driver_attrs: *mut *mut ipp_t,
        data: *mut c_void,
    ) -> bool,
>;

/// ```c
/// typedef int (*pappl_ml_subcmd_cb_t)(const char *base_name, int num_options, cups_option_t *options, int num_files, char **files, void *data);
/// ```
pub type pappl_ml_subcmd_cb_t = Option<
    unsafe extern "C" fn(
        base_name: *const c_char,
        num_options: c_int,
        options: *mut cups_option_t,
        num_files: c_int,
        files: *mut *mut c_char,
        data: *mut c_void,
    ) -> c_int,
>;

/// ```c
/// typedef pappl_system_t *(*pappl_ml_system_cb_t)(int num_options, cups_option_t *options, void *data);
/// ```
pub type pappl_ml_system_cb_t = Option<
    unsafe extern "C" fn(
        num_options: c_int,
        options: *mut cups_option_t,
        data: *mut c_void,
    ) -> *mut pappl_system_t,
>;

/// ```c
/// typedef void (*pappl_ml_usage_cb_t)(void *data);
/// ```
pub type pappl_ml_usage_cb_t = Option<unsafe extern "C" fn(data: *mut c_void)>;

/// ```c
/// typedef void (*pappl_deverror_cb_t)(const char *message, void *err_data);
/// ```
pub type pappl_deverror_cb_t =
    Option<unsafe extern "C" fn(message: *const c_char, err_data: *mut c_void)>;

// ============================================================================
// Structures
// ============================================================================

/// ```c
/// typedef struct pappl_icon_s {
///   char        filename[256];
///   const void  *data;
///   size_t      datalen;
/// } pappl_icon_t;
/// ```
#[repr(C)]
pub struct pappl_icon_t {
    pub filename: [c_char; 256],
    pub data: *const c_void,
    pub datalen: usize,
}

/// ```c
/// typedef struct pappl_media_col_s {
///   int    bottom_margin; int left_margin; int left_offset; int right_margin;
///   int    size_width;    int size_length; char size_name[64]; char source[64];
///   int    top_margin;    int top_offset;
///   pappl_media_tracking_t tracking;
///   char   type[64];
/// } pappl_media_col_t;
/// ```
///
/// All margins and sizes are in hundredths of a millimetre, which is the unit
/// the hard-margin table (decision Q-2) has to be converted into.
#[repr(C)]
pub struct pappl_media_col_t {
    pub bottom_margin: c_int,
    pub left_margin: c_int,
    pub left_offset: c_int,
    pub right_margin: c_int,
    pub size_width: c_int,
    pub size_length: c_int,
    pub size_name: [c_char; 64],
    pub source: [c_char; 64],
    pub top_margin: c_int,
    pub top_offset: c_int,
    pub tracking: pappl_media_tracking_t,
    pub type_: [c_char; 64],
}

/// ```c
/// typedef struct pappl_pr_driver_s {
///   const char *name; const char *description; const char *device_id; void *extension;
/// } pappl_pr_driver_t;
/// ```
#[repr(C)]
pub struct pappl_pr_driver_t {
    pub name: *const c_char,
    pub description: *const c_char,
    pub device_id: *const c_char,
    pub extension: *mut c_void,
}

/// ```c
/// typedef struct pappl_supply_s {
///   pappl_supply_color_t color; char description[256]; bool is_consumed;
///   int level; pappl_supply_type_t type;
/// } pappl_supply_t;
/// ```
#[repr(C)]
pub struct pappl_supply_t {
    pub color: pappl_supply_color_t,
    pub description: [c_char; 256],
    pub is_consumed: bool,
    pub level: c_int,
    pub type_: pappl_supply_type_t,
}

/// ```c
/// struct pappl_pr_options_s { ... };   // Combined print job options
/// ```
///
/// The full declaration is in `printer.h`; every field is transcribed here in
/// order and checked by `tests/layout.rs`.
#[repr(C)]
pub struct pappl_pr_options_t {
    pub header: cups_page_header2_t,
    pub num_pages: c_uint,
    pub first_page: c_uint,
    pub last_page: c_uint,
    pub dither: pappl_dither_t,
    pub copies: c_int,
    pub finishings: pappl_finishings_t,
    pub media: pappl_media_col_t,
    pub orientation_requested: ipp_orient_t,
    pub output_bin: [c_char; 64],
    pub print_color_mode: pappl_color_mode_t,
    pub print_content_optimize: pappl_content_t,
    pub print_darkness: c_int,
    pub darkness_configured: c_int,
    pub print_quality: ipp_quality_t,
    pub print_scaling: pappl_scaling_t,
    pub print_speed: c_int,
    pub printer_resolution: [c_int; 2],
    pub sides: pappl_sides_t,
    pub num_vendor: c_int,
    pub vendor: *mut cups_option_t,
}

/// ```c
/// struct pappl_pr_driver_data_s { ... };   // Printer driver data
/// ```
///
/// The 8728-byte structure the driver fills in to describe the printer. The
/// full declaration is in `printer.h`; every field is transcribed here in
/// order and checked by `tests/layout.rs`.
#[repr(C)]
pub struct pappl_pr_driver_data_t {
    pub extension: *mut c_void,
    pub delete_cb: pappl_pr_delete_cb_t,
    pub identify_cb: pappl_pr_identify_cb_t,
    pub printfile_cb: pappl_pr_printfile_cb_t,
    pub rendjob_cb: pappl_pr_rendjob_cb_t,
    pub rendpage_cb: pappl_pr_rendpage_cb_t,
    pub rstartjob_cb: pappl_pr_rstartjob_cb_t,
    pub rstartpage_cb: pappl_pr_rstartpage_cb_t,
    pub rwriteline_cb: pappl_pr_rwriteline_cb_t,
    pub status_cb: pappl_pr_status_cb_t,
    pub testpage_cb: pappl_pr_testpage_cb_t,
    pub gdither: pappl_dither_t,
    pub pdither: pappl_dither_t,
    pub format: *const c_char,
    pub make_and_model: [c_char; 128],
    pub ppm: c_int,
    pub ppm_color: c_int,
    pub icons: [pappl_icon_t; 3],
    pub kind: pappl_kind_t,
    pub has_supplies: bool,
    pub input_face_up: bool,
    pub output_face_up: bool,
    pub orient_default: ipp_orient_t,
    pub color_supported: pappl_color_mode_t,
    pub color_default: pappl_color_mode_t,
    pub content_default: pappl_content_t,
    pub quality_default: ipp_quality_t,
    pub scaling_default: pappl_scaling_t,
    pub raster_types: pappl_raster_type_t,
    pub force_raster_type: pappl_raster_type_t,
    pub duplex: pappl_duplex_t,
    pub sides_supported: pappl_sides_t,
    pub sides_default: pappl_sides_t,
    pub finishings: pappl_finishings_t,
    pub num_resolution: c_int,
    pub x_resolution: [c_int; PAPPL_MAX_RESOLUTION],
    pub y_resolution: [c_int; PAPPL_MAX_RESOLUTION],
    pub x_default: c_int,
    pub y_default: c_int,
    pub borderless: bool,
    pub left_right: c_int,
    pub bottom_top: c_int,
    pub num_media: c_int,
    pub media: [*const c_char; PAPPL_MAX_MEDIA],
    pub media_default: pappl_media_col_t,
    pub media_ready: [pappl_media_col_t; PAPPL_MAX_SOURCE],
    pub num_source: c_int,
    pub source: [*const c_char; PAPPL_MAX_SOURCE],
    pub left_offset_supported: [c_int; 2],
    pub top_offset_supported: [c_int; 2],
    pub tracking_supported: pappl_media_tracking_t,
    pub num_type: c_int,
    pub type_: [*const c_char; PAPPL_MAX_TYPE],
    pub num_bin: c_int,
    pub bin: [*const c_char; PAPPL_MAX_BIN],
    pub bin_default: c_int,
    pub mode_configured: pappl_label_mode_t,
    pub mode_supported: pappl_label_mode_t,
    pub tear_offset_configured: c_int,
    pub tear_offset_supported: [c_int; 2],
    pub speed_supported: [c_int; 2],
    pub speed_default: c_int,
    pub darkness_default: c_int,
    pub darkness_configured: c_int,
    pub darkness_supported: c_int,
    pub identify_default: pappl_identify_actions_t,
    pub identify_supported: pappl_identify_actions_t,
    pub num_features: c_int,
    pub features: [*const c_char; PAPPL_MAX_VENDOR],
    pub num_vendor: c_int,
    pub vendor: [*const c_char; PAPPL_MAX_VENDOR],
}

// ============================================================================
// Functions
// ============================================================================
//
// Only the surface this Printer Application actually needs is bound. Each
// declaration quotes the real prototype; `tests/symbols.rs` checks that every
// one of them is exported by the installed libpappl.

extern "C" {
    // ---- mainloop.h ------------------------------------------------------

    /// ```c
    /// extern int papplMainloop(int argc, char *argv[], const char *version, const char *footer_html, int num_drivers, pappl_pr_driver_t *drivers, pappl_pr_autoadd_cb_t autoadd_cb, pappl_pr_driver_cb_t driver_cb, const char *subcmd_name, pappl_ml_subcmd_cb_t subcmd_cb, pappl_ml_system_cb_t system_cb, pappl_ml_usage_cb_t usage_cb, void *data);
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn papplMainloop(
        argc: c_int,
        argv: *mut *mut c_char,
        version: *const c_char,
        footer_html: *const c_char,
        num_drivers: c_int,
        drivers: *mut pappl_pr_driver_t,
        autoadd_cb: pappl_pr_autoadd_cb_t,
        driver_cb: pappl_pr_driver_cb_t,
        subcmd_name: *const c_char,
        subcmd_cb: pappl_ml_subcmd_cb_t,
        system_cb: pappl_ml_system_cb_t,
        usage_cb: pappl_ml_usage_cb_t,
        data: *mut c_void,
    ) -> c_int;

    /// ```c
    /// extern void papplMainloopShutdown(void);
    /// ```
    pub fn papplMainloopShutdown();

    // ---- system.h --------------------------------------------------------

    /// ```c
    /// extern pappl_system_t *papplSystemCreate(pappl_soptions_t options, const char *name, int port, const char *subtypes, const char *spooldir, const char *logfile, pappl_loglevel_t loglevel, const char *auth_service, bool tls_only);
    /// ```
    pub fn papplSystemCreate(
        options: pappl_soptions_t,
        name: *const c_char,
        port: c_int,
        subtypes: *const c_char,
        spooldir: *const c_char,
        logfile: *const c_char,
        loglevel: pappl_loglevel_t,
        auth_service: *const c_char,
        tls_only: bool,
    ) -> *mut pappl_system_t;

    /// ```c
    /// extern void papplSystemDelete(pappl_system_t *system);
    /// ```
    pub fn papplSystemDelete(system: *mut pappl_system_t);

    /// ```c
    /// extern void papplSystemRun(pappl_system_t *system);
    /// ```
    pub fn papplSystemRun(system: *mut pappl_system_t);

    /// ```c
    /// extern void papplSystemShutdown(pappl_system_t *system);
    /// ```
    pub fn papplSystemShutdown(system: *mut pappl_system_t);

    /// ```c
    /// extern bool papplSystemIsRunning(pappl_system_t *system);
    /// ```
    pub fn papplSystemIsRunning(system: *mut pappl_system_t) -> bool;

    /// ```c
    /// extern bool papplSystemAddListeners(pappl_system_t *system, const char *name);
    /// ```
    ///
    /// The systemd unit defaults to loopback only (decision Q-1 follow-up,
    /// action 3); network exposure is a deliberate opt-in.
    pub fn papplSystemAddListeners(system: *mut pappl_system_t, name: *const c_char) -> bool;

    /// ```c
    /// extern void papplSystemSetPrinterDrivers(pappl_system_t *system, int num_drivers, pappl_pr_driver_t *drivers, pappl_pr_autoadd_cb_t autoadd_cb, pappl_pr_create_cb_t create_cb, pappl_pr_driver_cb_t driver_cb, void *data);
    /// ```
    pub fn papplSystemSetPrinterDrivers(
        system: *mut pappl_system_t,
        num_drivers: c_int,
        drivers: *mut pappl_pr_driver_t,
        autoadd_cb: pappl_pr_autoadd_cb_t,
        create_cb: pappl_pr_create_cb_t,
        driver_cb: pappl_pr_driver_cb_t,
        data: *mut c_void,
    );

    /// ```c
    /// extern void papplSystemSetLogLevel(pappl_system_t *system, pappl_loglevel_t loglevel);
    /// ```
    pub fn papplSystemSetLogLevel(system: *mut pappl_system_t, loglevel: pappl_loglevel_t);

    /// ```c
    /// extern pappl_loglevel_t papplSystemGetLogLevel(pappl_system_t *system);
    /// ```
    pub fn papplSystemGetLogLevel(system: *mut pappl_system_t) -> pappl_loglevel_t;

    /// ```c
    /// extern bool papplSystemLoadState(pappl_system_t *system, const char *filename);
    /// ```
    pub fn papplSystemLoadState(system: *mut pappl_system_t, filename: *const c_char) -> bool;

    /// ```c
    /// extern bool papplSystemSaveState(pappl_system_t *system, const char *filename);
    /// ```
    pub fn papplSystemSaveState(system: *mut pappl_system_t, filename: *const c_char) -> bool;

    /// ```c
    /// extern void papplSystemSetSaveCallback(pappl_system_t *system, pappl_save_cb_t cb, void *data);
    /// ```
    ///
    /// Setting this suppresses the mainloop's own state handling: the `server`
    /// subcommand installs a state file only `if (!system->save_cb)`
    /// (`pappl/mainloop-subcommands.c`), so a system that already has one
    /// neither loads nor writes the state file — `$XDG_CONFIG_HOME/<base
    /// name>.state` for a non-root server, `/var/lib/<base name>.state` when
    /// the server runs as root, which is why scoping `XDG_CONFIG_HOME` alone
    /// does not isolate one run from the next.
    pub fn papplSystemSetSaveCallback(
        system: *mut pappl_system_t,
        cb: pappl_save_cb_t,
        data: *mut c_void,
    );

    // ---- printer.h -------------------------------------------------------

    /// ```c
    /// extern pappl_printer_t *papplPrinterCreate(pappl_system_t *system, int printer_id, const char *printer_name, const char *driver_name, const char *device_id, const char *device_uri);
    /// ```
    pub fn papplPrinterCreate(
        system: *mut pappl_system_t,
        printer_id: c_int,
        printer_name: *const c_char,
        driver_name: *const c_char,
        device_id: *const c_char,
        device_uri: *const c_char,
    ) -> *mut pappl_printer_t;

    /// ```c
    /// extern void papplPrinterDelete(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterDelete(printer: *mut pappl_printer_t);

    /// ```c
    /// extern bool papplPrinterSetDriverData(pappl_printer_t *printer, pappl_pr_driver_data_t *data, ipp_t *attrs);
    /// ```
    pub fn papplPrinterSetDriverData(
        printer: *mut pappl_printer_t,
        data: *mut pappl_pr_driver_data_t,
        attrs: *mut ipp_t,
    ) -> bool;

    /// ```c
    /// extern pappl_pr_driver_data_t *papplPrinterGetDriverData(pappl_printer_t *printer, pappl_pr_driver_data_t *data);
    /// ```
    pub fn papplPrinterGetDriverData(
        printer: *mut pappl_printer_t,
        data: *mut pappl_pr_driver_data_t,
    ) -> *mut pappl_pr_driver_data_t;

    /// ```c
    /// extern bool papplPrinterSetReadyMedia(pappl_printer_t *printer, int num_ready, pappl_media_col_t *ready);
    /// ```
    pub fn papplPrinterSetReadyMedia(
        printer: *mut pappl_printer_t,
        num_ready: c_int,
        ready: *mut pappl_media_col_t,
    ) -> bool;

    /// ```c
    /// extern const char *papplPrinterGetName(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterGetName(printer: *mut pappl_printer_t) -> *const c_char;

    /// ```c
    /// extern int papplPrinterGetID(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterGetID(printer: *mut pappl_printer_t) -> c_int;

    /// ```c
    /// extern pappl_device_t *papplPrinterOpenDevice(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterOpenDevice(printer: *mut pappl_printer_t) -> *mut pappl_device_t;

    /// ```c
    /// extern void papplPrinterCloseDevice(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterCloseDevice(printer: *mut pappl_printer_t);

    /// ```c
    /// extern pappl_preason_t papplPrinterGetReasons(pappl_printer_t *printer);
    /// ```
    pub fn papplPrinterGetReasons(printer: *mut pappl_printer_t) -> pappl_preason_t;

    /// ```c
    /// extern void papplPrinterSetReasons(pappl_printer_t *printer, pappl_preason_t add, pappl_preason_t remove);
    /// ```
    pub fn papplPrinterSetReasons(
        printer: *mut pappl_printer_t,
        add: pappl_preason_t,
        remove: pappl_preason_t,
    );

    // ---- job.h -----------------------------------------------------------

    /// ```c
    /// extern const char *papplJobGetName(pappl_job_t *job);
    /// ```
    pub fn papplJobGetName(job: *mut pappl_job_t) -> *const c_char;

    /// ```c
    /// extern const char *papplJobGetUsername(pappl_job_t *job);
    /// ```
    pub fn papplJobGetUsername(job: *mut pappl_job_t) -> *const c_char;

    /// ```c
    /// extern int papplJobGetID(pappl_job_t *job);
    /// ```
    pub fn papplJobGetID(job: *mut pappl_job_t) -> c_int;

    /// ```c
    /// extern const char *papplJobGetFilename(pappl_job_t *job);
    /// ```
    pub fn papplJobGetFilename(job: *mut pappl_job_t) -> *const c_char;

    /// ```c
    /// extern const char *papplJobGetFormat(pappl_job_t *job);
    /// ```
    pub fn papplJobGetFormat(job: *mut pappl_job_t) -> *const c_char;

    /// ```c
    /// extern int papplJobGetImpressions(pappl_job_t *job);
    /// ```
    pub fn papplJobGetImpressions(job: *mut pappl_job_t) -> c_int;

    /// ```c
    /// extern void papplJobSetImpressionsCompleted(pappl_job_t *job, int add);
    /// ```
    pub fn papplJobSetImpressionsCompleted(job: *mut pappl_job_t, add: c_int);

    /// ```c
    /// extern bool papplJobIsCanceled(pappl_job_t *job);
    /// ```
    pub fn papplJobIsCanceled(job: *mut pappl_job_t) -> bool;

    /// ```c
    /// extern void *papplJobGetData(pappl_job_t *job);
    /// ```
    pub fn papplJobGetData(job: *mut pappl_job_t) -> *mut c_void;

    /// ```c
    /// extern void papplJobSetData(pappl_job_t *job, void *data);
    /// ```
    pub fn papplJobSetData(job: *mut pappl_job_t, data: *mut c_void);

    /// ```c
    /// extern void papplJobSetReasons(pappl_job_t *job, pappl_jreason_t add, pappl_jreason_t remove);
    /// ```
    pub fn papplJobSetReasons(job: *mut pappl_job_t, add: pappl_jreason_t, remove: pappl_jreason_t);

    /// ```c
    /// extern pappl_printer_t *papplJobGetPrinter(pappl_job_t *job);
    /// ```
    pub fn papplJobGetPrinter(job: *mut pappl_job_t) -> *mut pappl_printer_t;

    // ---- device.h --------------------------------------------------------

    /// ```c
    /// extern ssize_t papplDeviceWrite(pappl_device_t *device, const void *buffer, size_t bytes);
    /// ```
    ///
    /// This is how the SPL2 stream reaches the printer.
    pub fn papplDeviceWrite(
        device: *mut pappl_device_t,
        buffer: *const c_void,
        bytes: usize,
    ) -> isize;

    /// ```c
    /// extern ssize_t papplDevicePuts(pappl_device_t *device, const char *s);
    /// ```
    pub fn papplDevicePuts(device: *mut pappl_device_t, s: *const c_char) -> isize;

    /// ```c
    /// extern void papplDeviceFlush(pappl_device_t *device);
    /// ```
    pub fn papplDeviceFlush(device: *mut pappl_device_t);

    /// ```c
    /// extern ssize_t papplDeviceRead(pappl_device_t *device, void *buffer, size_t bytes);
    /// ```
    pub fn papplDeviceRead(device: *mut pappl_device_t, buffer: *mut c_void, bytes: usize)
        -> isize;

    /// ```c
    /// extern pappl_preason_t papplDeviceGetStatus(pappl_device_t *device);
    /// ```
    pub fn papplDeviceGetStatus(device: *mut pappl_device_t) -> pappl_preason_t;

    // ---- log.h -----------------------------------------------------------

    /// ```c
    /// extern void papplLog(pappl_system_t *system, pappl_loglevel_t level, const char *message, ...);
    /// ```
    ///
    /// Variadic and `printf`-formatted. The wrapper crate must never pass a
    /// caller-controlled string as `message`; it formats in Rust and passes
    /// `"%s"`.
    pub fn papplLog(
        system: *mut pappl_system_t,
        level: pappl_loglevel_t,
        message: *const c_char,
        ...
    );

    /// ```c
    /// extern void papplLogJob(pappl_job_t *job, pappl_loglevel_t level, const char *message, ...);
    /// ```
    pub fn papplLogJob(job: *mut pappl_job_t, level: pappl_loglevel_t, message: *const c_char, ...);

    /// ```c
    /// extern void papplLogPrinter(pappl_printer_t *printer, pappl_loglevel_t level, const char *message, ...);
    /// ```
    pub fn papplLogPrinter(
        printer: *mut pappl_printer_t,
        level: pappl_loglevel_t,
        message: *const c_char,
        ...
    );

    // ---- base.h ----------------------------------------------------------

    /// ```c
    /// extern size_t papplCopyString(char *dst, const char *src, size_t dstsize);
    /// ```
    pub fn papplCopyString(dst: *mut c_char, src: *const c_char, dstsize: usize) -> usize;
}

// Values from cups/raster.h and cups/ipp.h; verified by the layout probe.
pub const CUPS_CSPACE_K: u32 = 3;
pub const CUPS_ORDER_CHUNKED: u32 = 0;
pub const IPP_ORIENT_NONE: u32 = 7;
pub const IPP_QUALITY_NORMAL: u32 = 4;
