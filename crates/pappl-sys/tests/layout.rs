// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layout verification for the hand-written bindings.
//!
//! `build.rs` compiles `probe/layout_probe.c` against the installed PAPPL
//! headers and runs it; this test asserts the Rust declarations against that
//! output. Three things are checked, and the third is what makes the first two
//! trustworthy:
//!
//! 1. the size and alignment of every type declared in the crate,
//! 2. the byte offset and size of **every field** of every such type, and the
//!    value of every enum constant we hardcode,
//! 3. that no record the probe emitted was left unchecked — so a field added
//!    to the probe but forgotten in Rust fails the test instead of passing
//!    unnoticed.
//!
//! Sampling would defeat the purpose: the compiler cannot check a transcribed
//! C header, so a test compiled from that same header has to.

use std::collections::{HashMap, HashSet};
use std::mem::{align_of, offset_of, size_of};
use std::sync::Mutex;

use pappl_sys::*;

const PROBE: &str = include_str!(concat!(env!("OUT_DIR"), "/pappl_layout.txt"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record {
    /// `T <name> <size> <align>`
    Type { size: usize, align: usize },
    /// `F <type>.<field> <offset> <size>`
    Field { offset: usize, size: usize },
    /// `E <name> <value>`
    Enum { value: i64 },
}

fn probe() -> HashMap<String, Record> {
    assert!(
        !PROBE.starts_with("# unavailable"),
        "the layout probe was not produced: {}\n\
         These bindings are hand written, so they are unverified without it. \
         Build natively, or run this test on the target.",
        PROBE.trim()
    );

    let mut map = HashMap::new();
    for line in PROBE.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.split_whitespace();
        let kind = f.next().expect("record kind");
        let name = f.next().expect("record name").to_string();
        let a: i64 = f.next().expect("field 1").parse().expect("numeric");
        let record = match kind {
            "T" => Record::Type {
                size: a as usize,
                align: f.next().expect("align").parse().expect("numeric"),
            },
            "F" => Record::Field {
                offset: a as usize,
                size: f.next().expect("size").parse().expect("numeric"),
            },
            "E" | "V" => Record::Enum { value: a },
            other => panic!("unknown probe record kind {other:?}"),
        };
        assert!(
            map.insert(name.clone(), record).is_none(),
            "duplicate probe record for {name}"
        );
    }
    assert!(!map.is_empty(), "the layout probe produced no records");
    map
}

/// Names consumed by the assertions below, so unchecked records can be found.
static CHECKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn mark(name: &str) {
    let mut guard = CHECKED.lock().expect("the checked-name set is poisoned");
    guard
        .get_or_insert_with(HashSet::new)
        .insert(name.to_string());
}

fn expect(map: &HashMap<String, Record>, name: &str) -> Record {
    mark(name);
    *map.get(name)
        .unwrap_or_else(|| panic!("the probe emitted no record for {name}"))
}

macro_rules! check_type {
    ($map:expr, $rust:ty, $c:literal) => {{
        match expect($map, $c) {
            Record::Type { size, align } => {
                assert_eq!(size_of::<$rust>(), size, "size_of {}", $c);
                assert_eq!(align_of::<$rust>(), align, "align_of {}", $c);
            }
            other => panic!("{} is not a type record: {other:?}", $c),
        }
    }};
}

macro_rules! check_field {
    ($map:expr, $rust:ty, $c:literal, $field:ident, $fieldty:ty) => {{
        let key = concat!($c, ".", stringify!($field));
        match expect($map, key) {
            Record::Field { offset, size } => {
                assert_eq!(offset_of!($rust, $field), offset, "offset of {}", key);
                assert_eq!(size_of::<$fieldty>(), size, "size of {}", key);
            }
            other => panic!("{key} is not a field record: {other:?}"),
        }
    }};
}

macro_rules! check_enum {
    ($map:expr, $c:literal, $value:expr) => {{
        match expect($map, $c) {
            Record::Enum { value } => {
                assert_eq!(i64::from($value), value, "value of {}", $c);
            }
            other => panic!("{} is not an enum record: {other:?}", $c),
        }
    }};
}

#[test]
fn test_every_probed_record_matches_the_rust_declaration() {
    let map = probe();

    // ---- the CUPS ABI the raster header was measured against -------------
    //
    // R-6: cups_page_header2_t belongs to CUPS and is embedded by value at the
    // start of pappl_pr_options_t. A libcups release that changed its layout
    // would move every field after it, and would do so without changing a
    // single symbol — so nothing the linker records would catch it. This is
    // the build-time half of the mitigation; the runtime half is the geometry
    // validation the driver performs on every page.
    check_enum!(&map, "CUPS_VERSION_MAJOR", CUPS_ABI_MAJOR as i64);
    check_enum!(&map, "CUPS_VERSION_MINOR", CUPS_ABI_MINOR as i64);
    mark("CUPS_VERSION_PATCH"); // patch level does not change the layout
    assert!(
        map.contains_key("CUPS_VERSION_PATCH"),
        "the probe must report the CUPS patch level"
    );

    // ---- types ----------------------------------------------------------
    check_type!(&map, pappl_pr_driver_data_t, "pappl_pr_driver_data_t");
    check_type!(&map, pappl_pr_options_t, "pappl_pr_options_t");
    check_type!(&map, pappl_media_col_t, "pappl_media_col_t");
    check_type!(&map, pappl_icon_t, "pappl_icon_t");
    check_type!(&map, pappl_pr_driver_t, "pappl_pr_driver_t");
    check_type!(&map, pappl_supply_t, "pappl_supply_t");
    check_type!(&map, pappl_dither_t, "pappl_dither_t");
    check_type!(&map, cups_page_header2_t, "cups_page_header2_t");
    assert_eq!(
        size_of::<cups_page_header2_t>(),
        CUPS_PAGE_HEADER2_SIZE,
        "the hardcoded raster-header size no longer matches the type"
    );

    // ---- limits ---------------------------------------------------------
    check_enum!(&map, "PAPPL_MAX_BIN", PAPPL_MAX_BIN as i64);
    check_enum!(&map, "PAPPL_MAX_MEDIA", PAPPL_MAX_MEDIA as i64);
    check_enum!(&map, "PAPPL_MAX_RESOLUTION", PAPPL_MAX_RESOLUTION as i64);
    check_enum!(&map, "PAPPL_MAX_SOURCE", PAPPL_MAX_SOURCE as i64);
    check_enum!(&map, "PAPPL_MAX_TYPE", PAPPL_MAX_TYPE as i64);
    check_enum!(&map, "PAPPL_MAX_VENDOR", PAPPL_MAX_VENDOR as i64);

    // ---- pappl_icon_t ---------------------------------------------------
    check_field!(&map, pappl_icon_t, "pappl_icon_t", filename, [i8; 256]);
    check_field!(
        &map,
        pappl_icon_t,
        "pappl_icon_t",
        data,
        *const std::ffi::c_void
    );
    check_field!(&map, pappl_icon_t, "pappl_icon_t", datalen, usize);

    // ---- pappl_media_col_t ----------------------------------------------
    const MC: &str = "pappl_media_col_t";
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        bottom_margin,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        left_margin,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        left_offset,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        right_margin,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        size_width,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        size_length,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        size_name,
        [i8; 64]
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        source,
        [i8; 64]
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        top_margin,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        top_offset,
        i32
    );
    check_field!(
        &map,
        pappl_media_col_t,
        "pappl_media_col_t",
        tracking,
        pappl_media_tracking_t
    );
    // `type` is a Rust keyword, so the field is `type_`; the probe key keeps
    // the C name.
    mark(&format!("{MC}.type"));
    match map[&format!("{MC}.type")] {
        Record::Field { offset, size } => {
            assert_eq!(
                offset_of!(pappl_media_col_t, type_),
                offset,
                "offset of {MC}.type"
            );
            assert_eq!(size_of::<[i8; 64]>(), size, "size of {MC}.type");
        }
        ref other => panic!("{MC}.type is not a field record: {other:?}"),
    }

    // ---- pappl_pr_driver_t ----------------------------------------------
    check_field!(
        &map,
        pappl_pr_driver_t,
        "pappl_pr_driver_t",
        name,
        *const i8
    );
    check_field!(
        &map,
        pappl_pr_driver_t,
        "pappl_pr_driver_t",
        description,
        *const i8
    );
    check_field!(
        &map,
        pappl_pr_driver_t,
        "pappl_pr_driver_t",
        device_id,
        *const i8
    );
    check_field!(
        &map,
        pappl_pr_driver_t,
        "pappl_pr_driver_t",
        extension,
        *mut std::ffi::c_void
    );

    // ---- pappl_supply_t --------------------------------------------------
    check_field!(
        &map,
        pappl_supply_t,
        "pappl_supply_t",
        color,
        pappl_supply_color_t
    );
    check_field!(
        &map,
        pappl_supply_t,
        "pappl_supply_t",
        description,
        [i8; 256]
    );
    check_field!(&map, pappl_supply_t, "pappl_supply_t", is_consumed, bool);
    check_field!(&map, pappl_supply_t, "pappl_supply_t", level, i32);
    mark("pappl_supply_t.type");
    match map["pappl_supply_t.type"] {
        Record::Field { offset, size } => {
            assert_eq!(
                offset_of!(pappl_supply_t, type_),
                offset,
                "offset of pappl_supply_t.type"
            );
            assert_eq!(
                size_of::<pappl_supply_type_t>(),
                size,
                "size of pappl_supply_t.type"
            );
        }
        ref other => panic!("pappl_supply_t.type is not a field record: {other:?}"),
    }

    // ---- pappl_pr_options_t ---------------------------------------------
    const OP: &str = "pappl_pr_options_t";
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        header,
        cups_page_header2_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        num_pages,
        u32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        first_page,
        u32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        last_page,
        u32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        dither,
        pappl_dither_t
    );
    check_field!(&map, pappl_pr_options_t, "pappl_pr_options_t", copies, i32);
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        finishings,
        pappl_finishings_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        media,
        pappl_media_col_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        orientation_requested,
        ipp_orient_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        output_bin,
        [i8; 64]
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_color_mode,
        pappl_color_mode_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_content_optimize,
        pappl_content_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_darkness,
        i32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        darkness_configured,
        i32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_quality,
        ipp_quality_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_scaling,
        pappl_scaling_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        print_speed,
        i32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        printer_resolution,
        [i32; 2]
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        sides,
        pappl_sides_t
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        num_vendor,
        i32
    );
    check_field!(
        &map,
        pappl_pr_options_t,
        "pappl_pr_options_t",
        vendor,
        *mut cups_option_t
    );
    let _ = OP;

    // ---- pappl_pr_driver_data_t -----------------------------------------
    const DD: &str = "pappl_pr_driver_data_t";
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        extension,
        *mut std::ffi::c_void
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        delete_cb,
        pappl_pr_delete_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        identify_cb,
        pappl_pr_identify_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        printfile_cb,
        pappl_pr_printfile_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        rendjob_cb,
        pappl_pr_rendjob_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        rendpage_cb,
        pappl_pr_rendpage_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        rstartjob_cb,
        pappl_pr_rstartjob_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        rstartpage_cb,
        pappl_pr_rstartpage_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        rwriteline_cb,
        pappl_pr_rwriteline_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        status_cb,
        pappl_pr_status_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        testpage_cb,
        pappl_pr_testpage_cb_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        gdither,
        pappl_dither_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        pdither,
        pappl_dither_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        format,
        *const i8
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        make_and_model,
        [i8; 128]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        ppm,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        ppm_color,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        icons,
        [pappl_icon_t; 3]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        kind,
        pappl_kind_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        has_supplies,
        bool
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        input_face_up,
        bool
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        output_face_up,
        bool
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        orient_default,
        ipp_orient_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        color_supported,
        pappl_color_mode_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        color_default,
        pappl_color_mode_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        content_default,
        pappl_content_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        quality_default,
        ipp_quality_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        scaling_default,
        pappl_scaling_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        raster_types,
        pappl_raster_type_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        force_raster_type,
        pappl_raster_type_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        duplex,
        pappl_duplex_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        sides_supported,
        pappl_sides_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        sides_default,
        pappl_sides_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        finishings,
        pappl_finishings_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_resolution,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        x_resolution,
        [i32; PAPPL_MAX_RESOLUTION]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        y_resolution,
        [i32; PAPPL_MAX_RESOLUTION]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        x_default,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        y_default,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        borderless,
        bool
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        left_right,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        bottom_top,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_media,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        media,
        [*const i8; PAPPL_MAX_MEDIA]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        media_default,
        pappl_media_col_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        media_ready,
        [pappl_media_col_t; PAPPL_MAX_SOURCE]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_source,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        source,
        [*const i8; PAPPL_MAX_SOURCE]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        left_offset_supported,
        [i32; 2]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        top_offset_supported,
        [i32; 2]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        tracking_supported,
        pappl_media_tracking_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_type,
        i32
    );
    mark(&format!("{DD}.type"));
    match map[&format!("{DD}.type")] {
        Record::Field { offset, size } => {
            assert_eq!(
                offset_of!(pappl_pr_driver_data_t, type_),
                offset,
                "offset of {DD}.type"
            );
            assert_eq!(
                size_of::<[*const i8; PAPPL_MAX_TYPE]>(),
                size,
                "size of {DD}.type"
            );
        }
        ref other => panic!("{DD}.type is not a field record: {other:?}"),
    }
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_bin,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        bin,
        [*const i8; PAPPL_MAX_BIN]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        bin_default,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        mode_configured,
        pappl_label_mode_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        mode_supported,
        pappl_label_mode_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        tear_offset_configured,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        tear_offset_supported,
        [i32; 2]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        speed_supported,
        [i32; 2]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        speed_default,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        darkness_default,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        darkness_configured,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        darkness_supported,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        identify_default,
        pappl_identify_actions_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        identify_supported,
        pappl_identify_actions_t
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_features,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        features,
        [*const i8; PAPPL_MAX_VENDOR]
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        num_vendor,
        i32
    );
    check_field!(
        &map,
        pappl_pr_driver_data_t,
        "pappl_pr_driver_data_t",
        vendor,
        [*const i8; PAPPL_MAX_VENDOR]
    );

    // ---- enum constants --------------------------------------------------
    check_enum!(&map, "PAPPL_LOGLEVEL_UNSPEC", PAPPL_LOGLEVEL_UNSPEC);
    check_enum!(&map, "PAPPL_LOGLEVEL_DEBUG", PAPPL_LOGLEVEL_DEBUG);
    check_enum!(&map, "PAPPL_LOGLEVEL_INFO", PAPPL_LOGLEVEL_INFO);
    check_enum!(&map, "PAPPL_LOGLEVEL_WARN", PAPPL_LOGLEVEL_WARN);
    check_enum!(&map, "PAPPL_LOGLEVEL_ERROR", PAPPL_LOGLEVEL_ERROR);
    check_enum!(&map, "PAPPL_LOGLEVEL_FATAL", PAPPL_LOGLEVEL_FATAL);

    check_enum!(
        &map,
        "PAPPL_PWG_RASTER_TYPE_NONE",
        PAPPL_PWG_RASTER_TYPE_NONE
    );
    check_enum!(
        &map,
        "PAPPL_PWG_RASTER_TYPE_BLACK_1",
        PAPPL_PWG_RASTER_TYPE_BLACK_1
    );
    check_enum!(
        &map,
        "PAPPL_PWG_RASTER_TYPE_BLACK_8",
        PAPPL_PWG_RASTER_TYPE_BLACK_8
    );
    check_enum!(
        &map,
        "PAPPL_PWG_RASTER_TYPE_SGRAY_8",
        PAPPL_PWG_RASTER_TYPE_SGRAY_8
    );

    check_enum!(&map, "PAPPL_COLOR_MODE_AUTO", PAPPL_COLOR_MODE_AUTO);
    check_enum!(
        &map,
        "PAPPL_COLOR_MODE_AUTO_MONOCHROME",
        PAPPL_COLOR_MODE_AUTO_MONOCHROME
    );
    check_enum!(&map, "PAPPL_COLOR_MODE_BI_LEVEL", PAPPL_COLOR_MODE_BI_LEVEL);
    check_enum!(
        &map,
        "PAPPL_COLOR_MODE_MONOCHROME",
        PAPPL_COLOR_MODE_MONOCHROME
    );

    check_enum!(&map, "PAPPL_CONTENT_AUTO", PAPPL_CONTENT_AUTO);
    check_enum!(&map, "PAPPL_CONTENT_GRAPHIC", PAPPL_CONTENT_GRAPHIC);
    check_enum!(&map, "PAPPL_CONTENT_PHOTO", PAPPL_CONTENT_PHOTO);
    check_enum!(&map, "PAPPL_CONTENT_TEXT", PAPPL_CONTENT_TEXT);
    check_enum!(
        &map,
        "PAPPL_CONTENT_TEXT_AND_GRAPHIC",
        PAPPL_CONTENT_TEXT_AND_GRAPHIC
    );

    check_enum!(&map, "PAPPL_DUPLEX_NONE", PAPPL_DUPLEX_NONE);
    check_enum!(&map, "PAPPL_DUPLEX_NORMAL", PAPPL_DUPLEX_NORMAL);

    check_enum!(&map, "PAPPL_SIDES_ONE_SIDED", PAPPL_SIDES_ONE_SIDED);
    check_enum!(
        &map,
        "PAPPL_SIDES_TWO_SIDED_LONG_EDGE",
        PAPPL_SIDES_TWO_SIDED_LONG_EDGE
    );
    check_enum!(
        &map,
        "PAPPL_SIDES_TWO_SIDED_SHORT_EDGE",
        PAPPL_SIDES_TWO_SIDED_SHORT_EDGE
    );

    check_enum!(&map, "PAPPL_SCALING_AUTO", PAPPL_SCALING_AUTO);
    check_enum!(&map, "PAPPL_SCALING_AUTO_FIT", PAPPL_SCALING_AUTO_FIT);
    check_enum!(&map, "PAPPL_SCALING_FILL", PAPPL_SCALING_FILL);
    check_enum!(&map, "PAPPL_SCALING_FIT", PAPPL_SCALING_FIT);
    check_enum!(&map, "PAPPL_SCALING_NONE", PAPPL_SCALING_NONE);

    check_enum!(&map, "PAPPL_KIND_DOCUMENT", PAPPL_KIND_DOCUMENT);
    check_enum!(&map, "PAPPL_KIND_ENVELOPE", PAPPL_KIND_ENVELOPE);

    check_enum!(
        &map,
        "PAPPL_IDENTIFY_ACTIONS_NONE",
        PAPPL_IDENTIFY_ACTIONS_NONE
    );
    check_enum!(
        &map,
        "PAPPL_IDENTIFY_ACTIONS_DISPLAY",
        PAPPL_IDENTIFY_ACTIONS_DISPLAY
    );
    check_enum!(
        &map,
        "PAPPL_IDENTIFY_ACTIONS_SOUND",
        PAPPL_IDENTIFY_ACTIONS_SOUND
    );

    check_enum!(
        &map,
        "PAPPL_MEDIA_TRACKING_CONTINUOUS",
        PAPPL_MEDIA_TRACKING_CONTINUOUS
    );
    check_enum!(&map, "PAPPL_MEDIA_TRACKING_GAP", PAPPL_MEDIA_TRACKING_GAP);
    check_enum!(&map, "PAPPL_MEDIA_TRACKING_MARK", PAPPL_MEDIA_TRACKING_MARK);
    check_enum!(&map, "PAPPL_MEDIA_TRACKING_WEB", PAPPL_MEDIA_TRACKING_WEB);

    check_enum!(&map, "PAPPL_SOPTIONS_NONE", PAPPL_SOPTIONS_NONE);
    check_enum!(
        &map,
        "PAPPL_SOPTIONS_MULTI_QUEUE",
        PAPPL_SOPTIONS_MULTI_QUEUE
    );
    check_enum!(&map, "PAPPL_SOPTIONS_RAW_SOCKET", PAPPL_SOPTIONS_RAW_SOCKET);
    check_enum!(
        &map,
        "PAPPL_SOPTIONS_USB_PRINTER",
        PAPPL_SOPTIONS_USB_PRINTER
    );
    check_enum!(
        &map,
        "PAPPL_SOPTIONS_WEB_INTERFACE",
        PAPPL_SOPTIONS_WEB_INTERFACE
    );
    check_enum!(&map, "PAPPL_SOPTIONS_WEB_LOG", PAPPL_SOPTIONS_WEB_LOG);
    check_enum!(
        &map,
        "PAPPL_SOPTIONS_WEB_NETWORK",
        PAPPL_SOPTIONS_WEB_NETWORK
    );
    check_enum!(&map, "PAPPL_SOPTIONS_WEB_REMOTE", PAPPL_SOPTIONS_WEB_REMOTE);
    check_enum!(
        &map,
        "PAPPL_SOPTIONS_WEB_SECURITY",
        PAPPL_SOPTIONS_WEB_SECURITY
    );
    check_enum!(&map, "PAPPL_SOPTIONS_WEB_TLS", PAPPL_SOPTIONS_WEB_TLS);
    check_enum!(&map, "PAPPL_SOPTIONS_NO_TLS", PAPPL_SOPTIONS_NO_TLS);

    check_enum!(&map, "PAPPL_DEVTYPE_FILE", PAPPL_DEVTYPE_FILE);
    check_enum!(&map, "PAPPL_DEVTYPE_USB", PAPPL_DEVTYPE_USB);
    check_enum!(&map, "PAPPL_DEVTYPE_SOCKET", PAPPL_DEVTYPE_SOCKET);
    check_enum!(&map, "PAPPL_DEVTYPE_DNS_SD", PAPPL_DEVTYPE_DNS_SD);
    check_enum!(&map, "PAPPL_DEVTYPE_LOCAL", PAPPL_DEVTYPE_LOCAL);
    check_enum!(&map, "PAPPL_DEVTYPE_NETWORK", PAPPL_DEVTYPE_NETWORK);
    check_enum!(&map, "PAPPL_DEVTYPE_ALL", PAPPL_DEVTYPE_ALL);

    check_enum!(&map, "PAPPL_PREASON_NONE", PAPPL_PREASON_NONE);
    check_enum!(&map, "PAPPL_PREASON_OTHER", PAPPL_PREASON_OTHER);
    check_enum!(&map, "PAPPL_PREASON_COVER_OPEN", PAPPL_PREASON_COVER_OPEN);
    check_enum!(&map, "PAPPL_PREASON_MEDIA_EMPTY", PAPPL_PREASON_MEDIA_EMPTY);
    check_enum!(&map, "PAPPL_PREASON_MEDIA_JAM", PAPPL_PREASON_MEDIA_JAM);
    check_enum!(&map, "PAPPL_PREASON_MEDIA_LOW", PAPPL_PREASON_MEDIA_LOW);
    check_enum!(
        &map,
        "PAPPL_PREASON_MEDIA_NEEDED",
        PAPPL_PREASON_MEDIA_NEEDED
    );
    check_enum!(&map, "PAPPL_PREASON_TONER_EMPTY", PAPPL_PREASON_TONER_EMPTY);
    check_enum!(&map, "PAPPL_PREASON_TONER_LOW", PAPPL_PREASON_TONER_LOW);
    check_enum!(&map, "PAPPL_PREASON_DOOR_OPEN", PAPPL_PREASON_DOOR_OPEN);

    check_enum!(&map, "PAPPL_JREASON_NONE", PAPPL_JREASON_NONE);
    check_enum!(
        &map,
        "PAPPL_JREASON_ABORTED_BY_SYSTEM",
        PAPPL_JREASON_ABORTED_BY_SYSTEM
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_DOCUMENT_FORMAT_ERROR",
        PAPPL_JREASON_DOCUMENT_FORMAT_ERROR
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_DOCUMENT_UNPRINTABLE_ERROR",
        PAPPL_JREASON_DOCUMENT_UNPRINTABLE_ERROR
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_ERRORS_DETECTED",
        PAPPL_JREASON_ERRORS_DETECTED
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_JOB_CANCELED_BY_USER",
        PAPPL_JREASON_JOB_CANCELED_BY_USER
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_JOB_COMPLETED_SUCCESSFULLY",
        PAPPL_JREASON_JOB_COMPLETED_SUCCESSFULLY
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_JOB_COMPLETED_WITH_ERRORS",
        PAPPL_JREASON_JOB_COMPLETED_WITH_ERRORS
    );
    check_enum!(
        &map,
        "PAPPL_JREASON_WARNINGS_DETECTED",
        PAPPL_JREASON_WARNINGS_DETECTED
    );

    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MediaClass,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MediaColor,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MediaType,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        OutputType,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        AdvanceDistance,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        AdvanceMedia,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Collate,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        CutMedia,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Duplex,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        HWResolution,
        [std::ffi::c_uint; 2]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        ImagingBoundingBox,
        [std::ffi::c_uint; 4]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        InsertSheet,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Jog,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        LeadingEdge,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Margins,
        [std::ffi::c_uint; 2]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        ManualFeed,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MediaPosition,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MediaWeight,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        MirrorPrint,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        NegativePrint,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        NumCopies,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Orientation,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        OutputFaceUp,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        PageSize,
        [std::ffi::c_uint; 2]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Separations,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        TraySwitch,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        Tumble,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsWidth,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsHeight,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsMediaType,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsBitsPerColor,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsBitsPerPixel,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsBytesPerLine,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsColorOrder,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsColorSpace,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsCompression,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsRowCount,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsRowFeed,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsRowStep,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsNumColors,
        std::ffi::c_uint
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsBorderlessScalingFactor,
        f32
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsPageSize,
        [f32; 2]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsImagingBBox,
        [f32; 4]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsInteger,
        [std::ffi::c_uint; 16]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsReal,
        [f32; 16]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsString,
        [[std::ffi::c_char; 64]; 16]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsMarkerType,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsRenderingIntent,
        [std::ffi::c_char; 64]
    );
    check_field!(
        &map,
        cups_page_header2_t,
        "cups_page_header2_t",
        cupsPageSizeName,
        [std::ffi::c_char; 64]
    );
    check_enum!(&map, "CUPS_CSPACE_K", CUPS_CSPACE_K);
    check_enum!(&map, "CUPS_ORDER_CHUNKED", CUPS_ORDER_CHUNKED);
    check_enum!(&map, "IPP_ORIENT_NONE", IPP_ORIENT_NONE);
    check_enum!(&map, "IPP_QUALITY_NORMAL", IPP_QUALITY_NORMAL);
    // ---- nothing left unchecked -----------------------------------------
    //
    // This is the assertion that makes the rest meaningful: a field added to
    // the probe but forgotten here would otherwise pass silently.
    let checked = CHECKED
        .lock()
        .expect("the checked-name set is poisoned")
        .clone()
        .unwrap_or_default();
    let mut unchecked: Vec<&String> = map.keys().filter(|k| !checked.contains(*k)).collect();
    unchecked.sort();
    assert!(
        unchecked.is_empty(),
        "{} probed record(s) are not checked by this test: {:?}",
        unchecked.len(),
        unchecked
    );
}
