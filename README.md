# ml216x-printer-app

A PAPPL Printer Application for the Samsung ML-2160, ML-2165, ML-2165W and
ML-2168 protocol family. It accepts jobs over IPP and emits SPL2/QPDL through
PAPPL device transports, and it runs entirely in user space: a systemd user
service in your own session, as your own user, with no root process, no setuid
binary and no `sudo` in any of the commands below. File and loopback socket
output have been verified; the maintainer also reports successful hardware
printing and CUPS network sharing. The package includes a USB auto-configuration exception for the
hardware-confirmed `04e8:330f` device to prevent duplicate legacy queues;
see [hardware test notes](docs/HARDWARE-TESTING.md).

This is a development alpha. The selected **12.5 pt (4.41 mm)** margins still
need hardware measurement (G-1), including vertical placement (Q-13).
The [P9 security review](docs/SECURITY-REVIEW.md) reproduced two memory
corruption bugs in the tested libpappl dependency. Loopback binding limits
network exposure but does not fix these bugs. See the
[current migration state](docs/SESSION-STATE.md) for outstanding work.

## Workspace

| Crate | Licence | What it is |
|---|---|---|
| `spl2-core` | GPL-2.0-only | The SPL2/QPDL v3 engine: PJL envelope, page header, Algo 0x11 bands. No C or dependencies; output uses a caller-supplied writer. Byte-for-byte frozen by `goldens/`. |
| `pappl-sys` | Apache-2.0 OR MIT | Hand-written FFI to libpappl, with a C layout probe checking every offset. |
| `pappl` | Apache-2.0 OR MIT | The safe wrapper: RAII handles, the `catch_unwind` callback shim, and the `RasterDriver` seam. |
| `ml216x-printer-app` | GPL-2.0-only | The printer application: capability table and the SPL2 driver. |
| `rastertospl-rust` (root) | GPL-2.0-only | Legacy reference front end and golden harness; built explicitly. |

## Build

Dependencies: Rust 1.77+, `pkg-config`, a C compiler, `libpappl-dev`
(>= 1.3, < 2.0; tested with 1.3.1), and `libcups2-dev`.

```sh
cargo build --release
```

The default target is `target/release/ml216x-printer-app`. For the old
converter, regression harness, or removal of an old manual installation, see
[Legacy CUPS filter reference](docs/LEGACY-FILTER.md).

## Debian package (.deb)

The package installs the **2.0 printer application**, not the 1.x filter: one
binary at `/usr/bin/ml216x-printer-app`, a systemd **user** unit at
`/usr/lib/systemd/user/ml216x-printer-app.service`, a udev rule at
`/usr/lib/udev/rules.d/71-ml216x-printer-app.rules`, and the usual
documentation under `/usr/share/doc/`. It installs **no CUPS filter and no
PPD** — a printer application is driven over IPP, so nothing needs to live in
CUPS' filter directory. The 1.x filter and the PPD stay in the source tree and
can be built explicitly for regression testing.

Installing the package is the only step that needs root, and only because apt
writes to `/usr`. Nothing it installs runs as root: the service is started by
your own `systemd --user`, PAPPL keeps its state in
`$XDG_CONFIG_HOME/ml216x-printer-app.state` (`~/.config/…` by default), and its
control socket and spool in `$XDG_RUNTIME_DIR`, which is yours alone. The
socket goes there rather than into `/tmp` on purpose: libpappl creates it
world-connectable, and its subcommands are the server's whole control surface
(decision Q-19). Up to
2.0.0~alpha-3 this was a root system service; see decision Q-18 in
[`docs/DECISIONS.md`](docs/DECISIONS.md) for what changed and why.

The binary is dynamically linked against the archive's `libpappl1t64` and
glibc. The musl static build the 1.x package used is gone: vendoring or
statically linking a C library would take the package out of apt's security
updates and make this project the response path for libpappl's CVEs.

> [!NOTE]
> This is an alpha of the 2.0 line. The hard margins have not been measured on
> paper yet — release gate G-1 in [`docs/GOLDEN-VALIDATION.md`](docs/GOLDEN-VALIDATION.md)
> — so treat printed output as unverified until that gate is closed.

### Build it

```sh
./scripts/build-deb.sh
```

`dpkg-dev` is still not required — `dpkg-deb` comes with `dpkg` itself. The
script checks that `libpappl-dev` is present and inside the `>= 1.3, < 2.0`
range before it builds anything, so a wrong library version fails with a
sentence rather than with a link error, and it writes
`dist/ml216x-printer-app_<version>_<arch>.deb`. The metadata it packs comes
from `packaging/debian/`: `control`, `copyright`, `changelog` and the three
maintainer scripts.

