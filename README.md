# ml216x-printer-app

A PAPPL Printer Application for the Samsung ML-2160, ML-2165, ML-2165W and
ML-2168 protocol family. It accepts jobs over IPP and emits SPL2/QPDL through
PAPPL device transports. File and loopback socket output have been verified;
the maintainer also reports successful hardware printing and CUPS network
sharing. The package includes a USB auto-configuration exception for the
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
binary at `/usr/bin/ml216x-printer-app`, a systemd unit at
`/usr/lib/systemd/system/ml216x-printer-app.service`, and the usual
documentation under `/usr/share/doc/`. It installs **no CUPS filter and no
PPD** — a printer application is driven over IPP, so nothing needs to live in
CUPS' filter directory. The 1.x filter and the PPD stay in the source tree and
can be built explicitly for regression testing.

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
`dist/samsung-ml2160-rust_<version>_<arch>.deb`. The metadata it packs comes
from `packaging/debian/`: `control`, `copyright`, `changelog` and the three
maintainer scripts.

### Install it

```sh
sudo apt install ./dist/samsung-ml2160-rust_2.0.0~alpha-3_amd64.deb
systemctl status ml216x-printer-app
```

Use `apt install ./…` rather than `dpkg -i`, so the library dependencies are
resolved. Installing enables and starts the service; it listens on the loopback
address at port 8631 and advertises itself over DNS-SD if `avahi-daemon` is
running. Until you add a printer it does nothing else.

### Add your printer

The service knows how to talk to the printer; it does not know where the
printer is. That is one command, and the device URI is yours to choose:

```sh
sudo ml216x-printer-app devices          # what is attached, if anything
sudo ml216x-printer-app add -d ML2160 -m samsung_ml216x -v "$DEVICE_URI"
```

`$DEVICE_URI` is a `usb://…` line copied from `devices`, or
`socket://<printer-ip>:9100` for a network model — these printers speak no IPP
of their own, only raw JetDirect. Device IDs are self-reported; choose the
intended printer explicitly.
Set `DEVICE_URI` to that URI before running `add`.

The queue then appears to CUPS and to every other IPP client as
`ipp://localhost:8631/ipp/print/ML2160`, and CUPS discovers it over DNS-SD
without a PPD.

### Remove it

```sh
sudo apt remove samsung-ml2160-rust     # stops and disables the service
sudo apt purge samsung-ml2160-rust      # also drops /var/lib/ml216x-printer-app.state
```

`remove` leaves the printers you added on disk, so reinstalling brings them
back; `purge` is what forgets them.

## USB auto-configuration on Debian GNOME

For Samsung USB ID `04e8:330f`, the package cancels the legacy queue-creation
service requested by Debian's `70-printers.rules`. Physical USB access remains
available to PAPPL. This is a device-specific desktop integration rule, not
an installation of a PPD or automatic registration of a new PAPPL printer.
Other USB IDs are unaffected; do not extrapolate the rule to an entire vendor.
The existing IPP queue must still be configured as described above.

When upgrading from alpha-2, first ensure the IPP queue works, then remove
only the duplicate USB queue (use its actual name):

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
- `crates/ml216x-printer-app/` — the 2.0 binary: the SPL2 raster driver and the
  capability table
- `src/main.rs`, `src/golden.rs` — the frozen 1.x CUPS filter front end and the
  golden-file harness that pins its output
- `ppd/samsung-ml2160.ppd` — CUPS PPD for the 1.x queue; kept permanently as
  project data
- `packaging/debian/` — `control`, `copyright`, `changelog` and the maintainer
  scripts; `packaging/systemd/` — the service unit
- `scripts/` — `build-deb.sh` builds the package; `p5-probe.py` and
  `transport-probe.py` drive the printer application over loopback

## License

The driver and SPL2/QPDL engine are GPL-2.0-only, derived from OpenPrinting
SpliX; see [LICENSE](LICENSE). The `pappl-sys` and `pappl` crates are
[Apache-2.0](LICENSE-APACHE) OR [MIT](LICENSE-MIT).
