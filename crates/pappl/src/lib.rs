// SPDX-License-Identifier: MIT

//! Safe wrapper over [`pappl_sys`].
//!
//! This crate owns the entire `unsafe` surface of the Printer Application: the
//! raw declarations live in `pappl-sys`, the driver logic lives in
//! `spl2-core` and `ml216x-printer-app`, and everything unsafe that connects
//! them is here, in one place that can be reviewed as a unit.
//!
//! # What it is for
//!
//! Three jobs, in order of how badly they go wrong if skipped:
//!
//! 1. **Nothing unwinds into C.** Every `extern "C"` callback body goes through
//!    [`guard`], which catches panics and returns the failure value PAPPL
//!    expects. Unwinding across `extern "C"` is undefined behaviour (project
//!    rule 5), and a driver is full of ordinary ways to panic — an `unwrap()`
//!    on a missing margin entry, an arithmetic overflow in a debug build, an
//!    allocation failure while buffering a band.
//! 2. **Handles cannot outlive their callback.** [`Device`] and [`Job`] are
//!    borrowed wrappers with a lifetime and no `Drop`: PAPPL owns those
//!    objects and closes them when the callback returns, so a handle that
//!    escaped would be a use-after-free. Owning wrappers, with `Drop`, arrive
//!    when the mainloop is wired up and this crate creates objects of its own.
//! 3. **Result becomes PAPPL's convention.** PAPPL callbacks report failure as
//!    `false` or a null pointer, with no channel for a reason; [`guard`] logs
//!    the reason against the job and returns the bare failure.
//!
//! # What it deliberately does not do
//!
//! It does not interpret the protocol, and it does not decide anything about
//! the page. Geometry, margins and the SPL2 byte stream belong to
//! `spl2-core`, which stays `#![forbid(unsafe_code)]` and free of C. The one
//! place the two meet is [`Device`]'s [`std::io::Write`] implementation, which
//! lets the unchanged, byte-for-byte-critical encoder write to a PAPPL device
//! without knowing PAPPL exists.
//!
//! # Example: the shape of a callback
//!
//! ```no_run
//! use pappl::{guard, Device, Job};
//! use pappl_sys as sys;
//!
//! unsafe extern "C" fn rstartjob(
//!     job: *mut sys::pappl_job_t,
//!     _options: *mut sys::pappl_pr_options_t,
//!     device: *mut sys::pappl_device_t,
//! ) -> bool {
//!     // Every callback body is wrapped, and the wrapper is the only thing
//!     // that returns to C.
//!     guard(job, false, || {
//!         let job = Job::from_raw(job)?;
//!         let mut device = Device::from_raw(device)?;
//!         job.log(pappl::LogLevel::Info, "starting an SPL2 job");
//!         device.write_all(b"")?;
//!         Ok(true)
//!     })
//! }
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

pub mod application;
mod callback;
mod device;
mod error;
mod job;

pub use callback::guard;
pub use device::Device;
pub use error::{Error, Result};
pub use job::Job;

use pappl_sys as sys;

/// Log levels, mapped to `pappl_loglevel_t`.
///
/// A Rust enum rather than the raw `c_int`, so a caller cannot pass a value
/// PAPPL does not define. `PAPPL_LOGLEVEL_UNSPEC` is deliberately absent: it
/// means "not specified" in configuration, not a level to log at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// `PAPPL_LOGLEVEL_DEBUG`
    Debug,
    /// `PAPPL_LOGLEVEL_INFO`
    Info,
    /// `PAPPL_LOGLEVEL_WARN`
    Warn,
    /// `PAPPL_LOGLEVEL_ERROR`
    Error,
    /// `PAPPL_LOGLEVEL_FATAL`
    Fatal,
}

impl LogLevel {
    /// The `pappl_loglevel_t` value.
    pub fn to_raw(self) -> sys::pappl_loglevel_t {
        match self {
            LogLevel::Debug => sys::PAPPL_LOGLEVEL_DEBUG,
            LogLevel::Info => sys::PAPPL_LOGLEVEL_INFO,
            LogLevel::Warn => sys::PAPPL_LOGLEVEL_WARN,
            LogLevel::Error => sys::PAPPL_LOGLEVEL_ERROR,
            LogLevel::Fatal => sys::PAPPL_LOGLEVEL_FATAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_levels_map_to_the_values_pappl_defines() {
        // The constants themselves are checked against the C headers by
        // pappl-sys's layout test; this checks the mapping, so a reordered
        // match arm cannot quietly turn an error into a debug line.
        assert_eq!(LogLevel::Debug.to_raw(), 0);
        assert_eq!(LogLevel::Info.to_raw(), 1);
        assert_eq!(LogLevel::Warn.to_raw(), 2);
        assert_eq!(LogLevel::Error.to_raw(), 3);
        assert_eq!(LogLevel::Fatal.to_raw(), 4);
    }

    #[test]
    fn test_a_null_pointer_is_an_error_not_a_crash() {
        // PAPPL should not hand us null here, but "should not" is not a
        // guarantee we can dereference.
        let device = unsafe { Device::from_raw(std::ptr::null_mut()) };
        assert!(matches!(device, Err(Error::NullPointer("device"))));
        let job = unsafe { Job::from_raw(std::ptr::null_mut()) };
        assert!(matches!(job, Err(Error::NullPointer("job"))));
    }

    #[test]
    fn test_device_errors_carry_the_short_write_detail() {
        let error = Error::DeviceWrite {
            requested: 4096,
            returned: 12,
        };
        let text = error.to_string();
        assert!(text.contains("4096"), "{text}");
        assert!(text.contains("12"), "{text}");
    }
}
