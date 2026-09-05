# samsung-ml2160-rust
> **2.0 development:** the PAPPL mainloop and P5 geometry probe are implemented.
> Actual SPL2 printing through PAPPL is not connected yet. The transitional
> filter and PPD now use the maintainer-selected **12.5 pt** margins; physical
> validation is still required. The original 1.x release remains on
> `legacy/cups-filter-1.x` / `v1.x-final`.

## PAPPL development (P5)

Build dependencies: Rust 1.77+, `pkg-config`, a C compiler, `libpappl-dev`
(1.3.x; tested with 1.3.1), and `libcups2-dev`. Run the integration experiment
with `ipptool` from `cups-ipp-utils` installed:

```sh
cargo build -p ml216x-printer-app
python3 scripts/p5-probe.py --output /tmp/p5-measurements.json
python3 scripts/p5-probe.py --device-failure --output /tmp/p5-failure.json
```

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

The probe writes JSON Lines, permits only file destinations, and binds TCP to
127.0.0.1. Stop it with Ctrl-C or the `shutdown` subcommand and the same `-u`
server URI. Real print jobs are refused until the encoder adapter is connected.
PAPPL's auto-start command does not preserve these custom flags, so start the
probe with the explicit `server` command above.

The 17-case experiment measured **full-media raster with zero header margins**.
See [margin decision and measured results](docs/MARGINS.md) and
[current migration state](docs/SESSION-STATE.md). P9 must still review raster
conversion/dithering and input-versus-job geometry before real printing.

## Transitional CUPS filter

The instructions below describe the classic filter. The Debian packaging
recipe is historical and has not yet been migrated to build the PAPPL package.
The updated PPD must be reloaded into an existing queue when testing 12.5 pt.

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

Debian 13 (trixie) and later can install the driver as a package, which
replaces step 3 above and hands step 4 a system PPD path. The filter is
compiled against **musl** and linked statically, so the binary in the package
carries no libc dependency and runs unchanged on Debian 13, on later releases
and on derivatives; the only declared dependencies are `cups` and
`cups-filters`.

The package installs two files — `/usr/lib/cups/filter/rastertospl-rust` and
`/usr/share/ppd/samsung-ml2160-rust/samsung-ml2160.ppd` — plus the usual
documentation under `/usr/share/doc/`. It does **not** create a print queue.

### Build it

`dpkg-dev` is not needed: a `.deb` is an `ar` archive of three members, and
`ar`, `tar`, `xz` and `gzip` are enough to produce one. The control metadata
lives in `packaging/debian/` and its `Version:` field is what the file name
below has to match. The ownership warning in step 1 applies here too — the
package embeds the filter binary that CUPS will later run as user `lp`.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl

REPO=$PWD
VERSION=2.0.0~alpha-1
DOC=usr/share/doc/samsung-ml2160-rust
BUILD=$(mktemp -d)
mkdir -p "$BUILD"/root/usr/lib/cups/filter \
         "$BUILD"/root/usr/share/ppd/samsung-ml2160-rust \
         "$BUILD"/root/"$DOC" "$BUILD"/control

install -m 755 target/x86_64-unknown-linux-musl/release/rastertospl-rust \
    "$BUILD/root/usr/lib/cups/filter/rastertospl-rust"
install -m 644 ppd/samsung-ml2160.ppd \
    "$BUILD/root/usr/share/ppd/samsung-ml2160-rust/samsung-ml2160.ppd"
install -m 644 packaging/debian/copyright "$BUILD/root/$DOC/copyright"
gzip -9nc packaging/debian/changelog > "$BUILD/root/$DOC/changelog.Debian.gz"
gzip -9nc README.md > "$BUILD/root/$DOC/README.md.gz"
chmod 644 "$BUILD"/root/"$DOC"/*.gz

awk -v s="$(du -sk --apparent-size "$BUILD/root" | cut -f1)" \
    '/^Description:/ && !d { print "Installed-Size: " s; d = 1 } { print }' \
    packaging/debian/control > "$BUILD/control/control"
( cd "$BUILD/root" && find . -type f -printf '%P\n' | LC_ALL=C sort | xargs md5sum ) \
    > "$BUILD/control/md5sums"
chmod 644 "$BUILD/control/control" "$BUILD/control/md5sums"

printf '2.0\n' > "$BUILD/debian-binary"
TARFLAGS="--owner=root --group=root --numeric-owner --sort=name --mtime=@0"
tar $TARFLAGS -C "$BUILD/root"    -cf - . | xz -9e > "$BUILD/data.tar.xz"
tar $TARFLAGS -C "$BUILD/control" -cf - . | xz -9e > "$BUILD/control.tar.xz"

mkdir -p "$REPO/dist"
( cd "$BUILD" && ar rcD "$REPO/dist/samsung-ml2160-rust_${VERSION}_amd64.deb" \
    debian-binary control.tar.xz data.tar.xz )
```

The `--owner=root --group=root --numeric-owner` flags are not cosmetic: they are
what makes the installed filter root-owned regardless of who built the package.

### Install it and register the queue

```sh
sudo apt install ./dist/samsung-ml2160-rust_2.0.0~alpha-1_amd64.deb
sudo lpadmin -p ML2160_Rust -E -v "$DEVICE_URI" \
    -P /usr/share/ppd/samsung-ml2160-rust/samsung-ml2160.ppd
```

`$DEVICE_URI` comes from step 2 above. Use `apt install ./…` rather than
`dpkg -i` so that `cups` and `cups-filters` are pulled in if they are missing.

Because the PPD now lives under `/usr/share/ppd/`, CUPS lists it as a model in
its own driver database, so you can name it by model instead of by path — and
the graphical tools (CUPS' web interface at <http://localhost:631> →
*Administration* → *Add Printer*, or GNOME/KDE printer settings) offer it under
*Samsung ML-2160/2165/2165W/2168 Series, Rust SPL2 Driver* once the package is
installed:

```sh
lpinfo -m | grep -i ml-216       # samsung-ml2160-rust/samsung-ml2160.ppd …
sudo lpadmin -p ML2160_Rust -E -v "$DEVICE_URI" \
    -m samsung-ml2160-rust/samsung-ml2160.ppd
```

The device URI still has to be chosen — it is a property of your machine (a USB
serial number, or the printer's address), not of the package, so nothing that
ships in the `.deb` can know it. What the graphical dialogs save you is the
typing, not the choice: they show the detected USB device as a line you click,
which is the same deliberate pick the note in step 2 asks for. Network models
are not detected at all — these printers speak no IPP, only raw JetDirect — so
there `socket://<printer-ip>:9100` remains something you enter by hand.

To remove it, delete the queues first (`sudo lpadmin -x ML2160_Rust`, see
[Uninstallation](#uninstallation)) and then `sudo apt remove samsung-ml2160-rust`
— `apt remove` takes the filter away from every queue built on it, including any
it did not create.

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

- `src/main.rs` — CUPS filter entry point: argument parsing, page/band loop
- `src/raster.rs` — CUPS Raster (V1/V2/V3) header parser
- `src/spl.rs` — SPL2/QPDL protocol: PJL envelope, page/band records, Algo 0x11 RLE
- `ppd/samsung-ml2160.ppd` — CUPS PPD file
- `packaging/debian/` — `control`, `copyright` and `changelog` for the `.deb`

## License

GPLv2 (v2 only) — see [LICENSE](LICENSE). The protocol implementation is derived from the GPLv2-licensed OpenPrinting SpliX project, so it's licensed to match.
