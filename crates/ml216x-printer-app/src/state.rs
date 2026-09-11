// SPDX-License-Identifier: GPL-2.0-only

//! The state file this server loads itself, and why (decision Q-24).
//!
//! PAPPL registers a DNS-SD service for every printer that comes out of the
//! state file, and 1.3.1 has no supported way to decline: there is no system
//! option, `printer-dns-sd-name` is not in `printer-settable-attributes`, the
//! web interface has no field, and `papplPrinterSetDNSSDName` — the one API
//! that would do it — deadlocks when called from the
//! `PAPPL_EVENT_PRINTER_CREATED` callback, because PAPPL raises that event
//! holding the printer's lock for reading. Q-23 answered that by cutting the
//! process off from D-Bus so Avahi could not be reached at all, and that broke
//! two things it was not aiming at, both measured: the `devices` sub-command
//! aborted, and so did the running server the moment anyone opened the web
//! interface's "Add Printer" page. PAPPL hands the DNS-SD library a null
//! client on that path and the library asserts.
//!
//! So Q-24 says it in PAPPL's own vocabulary instead: a printer with no
//! DNS-SD name is never advertised, because `papplSystemRun` registers one
//! only `if (printer->dns_sd_name)`. The name has to be cleared after the
//! state file has been read and before that loop runs, and PAPPL's mainloop
//! leaves no hook between the two — it loads the state itself, after the
//! system callback returns. What it does offer is an opt-out: it installs a
//! state file only `if (!system->save_cb)`, so a system that already has a
//! save callback neither loads nor saves one. This application therefore takes
//! the whole job over, inside the system callback: load the state file, clear
//! the DNS-SD name of the system and of every printer that came out of it,
//! then install `papplSystemSaveState` on the same path.
//!
//! Taking the job over means owning the path, and the path must be the one the
//! mainloop would have chosen or an upgrade loses the printers the last
//! version saved. [`state_file`] is that path and nothing more: a
//! transcription of `_papplMainloopRunServer` in
//! `pappl/mainloop-subcommands.c` of PAPPL 1.3.1, including the order of its
//! branches and its treatment of a variable that is set but unusable. Where
//! the two could disagree the transcription wins, because agreeing with PAPPL
//! is the whole requirement; the places that cost fidelity are commented
//! where they arise.
//!
//! [`subcommand`] exists for the same reason. Only a `server` run may take
//! state handling over — `drivers` also builds a system through the same
//! callback, and a listing command must not rewrite the file a server owns —
//! and "is this a server run" is PAPPL's question to answer, so its parser is
//! transcribed too.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// The sub-commands PAPPL 1.3.1 recognises, from the `subcommands` array in
/// `papplMainloop`. A bare word that is not one of these is a file name, not a
/// sub-command, which is why the list is here in full rather than reduced to
/// the one entry this module cares about.
const SUBCOMMANDS: &[&[u8]] = &[
    b"add",
    b"autoadd",
    b"cancel",
    b"default",
    b"delete",
    b"devices",
    b"drivers",
    b"jobs",
    b"modify",
    b"options",
    b"pause",
    b"printers",
    b"resume",
    b"server",
    b"shutdown",
    b"status",
    b"submit",
];

/// The short options PAPPL consumes a following argument for: `-d PRINTER`,
/// `-h HOST`, `-j JOB-ID`, `-m DRIVER-NAME`, `-n COPIES`, `-o NAME=VALUE`,
/// `-t TITLE`, `-u PRINTER-URI`, `-v DEVICE-URI`. `-a` is the only short
/// option that stands alone.
const VALUE_OPTIONS: &[u8] = b"dhjmnotuv";

