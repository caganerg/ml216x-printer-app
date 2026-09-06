// SPDX-License-Identifier: GPL-2.0-only

#![forbid(unsafe_code)]

use pappl::application::{Application, Capabilities, GeometryProbe, RasterDriver};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

mod driver;
mod media_table;

fn run() -> Result<i32, Box<dyn std::error::Error>> {
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
    std::fs::create_dir_all(&spool)?;
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
