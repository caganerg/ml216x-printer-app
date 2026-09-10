// SPDX-License-Identifier: MIT

//! The job being printed.

use std::ffi::CStr;

use pappl_sys as sys;

use crate::{Error, LogLevel, Result};

/// A job PAPPL passed into a callback.
///
/// Borrowed like [`crate::Device`]: PAPPL owns the job object and outlives the
/// callback, so there is no `Drop` and the lifetime keeps the handle from
/// escaping the invocation.
pub struct Job<'a> {
    raw: *mut sys::pappl_job_t,
    _borrow: std::marker::PhantomData<&'a mut sys::pappl_job_t>,
}

impl<'a> Job<'a> {
    /// Wrap a job pointer PAPPL passed to a callback.
    ///
    /// # Safety
    ///
    /// `raw` must be a job pointer PAPPL owns and keeps valid for `'a`.
    pub unsafe fn from_raw(raw: *mut sys::pappl_job_t) -> Result<Self> {
        if raw.is_null() {
            return Err(Error::NullPointer("job"));
        }
        Ok(Self {
            raw,
            _borrow: std::marker::PhantomData,
        })
    }

    /// The job's numeric id.
    pub fn id(&self) -> i32 {
        // SAFETY: `raw` is a live job for 'a.
        unsafe { sys::papplJobGetID(self.raw) }
    }

    /// The job name, as supplied by the client.
    ///
    /// Untrusted: it comes from whoever submitted the job. It is returned as
    /// `&str` only when it is valid UTF-8, and callers must still quote it
    /// before it reaches a PJL line — PAPPL passes IPP job names through as
    /// UTF-8, which the classic filter path rarely saw.
    pub fn name(&self) -> Result<&str> {
        // SAFETY: `raw` is live; papplJobGetName returns a NUL-terminated
        // string owned by the job, valid as long as the job is.
        let raw = unsafe { sys::papplJobGetName(self.raw) };
        Self::str_from(raw, "job name")
    }

    /// The submitting user name. Untrusted, like [`Job::name`].
    pub fn username(&self) -> Result<&str> {
        // SAFETY: as above.
        let raw = unsafe { sys::papplJobGetUsername(self.raw) };
        Self::str_from(raw, "job user name")
    }

    /// Has the job been cancelled?
    ///
    /// Worth checking between pages and between bands: a cancelled job should
    /// stop producing output rather than finish the page it is on.
    pub fn is_cancelled(&self) -> bool {
        // SAFETY: `raw` is a live job for 'a.
        unsafe { sys::papplJobIsCanceled(self.raw) }
    }

    /// Record one more completed impression.
    pub fn add_impressions_completed(&mut self, count: i32) {
        // SAFETY: `raw` is a live job for 'a.
        unsafe { sys::papplJobSetImpressionsCompleted(self.raw, count) }
    }

    /// Write a line to the job's log.
    ///
    /// The message is passed to `papplLogJob` as a `"%s"` argument, never as
    /// the format string: a job name containing `%n` must not reach a `printf`
    /// implementation as a format.
    pub fn log(&self, level: LogLevel, message: &str) {
        let mut buffer = [0u8; 512];
        let len = message.len().min(buffer.len() - 1);
        for (slot, &byte) in buffer[..len].iter_mut().zip(message.as_bytes()) {
            *slot = if byte == 0 { b'?' } else { byte };
        }

        // SAFETY: `raw` is live; the buffer is NUL-terminated by construction
        // (it was zeroed and at most len-1 bytes were written).
        unsafe {
            sys::papplLogJob(
                self.raw,
                level.to_raw(),
                c"%s".as_ptr(),
                buffer.as_ptr().cast::<std::ffi::c_char>(),
            );
        }
    }

    /// The raw pointer, for the parts of the API this wrapper does not cover
    /// yet — including passing the job back to [`crate::guard`].
    pub fn as_raw(&self) -> *mut sys::pappl_job_t {
        self.raw
    }

    fn str_from(raw: *const std::ffi::c_char, what: &'static str) -> Result<&'a str> {
        if raw.is_null() {
            return Err(Error::NullPointer(what));
        }
        // SAFETY: non-null and NUL-terminated by PAPPL's contract; the string
        // is owned by the job, which outlives 'a.
        let bytes = unsafe { CStr::from_ptr(raw) };
        bytes.to_str().map_err(|_| Error::NotUtf8(what))
    }
}
