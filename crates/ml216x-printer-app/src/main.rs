// SPDX-License-Identifier: GPL-2.0-only

#![forbid(unsafe_code)]

use pappl::application::{Application, Capabilities, GeometryProbe, RasterDriver};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;

mod driver;
mod media_table;
mod runtime;

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    // Before anything reads TMPDIR — `std::env::temp_dir` below included — and
    // while this process is still single threaded. Decision Q-19: PAPPL puts
    // its control socket in $TMPDIR at mode 0777, so leaving that at /tmp hands
    // every local account the server's whole control surface.
    runtime::confine();
    // Also before any thread exists, and for the same kind of reason: Q-23
    // declines the DNS-SD announcement PAPPL would otherwise make for every
    // printer, which this loopback-only server cannot honour and which turns
    // into a second, undeletable queue on the desktop.
    runtime::deny_dnssd();
    let mut args = Vec::new();
    let mut probe = false;
    let mut probe_output = None;
    let mut port = 8631;
    let mut spool = std::env::temp_dir().join(format!("ml216x-{}", std::process::id()));
    let mut input = std::env::args_os();
    if let Some(program) = input.next() {
        args.push(CString::new(program.as_bytes())?);
    }
    while let Some(arg) = input.next() {
        match arg.to_str() {
            Some("--probe") => probe = true,
            Some("--probe-output") => {
                let path =
                    std::path::PathBuf::from(input.next().ok_or("missing --probe-output value")?);
                if !path.is_absolute() {
                    return Err("--probe-output requires an absolute path".into());
                }
                probe_output = Some(CString::new(path.as_os_str().as_bytes())?);
            }
            Some("--listen-port") => {
                port = input
                    .next()
                    .ok_or("missing --listen-port value")?
                    .to_str()
                    .ok_or("invalid port")?
                    .parse()?
            }
            Some("--spool-directory") => {
                spool = input
                    .next()
                    .ok_or("missing --spool-directory value")?
                    .into()
            }
            _ => args.push(CString::new(arg.as_bytes())?),
        }
    }
    if probe_output.is_some()
        && !args
            .iter()
            .skip(1)
            .any(|a| matches!(a.to_bytes(), b"server" | b"drivers" | b"--help"))
    {
        return Err("start the file destination explicitly with: [--probe] --probe-output /absolute/output server; submit using a second process and -u".into());
    }
    if args.iter().any(|a| a.to_bytes() == b"--help") {
        println!("Development options: --probe --probe-output ABSOLUTE-PATH --listen-port PORT --spool-directory DIRECTORY");
        println!("Without --probe the driver emits SPL2/QPDL to the device. The margins it");
        println!("uses have not been measured on hardware yet; see release gate G-1.");
    }

    // --probe keeps the P5 instrument: JSON Lines to a file, never a printer.
    let driver: Box<dyn RasterDriver> = if probe {
        Box::new(GeometryProbe)
    } else {
        Box::new(driver::Spl2Driver::new())
    };
    // A raster job is written into the spool before it is converted, so it
    // must not be readable by another local user. `create_dir_all` applies the
    // umask and leaves 0755 (or 0775) under an ordinary login; the root
    // service that ran until 2.0.0~alpha-3 got this guarantee from postinst's
    // `chmod 700 /var/spool/ml216x-printer-app`, and moving into the user's
    // session must not lose it (decision Q-18). The packaged unit creates the
    // directory itself as a 0700 RuntimeDirectory, so this is the backstop for
    // a manual run.
    //
    // Only a directory this process created is chmodded. `--spool-directory`
    // takes any path, and silently tightening one the caller already had —
    // `/tmp`, or a directory shared with something else — would be a worse
    // surprise than the one being prevented. An existing directory is reported
    // instead, on stderr, which is the journal under the unit and the terminal
    // for a hand-run server.
    let created = !spool.exists();
    std::fs::create_dir_all(&spool)?;
    if created {
        std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700))?;
    } else {
        let mode = std::fs::metadata(&spool)?.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "ml216x-printer-app: warning: spool directory {} is mode {:04o}, \
                 so other local users can read the raster of every job passing \
                 through it; 0700 is what this expects",
                spool.display(),
                mode & 0o7777
            );
        }
    }
    let app = Application {
        capabilities: Capabilities {
            // Q-15: the two drivers do not share a name, so a printer saved by
            // one is refused by the other rather than silently adopted.
            name: if probe {
                c"samsung_ml216x_probe"
            } else {
                c"samsung_ml216x"
            },
            description: c"Samsung ML-216x Series",
            media: media_table::MEDIA,
            // The order decides what a job that names no resolution runs at:
            // PAPPL takes the first entry for draft quality, the middle one for
            // normal and the last for high, and never consults the declared
            // default. Normal quality therefore has to sit in the middle. See
            // `pappl::application::quality_resolutions` for the transcription
            // of PAPPL's rule, and Q-14 for what the old order did.
            resolutions: &[(300, 300), (1200, 600), (600, 600), (1200, 1200)],
            default_resolution: (600, 600),
            sources: media_table::SOURCE_NAMES,
            types: media_table::TYPE_NAMES,
            // 12.5 pt = 440.9722... hundredths mm, nearest IPP unit is 441.
            margin: (spl2_core::media::HARD_MARGIN_PT * 2540.0 / 72.0).round() as i32,
        },
        driver,
        probe,
        probe_output,
        port,
        spool_directory: CString::new(spool.as_os_str().as_bytes())?,
    };
    app.run(&args).map_err(Into::into)
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(0) => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::FAILURE,
        Err(e) => {
            eprintln!("ml216x-printer-app: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
