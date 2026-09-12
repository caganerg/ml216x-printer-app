#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# Build and install upstream PAPPL 1.4.12 into a prefix.
#
# This tree supports two libpappl releases (decision Q-27): the 1.3.1 Debian
# ships, which needs nothing but `apt install libpappl-dev`, and upstream
# 1.4.12, which **no Debian suite packages** — stable, testing and unstable all
# carry 1.3.1-2.1, and experimental carries nothing. So the only way to have
# 1.4.12 is to build it, and the only reason to want it is what 1.4.12 fixed:
# the two memory-corruption defects recorded as S-1 and S-2 in
# docs/SECURITY-REVIEW.md, which `scripts/security-probe.py` still reproduces
# to a crash against 1.3.1 and cannot reproduce against this.
#
# This script is what CI's `pappl-1.4` job runs, so the build a contributor
# gets and the build the checks are proven against are the same build.
#
# Usage:
#   scripts/install-pappl-1.4.sh [prefix]     default prefix: /usr/local
#
# Installing into /usr/local needs write access there, so that form is run
# under sudo. Nothing about the printer application itself changes: it still
# runs as your own user with no privilege (decision Q-18). A prefix in your
# home directory needs no privilege at all and is what CI uses.
#
# Needs: curl, cc, make, and the libraries upstream's BUILD.md lists —
# on Debian: build-essential libavahi-client-dev libcups2-dev libgnutls28-dev
# libjpeg-dev libpam0g-dev libpng-dev libusb-1.0-0-dev zlib1g-dev
set -eu

VERSION=1.4.12
# sha256 of pappl-1.4.12.tar.gz as published on the upstream release page,
# recorded on 2026-09-12. The download is checked against it: a build that
# silently took different sources would make every measurement in
# docs/ meaningless, and this is the one place to notice.
SHA256=1684c4e06446e9f7d93a39729fa0ba56f07a4007560080fdad7d0e2076a3615f
URL=https://github.com/michaelrsweet/pappl/releases/download/v$VERSION/pappl-$VERSION.tar.gz

PREFIX=${1:-/usr/local}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

echo "=== downloading PAPPL $VERSION"
curl --proto '=https' --tlsv1.2 -fsSL -o "$WORK/pappl.tar.gz" "$URL"

echo "=== checking the tarball against its recorded sha256"
echo "$SHA256  $WORK/pappl.tar.gz" | sha256sum -c -

echo "=== building"
tar xzf "$WORK/pappl.tar.gz" -C "$WORK"
cd "$WORK/pappl-$VERSION"
./configure --prefix="$PREFIX" --enable-shared
make -j"$(nproc 2>/dev/null || echo 2)"

echo "=== installing into $PREFIX"
make install

cat <<EOF

PAPPL $VERSION is installed in $PREFIX.

Build and check this tree against it by pointing pkg-config and the dynamic
linker at that prefix — both, because they answer different questions and a
build that mixes the two would compile against one release and run against
the other (every 1.x has soname libpappl.so.1, so nothing would complain):

  export PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig
  export LD_LIBRARY_PATH=$PREFIX/lib
  pkg-config --modversion pappl     # must print $VERSION
  ./scripts/run-checks.sh

For a system-wide prefix, \`ldconfig\` instead of LD_LIBRARY_PATH is the usual
answer, but note that it makes $PREFIX/lib/libpappl.so.1 the library every
PAPPL application on the machine loads, not only this one.
EOF
