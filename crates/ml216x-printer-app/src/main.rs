// SPDX-License-Identifier: GPL-2.0-only

#![forbid(unsafe_code)]

use pappl::application::{Application, Capabilities, Media};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

// Shared with the transitional filter until spl2-core is extracted.
#[path = "../../../src/media.rs"]
mod media;

// PWG names and physical sizes (0.01 mm); PPD point dimensions are rounded.
// The PPD-to-PWG mapping is intentionally explicit, especially Folio/F4.
const MEDIA: &[Media] = &[
    Media {
        name: c"iso_a4_210x297mm",
        width: 21000,
        length: 29700,
    },
    Media {
        name: c"na_letter_8.5x11in",
        width: 21590,
        length: 27940,
    },
    Media {
        name: c"na_legal_8.5x14in",
        width: 21590,
        length: 35560,
    },
    Media {
        name: c"na_executive_7.25x10.5in",
        width: 18415,
        length: 26670,
    },
    Media {
        name: c"iso_a5_148x210mm",
        width: 14800,
        length: 21000,
    },
    Media {
        name: c"iso_a6_105x148mm",
        width: 10500,
        length: 14800,
    },
    Media {
        name: c"jis_b5_182x257mm",
        width: 18200,
        length: 25700,
    },
    Media {
        name: c"na_number-10_4.125x9.5in",
        width: 10477,
        length: 24130,
    },
    Media {
        name: c"iso_dl_110x220mm",
        width: 11000,
        length: 22000,
    },
    Media {
        name: c"iso_c5_162x229mm",
        width: 16200,
        length: 22900,
    },
    Media {
        name: c"om_folio_210x330mm",
        width: 21000,
        length: 33000,
    },
];

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
    if probe
        && !args
            .iter()
            .skip(1)
            .any(|a| matches!(a.to_bytes(), b"server" | b"drivers" | b"--help"))
    {
        return Err("start the probe explicitly with: --probe --probe-output /absolute/output.jsonl server; submit using a second process and -u".into());
    }
    if args.iter().any(|a| a.to_bytes() == b"--help") {
        println!("Development options: --probe --probe-output ABSOLUTE-PATH --listen-port PORT --spool-directory DIRECTORY");
        println!("P5 writes geometry diagnostics only; real SPL2 printing is not connected yet.");
    }
    std::fs::create_dir_all(&spool)?;
    let app = Application {
        capabilities: Capabilities {
            name: c"samsung_ml216x",
            description: c"Samsung ML-216x (P5 development)",
            media: MEDIA,
            resolutions: &[(300, 300), (600, 600), (1200, 600), (1200, 1200)],
            sources: &[c"auto", c"manual"],
            // IPP keywords mapped to the existing PPD/PJL vocabulary in P6.
            types: &[
                c"auto",
                c"stationery",
                c"stationery-heavyweight",
                c"stationery-lightweight",
                c"stationery-bond",
                c"transparency",
                c"cardstock",
                c"labels",
                c"stationery-preprinted",
                c"stationery-colored",
                c"envelope",
                c"stationery-cotton",
                c"stationery-recycled",
                c"other",
            ],
            // 12.5 pt = 440.9722... hundredths mm, nearest IPP unit is 441.
            margin: (media::HARD_MARGIN_PT * 2540.0 / 72.0).round() as i32,
        },
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