/// The sub-command PAPPL will act on, by PAPPL's own parsing rules.
///
/// `args` is what will be handed to `papplMainloop`, program name included.
///
/// One quirk is transcribed deliberately. PAPPL walks a bundle of short
/// options with `for (opt = argv[i] + 1; *opt; opt ++)` but switches on
/// `argv[i][1]` every time round, so `-dq` runs the `-d` case twice and eats
/// two arguments rather than one. That is not defensible and it is not
/// imitated by accident: this function has to reach the same conclusion as the
/// parser it is predicting, and the documented invocations never bundle short
/// options, so the quirk is unreachable in practice either way.
pub fn subcommand<'a>(args: impl IntoIterator<Item = &'a [u8]>) -> Option<&'static [u8]> {
    let mut rest = args.into_iter().skip(1);
    while let Some(bytes) = rest.next() {
        match bytes {
            // A file name follows, and is not a sub-command however it reads.
            b"--" => {
                rest.next();
            }
            // `--help` and `--version` return before a sub-command is reached,
            // and any other long option is an error; either way, nothing here.
            _ if bytes.starts_with(b"--") => return None,
            [b'-', flags @ ..] if !flags.is_empty() => {
                if VALUE_OPTIONS.contains(&flags[0]) {
                    for _ in flags {
                        rest.next();
                    }
                }
            }
            // The first bare word. PAPPL takes it as the sub-command only if
            // it names one, and treats anything else as a file to print.
            _ => return SUBCOMMANDS.iter().find(|name| **name == bytes).copied(),
        }
    }
    None
}

/// What this process knows about where a state file may go.
///
/// The fields are the environment variables and the process identity PAPPL's
/// mainloop consults, in one place so that [`state_file`] is a pure function
/// of them.
pub struct Environment<'a> {
    /// `basename(argv[0])`, which is what PAPPL names the file after.
    pub base_name: &'a OsStr,
    pub uid: u32,
    pub snap_common: Option<&'a OsStr>,
    pub xdg_config_home: Option<&'a OsStr>,
    pub home: Option<&'a OsStr>,
    pub tmpdir: Option<&'a OsStr>,
}

/// Where PAPPL's mainloop would keep this server's state file.
///
/// `reachable` answers PAPPL's `access(dir, X_OK)`, and `provide` its
/// `mkdir(dir, 0777)` — but as the question "is the directory there when this
/// returns", so that a directory which already exists and simply cannot be
/// searched counts as provided. That is what makes the root branch match
/// PAPPL, which only abandons `/var/lib` when `access` failed with `ENOENT`
/// *and* the `mkdir` that followed failed too.
///
/// Both are parameters because the decision is worth testing and touching the
/// filesystem to test it is not.
pub fn state_file(
    env: &Environment<'_>,
    reachable: impl Fn(&Path) -> bool,
    provide: impl Fn(&Path) -> bool,
) -> PathBuf {
    // Every candidate is assembled the way PAPPL assembles it, by pasting
    // bytes onto the directory rather than by `Path::join`: an empty `HOME`
    // has to produce `/.config`, as C's `snprintf("%s/.config", home)` does,
    // and not the relative `.config` that joining would give.
    let paste = |dir: &OsStr, tail: &[u8]| -> PathBuf {
        let mut bytes = dir.as_bytes().to_vec();
        bytes.extend_from_slice(tail);
        PathBuf::from(OsString::from_vec(bytes))
    };
    let state_in = |dir: &OsStr| -> PathBuf {
        let mut tail = b"/".to_vec();
        tail.extend_from_slice(env.base_name.as_bytes());
        tail.extend_from_slice(b".state");
        paste(dir, &tail)
    };

    // The branches, in PAPPL's order. A variable that is set but unusable
    // stops the chain there rather than falling through to the next branch,
    // because in C `if (snap_common)` and `else if (xdg_config_home)` test
    // only whether the variable is present — an empty or unreachable value
    // reaches the last resort, never the branch below.
    if let Some(snap_common) = env.snap_common {
        if reachable(Path::new(snap_common)) {
            return state_in(snap_common);
        }
    } else if env.uid == 0 {
        // PAPPL_STATEDIR is `/var` in the Debian build of 1.3.1, so root keeps
        // its state in `/var/lib`, created if it somehow is not there.
        let dir = Path::new("/var/lib");
        if reachable(dir) || provide(dir) {
            return state_in(OsStr::new("/var/lib"));
        }
    } else if let Some(xdg_config_home) = env.xdg_config_home {
        if reachable(Path::new(xdg_config_home)) {
            return state_in(xdg_config_home);
        }
    } else if let Some(home) = env.home {
        let config = paste(home, b"/.config");
        if reachable(&config) || provide(&config) {
            return state_in(config.as_os_str());
        }
    }

    // The last resort, where PAPPL says the state "will be lost on the nest
    // reboot/logout". `papplGetTempDir` is TMPDIR when it is set and writable
    // and `/tmp` otherwise; by the time this runs, decision Q-19 has already
    // pointed TMPDIR at `$XDG_RUNTIME_DIR`, and PAPPL reads the same variable
    // in the same process, so the two agree.
    let tmpdir = env
        .tmpdir
        .filter(|dir| !dir.is_empty() && reachable(Path::new(dir)))
        .unwrap_or(OsStr::new("/tmp"));
    let mut tail = b"/".to_vec();
    tail.extend_from_slice(env.base_name.as_bytes());
    tail.extend_from_slice(env.uid.to_string().as_bytes());
    tail.extend_from_slice(b".state");
    paste(tmpdir, &tail)
}

