# Legacy CUPS filter reference

The root package `rastertospl-rust` retains the 1.x filter for golden
regression tests and hardware comparison until migration gate P11 passes.
The PPD remains project data: tests compare its capabilities with the IPP table.
Neither is installed by the 2.0 Debian package. The untouched 1.x version is
available at the annotated tag `v1.x-final` (tag object `7c0cf2c`, commit
`33d4ff2`). The `legacy/cups-filter-1.x` branch that used to carry the same
commits was deleted on 2026-09-06, when the project narrowed to supporting 2.0
only; it held nothing the tag and `main` do not, so `git checkout v1.x-final`
reaches exactly what the branch reached.

Build and run the reference explicitly:

```sh
cargo build --release -p rastertospl-rust
cupsfilter -p ppd/samsung-ml2160.ppd -m application/vnd.cups-raster -- doc.pdf > test.raster
./target/release/rastertospl-rust 101 testuser Test 1 "" test.raster > out.spl
cargo test -p rastertospl-rust golden
```

The current reference uses 12.5 pt margins; historical 12 pt measurements do
not validate those margins. See [the hardware release gate](GOLDEN-VALIDATION.md#release-gate-g-1--measure-the-margins-on-real-hardware).

## Removing an old manual installation

After migrating to the Printer Application, remove obsolete queues explicitly
(use your actual queue name):

```sh
lpstat -p
sudo lpadmin -x ML2160_Rust
sudo grep -rlsF rastertospl-rust /etc/cups/ppd/
```

Only if no remaining queue references the filter, remove its binary:

```sh
sudo rm -f /usr/lib/cups/filter/rastertospl-rust
```

Removing the 2.0 package does not remove a manually installed 1.x filter.
