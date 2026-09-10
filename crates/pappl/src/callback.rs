// SPDX-License-Identifier: MIT

//! The callback boundary: nothing unwinds into C.
//!
//! Project rule 5: unwinding across `extern "C"` is undefined behaviour, and
//! every raster callback PAPPL invokes is an `extern "C"` function we provide.
//! A `panic!`, an `unwrap()` on a `None`, an arithmetic overflow in a debug
//! build, an allocation failure — any of them, anywhere in the driver, would
//! otherwise unwind into libpappl's C frames.
//!
//! So every callback body goes through [`guard`], and nothing else in the
//! crate is allowed to be reachable from an `extern "C"` function without it.
//! `guard` catches the unwind, turns it into the failure value PAPPL's
//! convention expects, and logs what happened against the job.
//!
//! # A note on `panic = "abort"`
//!
//! If the binary is ever built with `panic = "abort"`, a panic terminates the
//! process before [`guard`] sees it. That is not undefined behaviour — an
//! abort is a defined, if abrupt, outcome — but it loses the log line and the
//! refused job. The Debian build keeps the default unwinding strategy for
//! exactly that reason.
//!
//! # Why the panic path allocates nothing
//!
//! The panic being handled may itself be an allocation failure, and formatting
//! the message with `format!` would allocate again. The message is therefore
//! copied into a fixed stack buffer, truncated if necessary, and passed to
//! `papplLogJob` as a `"%s"` argument — never as the format string itself,
//! which would hand a caller-influenced string to a `printf` implementation.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

use pappl_sys as sys;

/// Largest panic message forwarded to the PAPPL log, NUL included.
const MESSAGE_CAPACITY: usize = 512;

/// A NUL-terminated message built without allocating.
struct StackMessage {
    buffer: [u8; MESSAGE_CAPACITY],
    len: usize,
}

impl StackMessage {
    fn new() -> Self {
        Self {
            buffer: [0; MESSAGE_CAPACITY],
            len: 0,
        }
    }

    /// Append what fits, dropping the rest. Interior NUL bytes are replaced so
    /// the C side cannot see a truncated message as a shorter one.
    fn push(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.len + 1 >= MESSAGE_CAPACITY {
                return;
            }
            self.buffer[self.len] = if byte == 0 { b'?' } else { byte };
            self.len += 1;
        }
    }

    fn as_ptr(&self) -> *const std::ffi::c_char {
        self.buffer.as_ptr().cast()
    }
}

/// Extract whatever a panic payload can tell us, without allocating.
fn describe(payload: &(dyn Any + Send), message: &mut StackMessage) {
    message.push("panic in a PAPPL callback: ");
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        message.push(text);
    } else if let Some(text) = payload.downcast_ref::<String>() {
        message.push(text);
    } else {
        message.push("(no message)");
    }
    message.push(
        " — this is a bug in the driver; the job is refused rather than \
         producing unknown bytes",
    );
}

/// Run a callback body, converting both errors and panics into `failure`.
///
/// `job` may be null: the shim is used from callbacks that have no job, and a
/// panic must still not escape. With no job to log against, the message goes
/// to stderr, which PAPPL captures for the printer's log.
///
/// # Safety
///
/// `job` must be either null or a job pointer PAPPL passed to this callback
/// and still owns for the duration of the call.
pub unsafe fn guard<T, F>(job: *mut sys::pappl_job_t, failure: T, body: F) -> T
where
    F: FnOnce() -> crate::Result<T>,
{
    // AssertUnwindSafe is the right call here rather than a papering-over: the
    // closure borrows driver state that lives across the callback, and if it
    // panics we do not continue using that state — we fail the job. There is
    // no "carry on with possibly-broken invariants" path for the panic to
    // poison.
    let outcome = catch_unwind(AssertUnwindSafe(body));

    match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            let mut message = StackMessage::new();
            message.push("job failed: ");
            // Display for our Error allocates only for Driver(String), which
            // already exists; this is the non-panic path, so it is safe to
            // format into the stack buffer through the Write impl below.
            let mut sink = MessageSink(&mut message);
            let _ = std::fmt::Write::write_fmt(&mut sink, format_args!("{error}"));
            log(job, sys::PAPPL_LOGLEVEL_ERROR, &message);
            failure
        }
        Err(payload) => {
            let mut message = StackMessage::new();
            describe(payload.as_ref(), &mut message);
            log(job, sys::PAPPL_LOGLEVEL_FATAL, &message);
            failure
        }
    }
}