/// Apply [`state_file`] to this process, or explain why it cannot be.
///
/// The uid comes from the owner of `/proc/self`, which is this process's own
/// uid and is how a crate that forbids unsafe code can learn it; `getuid` is
/// not in the standard library. Where that cannot be read the answer is
/// `Err`, carrying the sentence to print: the caller then leaves state
/// handling to PAPPL's mainloop, which works exactly as it did before Q-24 —
/// printers are restored, and advertised.
pub fn path(program: &OsStr) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;

    let uid = std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .map_err(|e| {
            format!(
                "this process's user id cannot be read from /proc/self ({e}), so \
                 the state file PAPPL would use cannot be worked out; printers \
                 will be restored by PAPPL itself and advertised over DNS-SD, \
                 which decision Q-24 declines"
            )
        })?;
    let base_name = base_name(program);
    let (snap_common, xdg_config_home, home, tmpdir) = (
        std::env::var_os("SNAP_COMMON"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("TMPDIR"),
    );
    Ok(state_file(
        &Environment {
            base_name,
            uid,
            snap_common: snap_common.as_deref(),
            xdg_config_home: xdg_config_home.as_deref(),
            home: home.as_deref(),
            tmpdir: tmpdir.as_deref(),
        },
        |path| std::fs::metadata(path).is_ok_and(|meta| meta.is_dir()),
        |path| std::fs::create_dir_all(path).is_ok(),
    ))
}