### Install it

```sh
sudo apt install ./dist/ml216x-printer-app_2.0.0~alpha-5_amd64.deb
systemctl --user start ml216x-printer-app     # or just log out and back in
systemctl --user status ml216x-printer-app
```

Use `apt install ./…` rather than `dpkg -i`, so the library dependencies are
resolved. The package was called `samsung-ml2160-rust` up to 2.0.0~alpha-4;
both install the same binary and cannot be co-installed, so apt removes the old
one as part of this — which is also what stops and disables the root service it
used to run. Printers you had added are untouched: they live in
`~/.config/ml216x-printer-app.state`. Note the `--user` in every `systemctl` line: installing runs
`systemctl --global enable`, which enables the service in every user's own
service manager, so it starts by itself at the next login. It cannot be
started from the package into a session that is already open, which is what
the explicit `start` above is for.

Once running it listens on the loopback address at port 8631 and advertises
itself over DNS-SD if `avahi-daemon` is running. Until you add a printer it
does nothing else.

Two consequences of running in your session are worth knowing before you rely
on it:

* **It stops when your session ends.** If the machine shares the queue to the
  network through CUPS and should keep doing so while nobody is logged in,
  enable lingering once: `sudo loginctl enable-linger $USER`. The user manager
  then starts at boot and the service with it.
* **One user at a time on port 8631.** A second user logging in gets a service
  that cannot bind, and it restarts on a five-second loop. Give that user
  another port with a drop-in — `systemctl --user edit ml216x-printer-app`:

  ```ini
  [Service]
  ExecStart=
  ExecStart=/usr/bin/ml216x-printer-app --listen-port 8632 \
      --spool-directory %t/ml216x-printer-app server
  ```

### Add your printer

The service knows how to talk to the printer; it does not know where the
printer is. That is one command, and the device URI is yours to choose:

```sh
ml216x-printer-app devices          # what is attached, if anything
ml216x-printer-app add -d ML2160 -m samsung_ml216x -v "$DEVICE_URI"
```

No `sudo`: these subcommands reach the server over a socket in your runtime
directory — `/run/user/$(id -u)/`, which only you can enter — and both ends are
your own user. Running them under `sudo` would look for root's server instead
and find nothing. If you run one where neither `TMPDIR` nor `XDG_RUNTIME_DIR`
is set, such as a cron job or a session-less `su`, it prints a warning and
reports that no server is running; export the runtime directory first:

```sh
export XDG_RUNTIME_DIR=/run/user/$(id -u)
```

`$DEVICE_URI` is a `usb://…` line copied from `devices`, or
`socket://<printer-ip>:9100` for a network model — these printers speak no IPP
of their own, only raw JetDirect. Device IDs are self-reported; choose the
intended printer explicitly.
Set `DEVICE_URI` to that URI before running `add`.

The queue then appears to CUPS and to every other IPP client as
`ipp://localhost:8631/ipp/print/ML2160`, and CUPS discovers it over DNS-SD
without a PPD.

