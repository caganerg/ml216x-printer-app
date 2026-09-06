# samsung-ml2160-rust
> **2.0 development:** the SPL2 engine is now a crate (`spl2-core`) shared by
> the frozen 1.x filter and the PAPPL printer application, and the raster
> callbacks emit real SPL2/QPDL. There is still **no device transport**, so the
> application can only print to a `file://` destination, and no page has been
> printed on hardware: the **12.5 pt** margins are the maintainer's selection,
> not a measurement (release gate G-1). The original 1.x release remains on
> `legacy/cups-filter-1.x` / `v1.x-final`.

## Workspace

| Crate | Licence | What it is |
|---|---|---|
| `spl2-core` | GPL-2.0-only | The SPL2/QPDL v3 engine: PJL envelope, page header, Algo 0x11 bands. No C, no I/O, no dependencies. Byte-for-byte frozen by `goldens/`. |
| `pappl-sys` | Apache-2.0 OR MIT | Hand-written FFI to libpappl, with a C layout probe checking every offset. |
| `pappl` | Apache-2.0 OR MIT | The safe wrapper: RAII handles, the `catch_unwind` callback shim, and the `RasterDriver` seam. |
| `ml216x-printer-app` | GPL-2.0-only | The printer application: capability table and the SPL2 driver. |
| `rastertospl-rust` (root) | GPL-2.0-only | The frozen 1.x CUPS filter front end. |

## PAPPL development (P5/P6)

Build dependencies: Rust 1.77+, `pkg-config`, a C compiler, `libpappl-dev`
(1.3.x; tested with 1.3.1), and `libcups2-dev`. Run the integration experiment
with `ipptool` from `cups-ipp-utils` installed:

```sh
cargo build -p ml216x-printer-app
python3 scripts/p5-probe.py --output /tmp/p5-measurements.json
python3 scripts/p5-probe.py --spl --output /tmp/p5-spl.json
python3 scripts/p5-probe.py --device-failure --output /tmp/p5-failure.json
```

The default run reproduces `docs/P5-MEASUREMENTS.json` byte for byte. `--spl`
runs the same 17 media/resolution cases through the SPL2 driver and checks the
QPDL page header each job produced.

For manual inspection, explicitly start the probe server in one terminal:

```sh
./target/debug/ml216x-printer-app --probe \
  --probe-output /tmp/ml216x-probe.jsonl --listen-port 8631 server
```

Submit a matching PWG file from another terminal, specifying the resolution
and medium explicitly:

```sh
./target/debug/ml216x-printer-app submit \
  -u ipp://127.0.0.1:8631/ipp/print/probe \
  -o printer-resolution=600dpi -o media=iso_a4_210x297mm page.pwg
```

`--probe` writes JSON Lines and permits only `file:///` destinations; without
it the same command emits SPL2/QPDL to whatever destination the URI names. Both
bind TCP to 127.0.0.1. Stop the server with Ctrl-C or the `shutdown`
subcommand and the same `-u` server URI. PAPPL's auto-start command does not
preserve these custom flags, so use the explicit `server` command above.

The 17-case experiment measured **full-media raster with zero header margins**,
so the driver subtracts the hard margin on both axes before handing the page to
the engine; the horizontal half is asserted to place the sheet exactly as the
classic filter does, and the vertical half is open question Q-13. See
[margin decision and measured results](docs/MARGINS.md),
[the decision log](docs/DECISIONS.md) and
[current migration state](docs/SESSION-STATE.md). P9 must still review raster
conversion/dithering and input-versus-job geometry before real printing.

## Transitional CUPS filter