/// `basename(argv[0])`, which is the name PAPPL gives the state file.
fn base_name(program: &OsStr) -> &OsStr {
    let bytes = program.as_bytes();
    match bytes.iter().rposition(|byte| *byte == b'/') {
        Some(slash) => OsStr::from_bytes(&bytes[slash + 1..]),
        None => program,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(words: &[&str]) -> Option<&'static [u8]> {
        subcommand(words.iter().map(|word| word.as_bytes()))
    }

    #[test]
    fn the_sub_command_is_the_first_bare_word_that_names_one() {
        assert_eq!(find(&["app", "server"]), Some(b"server".as_slice()));
        assert_eq!(find(&["app", "devices"]), Some(b"devices".as_slice()));
        assert_eq!(find(&["app"]), None);
    }

    /// Q-24: a server started with options in front of the sub-command is
    /// still a server run, and it is the one case where taking state handling
    /// over matters. `-o` consumes its value, so the value cannot be mistaken
    /// for the sub-command.
    #[test]
    fn options_before_the_sub_command_are_stepped_over_with_their_values() {
        assert_eq!(
            find(&["app", "-o", "log-level=debug", "server"]),
            Some(b"server".as_slice())
        );
        assert_eq!(find(&["app", "-a", "cancel"]), Some(b"cancel".as_slice()));
        // The value of `-d` is a printer name, not a sub-command, even when it
        // reads like one.
        assert_eq!(
            find(&["app", "-d", "server", "status"]),
            Some(b"status".as_slice())
        );
    }

    /// A file to print is not a sub-command, however it is spelled.
    #[test]
    fn a_file_name_is_never_a_sub_command() {
        assert_eq!(find(&["app", "--", "server"]), None);
        assert_eq!(find(&["app", "page.pdf"]), None);
        assert_eq!(find(&["app", "--help", "server"]), None);
    }

    fn env(base_name: &str, uid: u32) -> Environment<'_> {
        Environment {
            base_name: OsStr::new(base_name),
            uid,
            snap_common: None,
            xdg_config_home: None,
            home: None,
            tmpdir: None,
        }
    }

    fn all_reachable(_: &Path) -> bool {
        true
    }

    fn none_reachable(_: &Path) -> bool {
        false
    }

    /// The shipped case: an ordinary login session with no `XDG_CONFIG_HOME`,
    /// which is where the file the packaged unit has been writing since
    /// 2.0.0~alpha-4 actually is. Getting this one wrong would lose a user's
    /// printers on upgrade, which is why it is asserted against the literal
    /// path rather than against a recomputation of it.
    #[test]
    fn a_login_session_keeps_its_state_under_dot_config() {
        let home = OsString::from("/home/dev");
        let mut environment = env("ml216x-printer-app", 1000);
        environment.home = Some(&home);
        assert_eq!(
            state_file(&environment, all_reachable, |_| false),
            PathBuf::from("/home/dev/.config/ml216x-printer-app.state")
        );
    }

    /// `XDG_CONFIG_HOME` wins over `HOME`, and is used as given: this is what
    /// every probe script under `scripts/` relies on to keep a run out of the
    /// developer's own state.
    #[test]
    fn an_explicit_xdg_config_home_is_used_as_given() {
        let xdg = OsString::from("/tmp/probe/config");
        let home = OsString::from("/home/dev");
        let mut environment = env("ml216x-printer-app", 1000);
        environment.xdg_config_home = Some(&xdg);
        environment.home = Some(&home);
        assert_eq!(
            state_file(&environment, all_reachable, |_| false),
            PathBuf::from("/tmp/probe/config/ml216x-printer-app.state")
        );
    }

    /// Root does not use a home directory at all. The packaged service has run
    /// in the user's session since Q-18, but the 1.x package ran as root and a
    /// hand-run `sudo` server still reaches this branch.
    #[test]
    fn root_keeps_its_state_in_var_lib() {
        assert_eq!(
            state_file(&env("ml216x-printer-app", 0), all_reachable, |_| false),
            PathBuf::from("/var/lib/ml216x-printer-app.state")
        );
    }

    /// A directory PAPPL cannot search is not a directory it abandons: it
    /// tries to create it, and `mkdir` failing with `EEXIST` leaves the path
    /// in place. Only a directory that is neither there nor creatable sends
    /// the file to the temporary directory.
    #[test]
    fn an_unsearchable_directory_is_still_used_if_it_exists() {
        assert_eq!(
            state_file(&env("ml216x-printer-app", 0), none_reachable, |_| true),
            PathBuf::from("/var/lib/ml216x-printer-app.state")
        );
        assert_eq!(
            state_file(&env("ml216x-printer-app", 0), none_reachable, |_| false),
            PathBuf::from("/tmp/ml216x-printer-app0.state")
        );
    }

    /// The last resort carries the uid, because `/tmp` is shared. TMPDIR is
    /// used when it is usable — Q-19 has already pointed it at
    /// `$XDG_RUNTIME_DIR` by the time this runs — and `/tmp` otherwise.
    #[test]
    fn the_last_resort_is_the_temporary_directory_with_the_uid() {
        let tmpdir = OsString::from("/run/user/1000");
        let mut environment = env("ml216x-printer-app", 1000);
        environment.tmpdir = Some(&tmpdir);
        assert_eq!(
            state_file(&environment, all_reachable, |_| false),
            PathBuf::from("/run/user/1000/ml216x-printer-app1000.state")
        );
        environment.tmpdir = Some(OsStr::new(""));
        assert_eq!(
            state_file(&environment, all_reachable, |_| false),
            PathBuf::from("/tmp/ml216x-printer-app1000.state")
        );
    }

    /// A variable that is set but unusable stops the chain where PAPPL stops
    /// it. `SNAP_COMMON` set to something unreachable sends the file to the
    /// temporary directory; it does not fall through to `HOME`.
    #[test]
    fn a_set_but_unusable_variable_does_not_fall_through() {
        let snap = OsString::from("/snap/common");
        let home = OsString::from("/home/dev");
        let mut environment = env("ml216x-printer-app", 1000);
        environment.snap_common = Some(&snap);
        environment.home = Some(&home);
        assert_eq!(
            state_file(&environment, none_reachable, |_| false),
            PathBuf::from("/tmp/ml216x-printer-app1000.state")
        );
        assert_eq!(
            state_file(&environment, all_reachable, |_| false),
            PathBuf::from("/snap/common/ml216x-printer-app.state")
        );
    }

    #[test]
    fn the_file_is_named_after_the_program() {
        assert_eq!(
            base_name(OsStr::new("/usr/bin/ml216x-printer-app")),
            "ml216x-printer-app"
        );
        assert_eq!(base_name(OsStr::new("target/debug/app")), "app");
        assert_eq!(base_name(OsStr::new("app")), "app");
    }
}
