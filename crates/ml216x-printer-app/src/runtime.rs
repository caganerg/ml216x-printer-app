// SPDX-License-Identifier: GPL-2.0-only

//! Where PAPPL's control socket is allowed to live (decision Q-19).
//!
//! PAPPL puts a non-root server's control socket at `$TMPDIR/<name><uid>.sock`,
//! falling back to `/tmp` when `TMPDIR` is unset, and creates it mode 0777.
//! In `/tmp` that means **any local user may connect to it**, and the
//! subcommands reached through it — `add`, `modify`, `delete`, `default`,
//! `submit`, `shutdown` — are the whole control surface of the server, with no
//! authentication of their own. Measured, not inferred: a server run as uid
//! 1000 with `TMPDIR` unset logs
//! `Listening for connections on '/tmp/ml216x-printer-app1000.sock'` and the
//! node is `srwxrwxrwx`.
//!
//! `$XDG_RUNTIME_DIR` is the per-user directory the login session already owns
//! at mode 0700, so pointing `TMPDIR` at it closes that without changing a
//! single documented command: the server and the client subcommands are the
//! same binary and compute the socket path the same way. An explicit `TMPDIR`
//! always wins, so a caller that scopes it — every probe script under
//! `scripts/` does — keeps its own directory.
//!
//! Where no such directory exists the socket stays where PAPPL would have put
//! it and the reason is printed, because a control surface quietly opening to
//! every local account is exactly the kind of thing that must not be silent.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Where the D-Bus client looks for the system bus, and therefore where
/// libavahi-client reaches the daemon that would publish this server.
const SYSTEM_BUS: &str = "DBUS_SYSTEM_BUS_ADDRESS";

/// Stop this server announcing itself on the network (decision Q-23).
///
/// PAPPL registers a DNS-SD service for every printer, including one restored
/// from the state file at startup, and offers no way to decline: there is no
/// system option for it, `printer-dns-sd-name` is not in
/// `printer-settable-attributes`, the web interface has no field for it, and
/// `papplPrinterSetDNSSDName` — the one API that would do it — deadlocks when
/// called from the `PAPPL_EVENT_PRINTER_CREATED` callback, which PAPPL raises
/// while holding the printer's own lock. Measured, not assumed: the server
/// stops responding and the `add` subcommand times out.
///
/// So the announcement is prevented a step earlier. Avahi is reached over the
/// system bus, and pointing the bus address at a path that does not exist
/// makes `avahi_client_new` fail. PAPPL logs `Unable to initialize DNS-SD:
/// Daemon not running` twice and carries on: the server starts, printers are
/// added and restored, and IPP on the loopback address is untouched. Nothing
/// else here uses the bus — USB device access goes through libusb and udev.
///
/// Why decline at all, when a printer application usually wants to be found:
/// this one binds `127.0.0.1` only (Q-18), so the service it advertises names
/// a port nothing off this machine can open. What the announcement did produce
/// was a second queue on the user's own desktop — CUPS creates a temporary
/// queue for a printer it discovers, named `<printer>_<host>`, which cannot be
/// deleted for good because it is re-created from the announcement rather than
/// stored. Sharing to other machines is unaffected: that runs through CUPS on
/// port 631, which advertises its own queue and is a separate mechanism.
///
/// An explicit `DBUS_SYSTEM_BUS_ADDRESS` is left alone, on the same principle
/// as [`confine`]: a caller who has set it means it.
pub fn deny_dnssd() {
    if let Some(address) = bus_address(std::env::var_os(SYSTEM_BUS).as_deref()) {
        std::env::set_var(SYSTEM_BUS, address);
    }
}

/// What to put in `DBUS_SYSTEM_BUS_ADDRESS`, or `None` to leave it alone.
///
/// Split from [`deny_dnssd`] for the same reason [`choose`] is split from
/// [`confine`]: the decision is testable, the process-wide mutation is not.
fn bus_address(current: Option<&OsStr>) -> Option<&'static str> {
    if current.is_some_and(|value| !value.is_empty()) {
        return None;
    }
    // The path must not exist and must not be creatable by accident. It is
    // never opened by this process; it is read by libdbus, which fails to
    // connect, which is the whole point.
    Some("unix:path=/nonexistent/ml216x-no-dns-sd")
}

/// What to do with `TMPDIR` before handing control to PAPPL.
#[derive(Debug, PartialEq, Eq)]
pub enum SocketDir {
    /// `TMPDIR` is set by the caller and is left exactly as it is.
    Explicit,
    /// Set `TMPDIR` to this directory: it is reserved to this user.
    Confine(PathBuf),
    /// Nothing suitable was found; the socket lands wherever PAPPL puts it and
    /// other local users can reach it. Carries the sentence to print.
    Exposed(String),
}

