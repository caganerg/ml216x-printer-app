#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# Build the .deb for the 2.0 printer application.
#
# The 1.x package was a statically linked musl filter plus a PPD, and the
# recipe for it lived in the README. Neither carries over. 2.0 links libpappl
# and libcups dynamically against the versions Debian ships (decision Q-1/Q-7),
# so the binary is a normal glibc one and its dependencies are real package
# dependencies; and it installs a daemon with a service unit rather than a
# filter, which needs maintainer scripts. That is more than a README snippet
# should carry, so it lives here where it can be run and read.
#
# Needs: cargo, pkg-config, dpkg-deb (from `dpkg`, present on every Debian
# system — `dpkg-dev` is still not required), and libpappl-dev in the
# supported range. Everything below is checked before anything is built.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

die() {
    echo "build-deb: $*" >&2
    exit 1
}

for tool in cargo pkg-config dpkg-deb; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done

# The build-time half of decision Q-1, enforced rather than documented.
# `crates/pappl-sys/build.rs` refuses the same range at compile time; failing
# here first gives a message that names the packaging requirement.
pkg-config --exists pappl || die "libpappl-dev is not installed"
PAPPL_VERSION=$(pkg-config --modversion pappl)
pkg-config --atleast-version=1.3 pappl || die "libpappl $PAPPL_VERSION is older than 1.3"
pkg-config --max-version=1.999 pappl || die "libpappl $PAPPL_VERSION is 2.x; decision Q-1 targets 1.3"

VERSION=$(sed -n 's/^Version: //p' packaging/debian/control)
ARCH=$(sed -n 's/^Architecture: //p' packaging/debian/control)
[ -n "$VERSION" ] || die "no Version: field in packaging/debian/control"
[ "$ARCH" = "$(dpkg --print-architecture)" ] ||
    die "packaging/debian/control says $ARCH, this machine is $(dpkg --print-architecture)"

BINARY=target/release/ml216x-printer-app
cargo build --release -p ml216x-printer-app

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
# mktemp gives 0700; the package's own root directory must not carry that.
chmod 755 "$STAGE"
DOC=usr/share/doc/samsung-ml2160-rust

install -D -m 755 "$BINARY" "$STAGE/usr/bin/ml216x-printer-app"
# /usr/lib/systemd/user, not .../system: the application runs as a user
# service in the user's own session, so there is no root process anywhere in
# the path (decision Q-18). /usr/lib rather than /lib because trixie is
# usr-merged and DEP-17 wants the real path.
install -D -m 644 packaging/systemd/user/ml216x-printer-app.service \
    "$STAGE/usr/lib/systemd/user/ml216x-printer-app.service"
install -D -m 644 packaging/udev/71-ml216x-printer-app.rules \
    "$STAGE/usr/lib/udev/rules.d/71-ml216x-printer-app.rules"
install -D -m 644 packaging/debian/copyright "$STAGE/$DOC/copyright"
gzip -9nc packaging/debian/changelog >"$STAGE/$DOC/changelog.Debian.gz"
gzip -9nc README.md >"$STAGE/$DOC/README.md.gz"
chmod 644 "$STAGE/$DOC"/*.gz

# Installed-Size is in KiB and is what apt reports before installing.
SIZE=$(du -sk --apparent-size "$STAGE" | cut -f1)
mkdir -p "$STAGE/DEBIAN"
awk -v s="$SIZE" '/^Description:/ && !d { print "Installed-Size: " s; d = 1 } { print }' \
    packaging/debian/control >"$STAGE/DEBIAN/control"
for script in postinst prerm postrm; do
    install -m 755 "packaging/debian/$script" "$STAGE/DEBIAN/$script"
done
(cd "$STAGE" && find . -path ./DEBIAN -prune -o -type f -printf '%P\n' |
    LC_ALL=C sort | xargs md5sum) >"$STAGE/DEBIAN/md5sums"
chmod 644 "$STAGE/DEBIAN/control" "$STAGE/DEBIAN/md5sums"

# --root-owner-group is what makes the installed files root-owned whoever
# built the package; the 1.x recipe spelled the same thing out with tar flags.
mkdir -p dist
OUTPUT="dist/samsung-ml2160-rust_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$STAGE" "$OUTPUT" >/dev/null
echo "built $OUTPUT"
dpkg-deb --info "$OUTPUT" | sed -n '/^ Package:/,/^ Description:/p'