The instructions below describe the classic filter, which is still built by
`cargo build` and still frozen in the tree. The Debian package no longer ships
it: [`.deb`](#debian-package-deb) now installs the printer application. The
updated PPD must be reloaded into an existing queue when testing 12.5 pt.

A CUPS raster filter (`rastertospl-rust`) for Samsung ML-2160 series monochrome laser printers, written in Rust. It converts CUPS's standard raster stream (`RaSt`/`RaS2`/`RaS3`) into the printer's native binary **SPL2 / QPDL v3** format: PJL job envelope, 17-byte page header, Algo 0x11 RLE-compressed band records, and checksums.

The protocol implementation was verified against the actual source of the [OpenPrinting SpliX](https://github.com/OpenPrinting/splix) driver (`document.cpp`, `compress.cpp`, `qpdl.cpp`, `algo0x11.cpp`, `printer.cpp`) and tested on real hardware.

## Supported Models

ML-2160, ML-2165, ML-2165W, ML-2168 (same QPDL v3 protocol family).

## Requirements

- Rust toolchain (`cargo`)
- CUPS (`lpadmin`, `lpinfo`, `cupstestppd`)
- Printer powered on and reachable — over USB, or over the network on the JetDirect port (9100)

## Installation

There is no install script — the steps below *are* the procedure. Run them from
the repository root as your normal user: only the two `sudo` lines write to
system paths, so **don't run the whole sequence as root**.

### 1. Check that the repository path is under your control

```sh
namei -l .
```

Every component from `/` down to the repository must be owned by you or by
root, and none of them may be group- or world-writable (a sticky directory such
as `/tmp` is acceptable for the parents, since only an entry's owner can replace
it there). Check the build inputs too — `src/`, `Cargo.toml` and `Cargo.lock`
end up compiled into the binary, and `ppd/` into the installed PPD:

```sh
find . -path ./target -prune -o -perm /022 -print
```

This is not boilerplate caution. Steps 3 and 4 copy the filter binary and the
PPD into system locations **as root**, and a PPD is not a passive config file: its
`*cupsFilter`/`*cupsFilter2` line names the program CUPS executes for every
print job, as user `lp`. Anyone who can write into the repository — or replace
any directory above it — can therefore have a program of their own installed as
a root-owned CUPS filter.

### 2. Find your printer's device URI

```sh
lpinfo -v
```

Copy the URI from the second column of the line matching your printer — a USB printer looks like `usb://Samsung/ML-2165W%20Series?serial=...`, an mDNS/Bonjour-discovered one like `dnssd://Samsung%20ML-2165W%20Series._pdl-datastream._tcp.local/`. A network/Wi-Fi model that isn't listed (e.g. an ML-2165W that mDNS hasn't found) accepts raw print data on the JetDirect port, so use `socket://<printer-ip>:9100` — these printers do not speak IPP.

Every form is used the same way below, so only the value changes:

```sh
DEVICE_URI="usb://Samsung/ML-2165W%20Series?serial=Z1A2B3C4D5"   # USB
DEVICE_URI="socket://192.168.1.50:9100"                         # network / Wi-Fi (JetDirect)
```

Keep it quoted everywhere: `usb://` URIs contain `?` and `&`, which the shell would otherwise interpret.

### 3. Build, install the filter, validate the PPD

```sh
cargo build --release
sudo install -m 755 -o root -g root \
    target/release/rastertospl-rust /usr/lib/cups/filter/rastertospl-rust
cupstestppd ppd/samsung-ml2160.ppd
```

- **Use `install`, not `cp`.** The `-m 755 -o root -g root` flags matter: a filter that is writable by a non-root user is a filter someone else can replace.
- **Run `cupstestppd` after the binary is in place.** It checks that the file referenced by the PPD's `cupsFilter`/`cupsFilter2` line actually exists, so running it first reports a failure that isn't real.

### 4. Register the CUPS queue

```sh
sudo lpadmin -p ML2160_Rust -E -v "$DEVICE_URI" -P ppd/samsung-ml2160.ppd
```

`ML2160_Rust` is the queue name and is yours to choose; CUPS allows at most 127 characters and rejects spaces, `/` and `#`, so stick to letters, digits, `_`, `.` and `-`.

Then send a test print:

```sh
lp -d ML2160_Rust file.pdf
```

> [!NOTE]
> Pick the device URI yourself rather than letting anything auto-detect it. CUPS device discovery is unauthenticated — over the network (mDNS/Bonjour/SNMP) and over USB (descriptor strings) alike — so any device can advertise itself as a "Samsung ML-216x" and be wired up as the print destination, silently receiving your documents over unencrypted JetDirect. Reading `lpinfo -v` and choosing the line yourself is the review step that prevents this.

## Debian package (.deb)

The package installs the **2.0 printer application**, not the 1.x filter: one
binary at `/usr/bin/ml216x-printer-app`, a systemd unit at
`/usr/lib/systemd/system/ml216x-printer-app.service`, and the usual
documentation under `/usr/share/doc/`. It installs **no CUPS filter and no
PPD** — a printer application is driven over IPP, so nothing needs to live in
CUPS' filter directory. The 1.x filter and the PPD stay in the source tree and
are still built by `cargo build`; they are simply not what the `.deb` ships.

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
sudo apt install ./dist/samsung-ml2160-rust_2.0.0~alpha-2_amd64.deb
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
of their own, only raw JetDirect. The warning in step 2 of the filter
instructions above applies here too and is why nothing is auto-detected: a
device ID is self-reported, so an attacker's device can claim to be a Samsung
ML-216x. Read the line yourself and pass it yourself.

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

## Print options

Beyond page size and resolution, the PPD exposes two options that the filter
reads from the CUPS raster page header and forwards to the printer:

```sh
lp -d ML2160_Rust -o InputSlot=Manual -o MediaType=ENV envelope.pdf
lpoptions -d ML2160_Rust -l            # list every option and its choices
```

- **`InputSlot`** — `Auto` (the cassette) or `Manual` (the manual feed slot).
  The PPD numbers these with the QPDL paper-source codes themselves
  (`<</MediaPosition 1>>` and `2`), which the filter writes straight into byte
  `0x9` of the QPDL page header.
- **`MediaType`** — `OFF`, `NORMAL`, `THICK`, `THIN`, `BOND`, `OHP`, `CARD`,
  `LABEL`, `USED`, `COLOR`, `ENV`, `COTTON`, `RECYCLED`, `ARCHIVE`. These
  uppercase keywords look unfriendly because they are not labels: each one is
  the literal value sent as `@PJL SET PAPERTYPE=...`, taken from the
  `*MediaType` list in upstream SpliX's PPDs for this engine family
  (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`, `ml1640.ppd`, `ml2510.ppd`). The
  printer picks its fuser temperature and feed speed from this, so it is worth
  setting for envelopes, labels and card stock. `OFF` is the default and means
  "use the printer's own setting". Anything the filter does not recognise falls
  back to `OFF` and is reported on stderr, so a stale PPD shows up in
  `/var/log/cups/error_log` rather than silently printing envelopes on
  plain-paper settings.

> [!NOTE]
> The filter accepts only the page sizes and resolutions this PPD declares
> (paper dimensions may also have their width and height swapped for landscape).
> A page geometry that matches neither a `*PaperDimension` entry nor its
> landscape rotation, or a resolution pair other than 300x300, 600x600,
> 1200x600 and 1200x1200, fails the job with an `ERROR: … Desteklenmeyen …`
> line in `/var/log/cups/error_log` instead of printing (the filter's diagnostics
> are in Turkish). Earlier versions substituted A4 while logging a warning and
> coerced each unsupported resolution axis independently to 300, 600 or 1200
> DPI. That could send the printer a paper code or DPI that did not match the
> raster geometry it was being handed — misaligned output, or a feed from the
> wrong tray. Such a failure usually means that the queue is using a different
> PPD than `ppd/samsung-ml2160.ppd`, or that it received a malformed raster
> stream; first re-run the `lpadmin` command from step 4 to confirm the PPD.

> [!IMPORTANT]
> If you installed an earlier version of this PPD, re-run the `lpadmin` command
> from step 4 to load the current one. The older PPD offered paper types under
> readable names (`Plain`, `Envelope`, …) that never reached the printer, and a
> third paper source ("Tray 1") that does not exist on this hardware. Saved
> defaults referring to those names (`lpoptions -o MediaType=Plain`) are no
> longer valid choices and should be set again.

## Uninstallation

This section is about the **manually installed** 1.x filter. If you installed
the package instead, `apt remove samsung-ml2160-rust` is the whole story — see
[Remove it](#remove-it) — because the package ships no filter binary.

### 1. Remove the queue

```sh
lpstat -p                       # if you're unsure of the name
sudo lpadmin -x ML2160_Rust
```

### 2. Remove the filter binary, but only once nothing still uses it

The binary is shared by every queue built on this driver, so deleting it while
another one is still installed breaks that queue silently — its jobs start
failing with "filter failed". Ask which installed PPDs still name the filter:

```sh
sudo grep -rlsF rastertospl-rust /etc/cups/ppd/
```

`lpadmin -x` already deleted the removed queue's own PPD, so it won't appear
here. If the command prints nothing, no queue needs the filter any more:

```sh
sudo rm -f /usr/lib/cups/filter/rastertospl-rust
```

Note that the question is which PPD references the filter, not which queue looks
like a Samsung: a queue created against a plain `socket://<ip>:9100` address
carries no model name anywhere in its device URI.

## Testing

```sh
cargo test --workspace
```

The filter unit tests cover the CUPS Raster parser (v1/v2/v3, both endiannesses, the
v2 line-RLE decoder), page-header validation, the SPL2/QPDL record layout, the
horizontal band placement, the Algo 0x11 RLE round trip, PJL field sanitisation,
and every resource limit the filter enforces.

Several of them are pinned against measurements from real `cupsfilter` output
rather than from the specification — `test_validate_page_header_accepts_real_cupsfilter_heights`
carries the observed `cupsHeight` for each paper size and resolution (A4 at
600 DPI is 6817 lines, not the 7017 the page dimensions alone suggest, because
the PPD's `*ImageableArea` margins come off first). Keep that table measured,
not computed, if you extend it.

There is no end-to-end script. To check the filter against the real CUPS
toolchain by hand:

```sh
cargo build --release
cupsfilter -p ppd/samsung-ml2160.ppd -m application/vnd.cups-raster -- doc.pdf > test.raster
./target/release/rastertospl-rust 101 testuser Test 1 "" test.raster > out.spl
```

A well-formed `out.spl` starts with `\x1b%-12345X@PJL`, contains
`@PJL ENTER LANGUAGE = QPDL`, and ends with `\t\x1b%-12345X`.

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

GPLv2 (v2 only) — see [LICENSE](LICENSE). The protocol implementation is derived from the GPLv2-licensed OpenPrinting SpliX project, so it's licensed to match.