If `devices` lists nothing while the printer is plugged in, the USB node's
permissions are the thing to check; see
[USB access without root](#usb-access-without-root) below.

### Remove it

```sh
systemctl --user stop ml216x-printer-app   # in each session that runs it
sudo apt remove ml216x-printer-app         # disables it for future logins
sudo apt purge ml216x-printer-app          # also drops the old root service's state
```

Stop it yourself first: removal runs `systemctl --global disable`, which stops
it from starting at the next login, but root cannot reach into an open session
to stop a running user service.

`remove` leaves the printers you added on disk, so reinstalling brings them
back. `purge` deletes only what the pre-alpha-4 **root** service owned —
`/var/lib/ml216x-printer-app.state` and `/var/spool/ml216x-printer-app`. Your
own printers are yours: they live in `~/.config/ml216x-printer-app.state`, and
no package script touches a home directory. Delete that file to forget them.

## USB access without root

Debian's `50-udev-default.rules` gives a USB printer-class device
`root:lp 0664`. That is what CUPS' own backend needs, because it runs as root;
a printer application running as you is not in group `lp` and cannot open
`/dev/bus/usb/…` at all. So the package's udev rule tags the
hardware-confirmed `04e8:330f` device `uaccess`, and `systemd-logind` puts an
ACL for the user of the active local session on that node. Nothing else is
changed: not the owner, not the mode, not any other device.

This works for someone logged in at the machine. It does not work over SSH
with nobody at the console, because there is no active local session to grant
the ACL to. For a headless machine, put the user in group `lp` instead and
have them log in again:

```sh
sudo adduser "$USER" lp
```

Group `lp` is the wider grant of the two — it reaches every USB printer on the
machine, not just this one — which is why it is the fallback rather than the
default.

## USB auto-configuration on Debian GNOME

For Samsung USB ID `04e8:330f`, the package also cancels the legacy
queue-creation service requested by Debian's `70-printers.rules`. This is a
device-specific desktop integration rule, not an installation of a PPD or
automatic registration of a new PAPPL printer.
Other USB IDs are unaffected; do not extrapolate the rule to an entire vendor.
The existing IPP queue must still be configured as described above.

When upgrading from alpha-2 or alpha-3, first ensure the IPP queue works, then
remove only the duplicate USB queue (use its actual name):

```sh
lpstat -v
sudo lpadmin -x ML-2160-Series
```

Keep `ML2160`, whose URI is `ipp://127.0.0.1:8631/ipp/print/ML2160`.
The package reloads udev rules; reconnect or power-cycle the printer after
upgrading. Confirm that the duplicate queue and driver-search notification
stay absent, and that printing still works. This hardware retest is pending.
Removing the package restores the distribution's normal USB auto-setup rules.

## Print options

Use IPP options with a CUPS queue connected to the application's IPP URI:

```sh
lp -d ML2160 -o media=iso_a4_210x297mm -o printer-resolution=600dpi document.pdf
lp -d ML2160 -o media=iso_c5_162x229mm -o media-source=manual -o media-type=envelope envelope.pdf
lpoptions -d ML2160 -l
```

`ML2160` here is the client-side CUPS queue name. The application exposes
`auto` and `manual` sources, eleven media sizes, and 300x300, 600x600,
1200x600 and 1200x1200 DPI. Normal quality defaults to 600 DPI.
The complete IPP media-type mapping is in
[`media_table.rs`](crates/ml216x-printer-app/src/media_table.rs).

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
(cd goldens && sha256sum -c SHA256SUMS)
```

The workspace suite includes the legacy parser and 32 golden SPL streams,
shared engine tests, FFI layout checks, and application capability tests.
Always use `--workspace` to include the reference harness.

Integration probes require `ipptool` (`cups-ipp-utils`) and a debug build:

```sh
cargo build
python3 scripts/p5-probe.py --output /tmp/p5-measurements.json
python3 scripts/p5-probe.py --spl --output /tmp/p5-spl.json
python3 scripts/p5-probe.py --device-failure --output /tmp/p5-failure.json
python3 scripts/transport-probe.py
```

These use temporary state and local destinations. The P5 probe checks raster
geometry, SPL page headers and write failure handling; the transport probe
compares file and socket output and checks print-quality resolution selection.
They do not replace a hardware print. See [margin measurements](docs/MARGINS.md)
and [golden validation](docs/GOLDEN-VALIDATION.md).

## Project Structure

- `crates/spl2-core/` — the protocol engine, shared byte for byte by both front
  ends: `qpdl.rs` (PJL envelope, page/band records, Algo 0x11 RLE), `geometry.rs`
  and `engine.rs` (the SpliX geometry rules), `raster.rs` (the CUPS Raster
  V1/V2/V3 parser, behind the `golden-replay` feature)
- `crates/pappl-sys/`, `crates/pappl/` — hand-written FFI for libpappl 1.3 and
  the safe wrapper that owns every `unsafe` line and the callback boundary
- `crates/ml216x-printer-app/` — the 2.0 binary: the SPL2 raster driver, the
  capability table, and `runtime.rs`, which keeps the control socket out of
  a shared directory (Q-19)
- `src/main.rs`, `src/golden.rs` — the frozen 1.x CUPS filter front end and the
  golden-file harness that pins its output
- `ppd/samsung-ml2160.ppd` — CUPS PPD for the 1.x queue; kept permanently as
  project data
- `packaging/debian/` — `control`, `copyright`, `changelog` and the maintainer
  scripts; `packaging/systemd/user/` — the user unit the package installs
  (`packaging/systemd/ml216x-printer-app.service` is the retired root system
  unit, kept as a record and no longer installed); `packaging/udev/` — the
  device-scoped rules
- `scripts/` — `build-deb.sh` builds the package; `p5-probe.py` and
  `transport-probe.py` drive the printer application over loopback

## License

The driver and SPL2/QPDL engine are GPL-2.0-only, derived from OpenPrinting
SpliX; see [LICENSE](LICENSE). The `pappl-sys` and `pappl` crates are
[Apache-2.0](LICENSE-APACHE) OR [MIT](LICENSE-MIT).
