# ml216x-printer-app

[![checks](https://github.com/caganerg/ml216x-printer-app/actions/workflows/checks.yml/badge.svg)](https://github.com/caganerg/ml216x-printer-app/actions/workflows/checks.yml)

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
need hardware measurement (G-1), including vertical placement (Q-13). **A green
badge above does not mean the margins are right**: every check is software, and
what it cannot cover is listed in [docs/CI.md](docs/CI.md).
The [P9 security review](docs/SECURITY-REVIEW.md) reproduced two memory
corruption bugs in libpappl 1.3.1, the version Debian ships in stable, testing
and unstable alike, and
the network listener makes them reachable by network clients: on that library,
use a trusted network and restrict access to TCP port 8631. **Both are fixed in
upstream PAPPL 1.4.12**, which this application also supports and is tested
against; it is not packaged by Debian, so [running it is a source
build](#optional-pappl-1412-instead-of-the-archives-131). See the
[current migration state](docs/SESSION-STATE.md) for outstanding work.

## Workspace

| Crate | Licence | What it is |
|---|---|---|
| `spl2-core` | GPL-2.0-only | The SPL2/QPDL v3 engine: PJL envelope, page header, Algo 0x11 bands. No C or dependencies; output uses a caller-supplied writer. Byte-for-byte frozen by `goldens/`. Behind `golden-replay` it also holds `replay.rs`, the 1.x filter's page loop, which the corpus is replayed through. |
| `pappl-sys` | MIT | Hand-written FFI to libpappl, with a C layout probe checking every offset. |
| `pappl` | MIT | The safe wrapper: RAII handles, the `catch_unwind` callback shim, and the `RasterDriver` seam. |
| `ml216x-printer-app` | GPL-2.0-only | The printer application: capability table and the SPL2 driver. |

## Build

Dependencies: Rust 1.77+, `pkg-config`, a C compiler, `libpappl-dev`
(>= 1.3, < 2.0; tested against both 1.3.1 and 1.4.12), and `libcups2-dev`.

```sh
cargo build --release
```

The default target is `target/release/ml216x-printer-app`. The 1.x CUPS filter
that preceded it was deleted at gate P11 (`docs/P11-DELETE-LIST.md`); it is
still buildable from the annotated tag `v1.x-final`, and what it produced is
still pinned here — the corpus in `goldens/` is replayed through
`crates/spl2-core/src/replay.rs`, which is that filter's own page loop.

### Optional: PAPPL 1.4.12 instead of the archive's 1.3.1

Two libpappl releases are supported (decision Q-27 in
[`docs/DECISIONS.md`](docs/DECISIONS.md)): the **1.3.1** in `libpappl-dev`,
which needs nothing extra, and upstream **1.4.12**, which fixes four of the
six libpappl defects in the security review — including the two memory
corruption bugs reachable over IPP. Debian packages no 1.4.x at all — stable,
testing and unstable all carry 1.3.1-2.1 and experimental carries nothing — so
1.4.12 has to be built:

```sh
sudo apt install build-essential libavahi-client-dev libcups2-dev \
    libgnutls28-dev libjpeg-dev libpam0g-dev libpng-dev libusb-1.0-0-dev \
    zlib1g-dev
sudo ./scripts/install-pappl-1.4.sh          # into /usr/local
```

The script checks the tarball against a recorded sha256 before building. Then
build this tree against it — both variables, because one decides what the
crate compiles against and the other what it loads at run time:

```sh
export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig
export LD_LIBRARY_PATH=/usr/local/lib
pkg-config --modversion pappl     # 1.4.12
cargo build --release
```

Only the machine that **runs** the application needs this. A second computer
printing to the queue speaks IPP and needs no libpappl at all.

Two things worth knowing. Every PAPPL 1.x has soname `libpappl.so.1`, so
nothing warns you if you compile against one release and run against the
other; it is safe here — the structs are identical and the driver reads the
page numbering off each job rather than assuming a release — but it does mean
`LD_LIBRARY_PATH` (or `ldconfig`) is what actually selects the library. And a
`.deb` built this way still declares the archive's `libpappl1t64`, because the
package targets the archive; the 1.4.12 build is for running from the source
tree.

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
updates and make this project the response path for libpappl's CVEs. That
applies to 1.4.12 as much as to 1.3.1: the package depends on the archive's
library either way, and the 1.4.12 under
[Build](#optional-pappl-1412-instead-of-the-archives-131) is something you
install alongside that library rather than something shipped inside the
`.deb`.

> [!NOTE]
> This is an alpha of the 2.0 line. The hard margins have not been measured on
> paper yet — release gate G-1 in [`docs/GOLDEN-VALIDATION.md`](docs/GOLDEN-VALIDATION.md)
> — so treat printed output as unverified until that gate is closed. The
> procedure and the record form are ready in
> [`docs/G1-MEASUREMENT.md`](docs/G1-MEASUREMENT.md); taking the measurement
> needs a printer, paper and a millimetre rule.

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
sudo apt install ./dist/ml216x-printer-app_2.0.0~alpha-8_amd64.deb
systemctl --user restart ml216x-printer-app   # also replaces a running older version
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
the explicit `restart` above is for.

Once running it serves IPP on all IPv4/IPv6 interfaces at port 8631 and uses
PAPPL's normal DNS-SD announcements (Q-26). With `avahi-daemon` running,
clients can discover the application directly; a separate CUPS sharing queue
is optional. The local web interface is `http://localhost:8631/`.
Remote administration remains disabled. IPP transport currently has TLS
disabled, so use this service on a trusted network. Until you add a printer,
there is no printer destination to use.

Upgrading from alpha-7 preserves the same PAPPL state file and existing CUPS
queues. Restart the user service after installing. No printer recreation or
manual state-file edit is needed.

Two consequences of running in your session are worth knowing before you rely
on it:

* **It stops when your session ends.** If the machine should serve the printer on the
  network while nobody is logged in,
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
systemctl --user start ml216x-printer-app   # if it is not running already
ml216x-printer-app devices          # what is attached, if anything
ml216x-printer-app add -d ML2160 -m samsung_ml216x -v "$DEVICE_URI"
```

`add` needs a running server; `devices` does not, because it only enumerates
what is attached. Installing the package enables the service in every user's
own `systemd --user` manager, so a **new login starts it by itself** — but a
session that was already open when the package landed has to be told once, as
above. Root cannot do this for you: a user service belongs to your session, and
`postinst` runs outside it. That is the trade for a server that never runs as
root.

If the server is not running, `add` fails with a message that names the wrong
problem:

```
ml216x-printer-app: Unable to start server: No such file or directory
```

That comes from PAPPL trying to start a server for you. It spawns the path it
was invoked with, so the file it cannot find is this program itself, reached by
bare name through `PATH` rather than by an absolute path. Start the service and
run `add` again.

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

The queue then appears to every IPP client as
`ipp://localhost:8631/ipp/print/ML2160`, without a PPD.

### Discover and use the printer

Use your client's driverless printer selection to choose the announced
Samsung printer. CUPS clients can enumerate it with `lpstat -e`; DNS-SD
requires Avahi and working multicast DNS on the network, but does not require
`cups-browsed` to create a permanent queue.

For a direct network connection, use
`ipp://<server-hostname>:8631/ipp/print/ML2160`. CUPS on port 631 is not needed
as an intermediary. On PAPPL 1.3.1, after adding a printer with the CLI,
restart once to trigger its startup advertisement:

```sh
systemctl --user restart ml216x-printer-app
lpstat -e
```

The web interface's Add Printer form also uses PAPPL's normal discovery
behaviour; this application no longer clears names on restart.

### Optional permanent CUPS queue

If you want a fixed local queue name or your client lacks discovery, create
one explicitly while the application is running:

```sh
sudo lpadmin -p ML2160 -E -v ipp://127.0.0.1:8631/ipp/print/ML2160 -m everywhere
lpoptions -d ML2160
```

`-m everywhere` asks the IPP service for its capabilities. Existing queues
using this URI continue to work. Sharing this CUPS queue is optional; it
introduces another advertised service on port 631 alongside PAPPL on 8631.

### Understand additional printer entries

A discovered destination in `lpstat -e` is not necessarily a configured queue
in `lpstat -p` or `lpstat -v`. A name such as `ML2160_thinkcentre` alone does
not identify the component that created it. Inspect its URI or DNS-SD port:
8631 is this application, 631 is CUPS. Multiple names do not send a job twice.
Keep a preferred default if both an explicit queue and a discovered destination
appear. Do not globally disable Avahi or cups-browsed just to hide an entry.

A separate legacy USB queue is a different issue:

* **Hotplug.** `system-config-printer-udev`'s `70-printers.rules` asks systemd
  to configure a queue whenever a USB printer is plugged in, which is where a
  `usb://…` queue — and another one on the next replug — comes from. The
  package's own udev rule cancels that request, but **only for USB ID
  `04e8:330f`**, the device this was confirmed against. Check what your printer
  reports; if the ID differs, the rule does not match it and the queues will
  keep coming back:

  ```sh
  lsusb | grep -i samsung        # expected: ID 04e8:330f
  ```

To remove a redundant legacy USB queue, check the URI
rather than the name, and delete only those pointing at `usb://…`:

```sh
lpstat -v
sudo lpadmin -x ML-2160-Series
```

Deleting a queue does not touch the printer you added to the application, which
lives in `~/.config/ml216x-printer-app.state`.

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
./scripts/run-checks.sh          # every group, the same list CI runs
./scripts/run-checks.sh --list   # what each group covers
```

Or by hand:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
(cd goldens && sha256sum -c SHA256SUMS)
```

Everything above runs against whichever libpappl is installed. To run it
against the other supported release, set `PKG_CONFIG_PATH` and
`LD_LIBRARY_PATH` as under
[Build](#optional-pappl-1412-instead-of-the-archives-131); CI does both on
every push.

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
python3 scripts/g1-probe.py --all
```

These use temporary state and local destinations. The P5 probe checks raster
geometry, SPL page headers and write failure handling; the transport probe
compares file and socket output and checks print-quality resolution selection;
the G-1 probe prints the measurement page of
[`docs/G1-MEASUREMENT.md`](docs/G1-MEASUREMENT.md) and decodes the QPDL back
into a bitmap to confirm every ruler tick landed on the predicted pixel, on
all 11 media and 4 resolutions. Each one can be made to fail on demand
(`--inject`); a harness that has never gone red is not evidence.
They do not replace a hardware print. See [margin measurements](docs/MARGINS.md)
and [golden validation](docs/GOLDEN-VALIDATION.md).

## Project Structure

- `crates/spl2-core/` — the protocol engine, shared byte for byte by both front
  ends: `qpdl.rs` (PJL envelope, page/band records, Algo 0x11 RLE), `geometry.rs`
  and `engine.rs` (the SpliX geometry rules), `raster.rs` (the CUPS Raster
  V1/V2/V3 parser, behind the `golden-replay` feature)
- `crates/pappl-sys/`, `crates/pappl/` — hand-written FFI for libpappl 1.3 and
  1.4 and the safe wrapper that owns every `unsafe` line and the callback
  boundary
- `crates/ml216x-printer-app/` — the 2.0 binary: the SPL2 raster driver, the
  capability table, and `runtime.rs`, which keeps the control socket out of
  a shared directory (Q-19)
- `ppd/samsung-ml2160.ppd` — CUPS PPD for the 1.x queue; kept permanently as
  project data
- `packaging/debian/` — `control`, `copyright`, `changelog` and the maintainer
  scripts; `packaging/systemd/user/` — the user unit the package installs
  (`packaging/systemd/ml216x-printer-app.service` is the retired root system
  unit, kept as a record and no longer installed); `packaging/udev/` — the
  device-scoped rules
- `scripts/` — `run-checks.sh` is the whole check list, and what CI runs;
  `build-deb.sh` builds the package; `install-pappl-1.4.sh` builds the optional
  upstream libpappl 1.4.12; `p5-probe.py`, `transport-probe.py`,
  `server-probe.py`, `security-probe.py` and `g1-probe.py` drive the printer
  application over loopback, and `g1-page.c` generates the G-1 measurement page

## License

The driver and SPL2/QPDL engine are GPL-2.0-only, derived from OpenPrinting
SpliX; see [LICENSE](LICENSE). The `pappl-sys` and `pappl` crates are
[MIT](LICENSE-MIT), which is what keeps them linkable into the GPL-2.0-only
binary while staying reusable elsewhere.
