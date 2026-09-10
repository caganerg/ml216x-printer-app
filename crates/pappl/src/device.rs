// SPDX-License-Identifier: MIT

//! The output device.

use std::io;

use pappl_sys as sys;

use crate::{Error, Result};

/// A device PAPPL opened and still owns.
///
/// Borrowed, never owned: PAPPL passes the device into a callback and closes
/// it itself afterwards, so this wrapper has no `Drop`. The lifetime ties it
/// to the callback invocation it came from, which is what stops it being
/// stashed somewhere and used after PAPPL has closed it.
pub struct Device<'a> {
    raw: *mut sys::pappl_device_t,
    _borrow: std::marker::PhantomData<&'a mut sys::pappl_device_t>,
}

impl Device<'_> {
    /// Wrap a device pointer PAPPL passed to a callback.
    ///
    /// # Safety
    ///
    /// `raw` must be a device pointer PAPPL owns and keeps valid for `'a`.
    pub unsafe fn from_raw(raw: *mut sys::pappl_device_t) -> Result<Self> {
        if raw.is_null() {
            return Err(Error::NullPointer("device"));
        }
        Ok(Self {
            raw,
            _borrow: std::marker::PhantomData,
        })
    }

    /// Write the whole buffer, or fail.
    ///
    /// `papplDeviceWrite` returns the number of bytes written, or a negative
    /// value on error. A short write is treated as an error rather than
    /// retried silently: a partially written band is not a recoverable state
    /// for a QPDL stream, because the printer is mid-record and the checksum
    /// that follows would describe bytes it never received.
    pub fn write_all(&mut self, buffer: &[u8]) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        // SAFETY: `raw` is a live device (checked non-null at construction and
        // valid for 'a); `buffer` is a valid slice for the length given.
        let written =
            unsafe { sys::papplDeviceWrite(self.raw, buffer.as_ptr().cast(), buffer.len()) };

        if written < 0 || written as usize != buffer.len() {
            return Err(Error::DeviceWrite {
                requested: buffer.len(),
                returned: written,
            });
        }
        Ok(())
    }

    /// Flush whatever PAPPL has buffered for the device.
    pub fn flush(&mut self) {
        // SAFETY: as above.
        unsafe { sys::papplDeviceFlush(self.raw) }
    }

    /// The raw pointer, for the parts of the API this wrapper does not cover
    /// yet.
    ///
    /// # Safety
    ///
    /// The caller takes on the same obligations the wrapper was written to
    /// discharge: no unwinding across the boundary, no use after the callback
    /// returns.
    pub unsafe fn as_raw(&mut self) -> *mut sys::pappl_device_t {
        self.raw
    }
}

/// `io::Write` so the SPL2 engine can write straight to the printer.
///
/// This is the join between the two halves of the migration: `spl2-core`'s
/// stream writer is generic over `io::Write`, and this impl is what lets the
/// unchanged, byte-for-byte-critical encoder emit into a PAPPL device without
/// knowing PAPPL exists.
impl io::Write for Device<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        // Deliberately all-or-nothing: see `write_all`. Returning a short
        // count here would let a caller believe a partial band was acceptable.
        self.write_all(buffer).map_err(io::Error::from)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Device::flush(self);
        Ok(())
    }
}