/// Decide the socket directory from the environment.
///
/// `probe` reports `(is_dir, mode)` for a path, or `None` if it cannot be
/// examined; it is a parameter so the decision can be tested without a
/// filesystem. Split out from [`confine`] for the same reason: the mutation of
/// a process-wide environment variable is not something to run inside a test
/// harness that uses threads.
pub fn choose(
    tmpdir: Option<&OsStr>,
    runtime_dir: Option<&OsStr>,
    probe: impl FnOnce(&Path) -> Option<(bool, u32)>,
) -> SocketDir {
    if tmpdir.is_some_and(|value| !value.is_empty()) {
        return SocketDir::Explicit;
    }
    let Some(dir) = runtime_dir.filter(|value| !value.is_empty()) else {
        return SocketDir::Exposed(
            "neither TMPDIR nor XDG_RUNTIME_DIR is set, so PAPPL's control socket \
             lands in /tmp at mode 0777, where any local user can drive this server"
                .into(),
        );
    };
    let path = PathBuf::from(dir);
    match probe(&path) {
        // Group and other must have nothing. Ownership needs no check: a
        // directory this user cannot write is a bind failure PAPPL reports, not
        // a quiet exposure, and reading the process uid would cost this crate
        // its `forbid(unsafe_code)`.
        Some((true, mode)) if mode & 0o077 == 0 => SocketDir::Confine(path),
        Some((true, mode)) => SocketDir::Exposed(format!(
            "XDG_RUNTIME_DIR {} is mode {:04o} rather than 0700, so PAPPL's control \
             socket is left in /tmp, where any local user can drive this server",
            path.display(),
            mode & 0o7777
        )),
        _ => SocketDir::Exposed(format!(
            "XDG_RUNTIME_DIR {} is not a directory, so PAPPL's control socket is \
             left in /tmp, where any local user can drive this server",
            path.display()
        )),
    }
}

/// Apply [`choose`] to this process, warning on stderr when it cannot help.
///
/// Called once, at the top of `main`, before any thread exists and before
/// anything reads `TMPDIR` — including `std::env::temp_dir`, so the default
/// spool directory follows the socket into the same per-user directory.
pub fn confine() {
    let decision = choose(
        std::env::var_os("TMPDIR").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        |path| {
            std::fs::metadata(path)
                .ok()
                .map(|meta| (meta.is_dir(), meta.permissions().mode()))
        },
    );
    match decision {
        SocketDir::Explicit => {}
        SocketDir::Confine(path) => std::env::set_var("TMPDIR", path),
        SocketDir::Exposed(reason) => {
            eprintln!("ml216x-printer-app: warning: {reason}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn dir(mode: u32) -> impl FnOnce(&Path) -> Option<(bool, u32)> {
        move |_| Some((true, mode))
    }

    /// A caller that scopes `TMPDIR` keeps it. The probe scripts depend on
    /// this: they point both ends at a temporary directory of their own.
    #[test]
    fn an_explicit_tmpdir_is_never_overridden() {
        let tmpdir = OsString::from("/scoped/by/the/caller");
        assert_eq!(
            choose(
                Some(&tmpdir),
                Some(OsStr::new("/run/user/1000")),
                dir(0o40700)
            ),
            SocketDir::Explicit
        );
    }

    /// Q-23: with nothing set, the bus address is pointed at a path that does
    /// not exist, so libavahi-client cannot reach the daemon and PAPPL's
    /// registration attempt fails instead of publishing the printer.
    #[test]
    fn an_unset_bus_address_is_pointed_at_nothing() {
        assert_eq!(
            bus_address(None),
            Some("unix:path=/nonexistent/ml216x-no-dns-sd")
        );
        assert_eq!(
            bus_address(Some(OsStr::new(""))),
            Some("unix:path=/nonexistent/ml216x-no-dns-sd")
        );
    }

    /// A caller who has set the bus address means it, exactly as with `TMPDIR`.
    #[test]
    fn an_explicit_bus_address_is_never_overridden() {
        assert_eq!(
            bus_address(Some(OsStr::new("unix:path=/run/dbus/system_bus_socket"))),
            None
        );
    }

    /// The case the decision exists for: a 0700 runtime directory takes the
    /// socket out of `/tmp`.
    #[test]
    fn a_private_runtime_directory_is_used() {
        assert_eq!(
            choose(None, Some(OsStr::new("/run/user/1000")), dir(0o40700)),
            SocketDir::Confine(PathBuf::from("/run/user/1000"))
        );
    }

    /// An empty variable is not a directory name.
    #[test]
    fn an_empty_tmpdir_does_not_count_as_set() {
        let empty = OsString::new();
        assert_eq!(
            choose(
                Some(&empty),
                Some(OsStr::new("/run/user/1000")),
                dir(0o40700)
            ),
            SocketDir::Confine(PathBuf::from("/run/user/1000"))
        );
    }

    /// A runtime directory others can enter buys nothing, and saying so is the
    /// point: the exposure is reported rather than papered over.
    #[test]
    fn a_group_or_world_accessible_runtime_directory_is_refused() {
        for mode in [0o40750, 0o40705, 0o40777] {
            let SocketDir::Exposed(reason) =
                choose(None, Some(OsStr::new("/run/user/1000")), dir(mode))
            else {
                panic!("mode {mode:o} was accepted");
            };
            assert!(reason.contains("rather than 0700"), "{reason}");
        }
    }

    /// Nothing to fall back to: still not silent.
    #[test]
    fn a_missing_runtime_directory_is_reported() {
        let SocketDir::Exposed(reason) = choose(None, None, dir(0o40700)) else {
            panic!("a missing XDG_RUNTIME_DIR was accepted");
        };
        assert!(
            reason.contains("neither TMPDIR nor XDG_RUNTIME_DIR"),
            "{reason}"
        );

        let SocketDir::Exposed(reason) = choose(None, Some(OsStr::new("/run/user/1000")), |_| None)
        else {
            panic!("an unexaminable XDG_RUNTIME_DIR was accepted");
        };
        assert!(reason.contains("is not a directory"), "{reason}");
    }
}