/// Adapter so `write!` can target the stack buffer without allocating.
struct MessageSink<'a>(&'a mut StackMessage);

impl std::fmt::Write for MessageSink<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.push(s);
        Ok(())
    }
}

fn log(job: *mut sys::pappl_job_t, level: sys::pappl_loglevel_t, message: &StackMessage) {
    if job.is_null() {
        // No job to log against. stderr is what PAPPL gives a driver with no
        // context, and it is better than dropping the reason on the floor.
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(b"pappl: ");
        let _ = stderr.write_all(&message.buffer[..message.len]);
        let _ = stderr.write_all(b"\n");
        return;
    }

    // SAFETY: `job` is non-null and owned by PAPPL for the duration of the
    // callback. The format string is our own literal; the message is passed
    // as an argument, never interpreted as a format.
    unsafe {
        sys::papplLogJob(job, level, c"%s".as_ptr(), message.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    // Every test here uses a null job pointer on purpose: it exercises the
    // shim without a live PAPPL system, and the null path is itself something
    // that must not crash.

    #[test]
    fn test_a_panic_in_a_callback_does_not_escape() {
        let result = unsafe {
            guard(std::ptr::null_mut(), false, || {
                panic!("deliberate panic from a test");
            })
        };
        assert!(!result, "a panicking callback must report failure to PAPPL");
    }

    #[test]
    fn test_a_panic_with_a_formatted_message_does_not_escape() {
        let page = 7;
        let result = unsafe {
            guard(std::ptr::null_mut(), false, || {
                panic!("deliberate panic on page {page}");
            })
        };
        assert!(!result);
    }

    #[test]
    fn test_an_unwrap_on_none_does_not_escape() {
        // The failure mode rule 5 is really about: not a deliberate panic! but
        // an accidental one deep in driver code.
        let result = unsafe {
            guard(std::ptr::null_mut(), false, || {
                // A lookup that misses, as a margin table lookup would.
                let table: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
                Ok(*table.get("A4").unwrap() > 0)
            })
        };
        assert!(!result);
    }

    #[test]
    fn test_an_error_reports_failure_without_panicking() {
        let result = unsafe {
            guard(std::ptr::null_mut(), false, || {
                Err::<bool, _>(Error::Driver("no margin table for this medium".into()))
            })
        };
        assert!(!result);
    }

    #[test]
    fn test_success_passes_the_value_through() {
        let result = unsafe { guard(std::ptr::null_mut(), false, || Ok(true)) };
        assert!(result);
    }

    #[test]
    fn test_the_failure_value_is_whatever_the_caller_says() {
        // Not every callback returns bool: pappl_pr_testpage_cb_t returns a
        // pointer, and the failure value there is null.
        let result: isize = unsafe { guard(std::ptr::null_mut(), -1, || panic!("boom")) };
        assert_eq!(result, -1);
    }

    #[test]
    fn test_a_long_panic_message_is_truncated_not_lost() {
        let mut message = StackMessage::new();
        message.push(&"x".repeat(MESSAGE_CAPACITY * 2));
        assert_eq!(message.len, MESSAGE_CAPACITY - 1);
        assert_eq!(
            message.buffer[MESSAGE_CAPACITY - 1],
            0,
            "must stay NUL-terminated"
        );
    }

    #[test]
    fn test_interior_nul_bytes_cannot_truncate_the_message() {
        let mut message = StackMessage::new();
        message.push("before\0after");
        let text = String::from_utf8_lossy(&message.buffer[..message.len]).into_owned();
        assert_eq!(text, "before?after");
    }
}
