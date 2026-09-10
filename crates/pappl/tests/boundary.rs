// SPDX-License-Identifier: MIT

//! The property this crate exists for, tested through a real `extern "C"`
//! function pointer of the type PAPPL will store in its driver data.
//!
//! The unit tests call [`pappl::guard`] directly. This one goes one step
//! further and calls it the way libpappl will: through a
//! `pappl_pr_rstartjob_cb_t`, which is the exact function-pointer type
//! `pappl_pr_driver_data_t.rstartjob_cb` holds. If the shim ever stopped
//! catching, the process would abort here rather than return `false`.

use pappl::{guard, Device};
use pappl_sys as sys;

/// A callback that panics deep inside its body, as a driver bug would.
unsafe extern "C" fn panicking_rstartjob(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    _device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        guard(job, false, || {
            let bands: Vec<u32> = Vec::new();
            // The kind of accident rule 5 is about: an index that is fine for
            // every page the author had in mind, and not for this one.
            Ok(bands[3] > 0)
        })
    }
}

/// A callback that fails cleanly, returning an error rather than panicking.
unsafe extern "C" fn failing_rstartjob(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    device: *mut sys::pappl_device_t,
) -> bool {
    unsafe {
        guard(job, false, || {
            // Null device: an error, not a dereference.
            let mut device = Device::from_raw(device)?;
            device.write_all(b"\x1b%-12345X")?;
            Ok(true)
        })
    }
}

/// A callback that succeeds, so the test proves the shim is not simply
/// returning `false` for everything.
unsafe extern "C" fn succeeding_rstartjob(
    job: *mut sys::pappl_job_t,
    _options: *mut sys::pappl_pr_options_t,
    _device: *mut sys::pappl_device_t,
) -> bool {
    unsafe { guard(job, false, || Ok(true)) }
}

fn call(cb: sys::pappl_pr_rstartjob_cb_t) -> bool {
    let cb = cb.expect("the callback slot must be filled");
    // SAFETY: all three pointers are null, and every callback above is written
    // to treat null as an error rather than dereference it. That is the point:
    // the boundary must hold even when PAPPL hands us nothing.
    unsafe {
        cb(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

#[test]
fn test_a_panic_inside_a_c_callback_returns_false_instead_of_unwinding() {
    assert!(
        !call(Some(panicking_rstartjob)),
        "a panicking callback must return PAPPL's failure value"
    );
}

#[test]
fn test_a_clean_error_inside_a_c_callback_returns_false() {
    assert!(!call(Some(failing_rstartjob)));
}

#[test]
fn test_a_successful_callback_still_returns_true() {
    assert!(
        call(Some(succeeding_rstartjob)),
        "the shim must not turn every call into a failure"
    );
}

#[test]
fn test_the_callback_type_is_the_one_pappl_stores() {
    // If the signature of pappl_pr_rstartjob_cb_t ever changes, this stops
    // compiling — which is the intent. The layout test in pappl-sys checks
    // the field's offset and size; this checks that what we put in it is
    // assignment-compatible with the declared type.
    let slot: sys::pappl_pr_rstartjob_cb_t = Some(succeeding_rstartjob);
    assert!(slot.is_some());
}
