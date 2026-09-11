#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# The check list, in one runnable place.
#
# Until now the list of things that must pass lived in prose — in
# CONTRIBUTING.md, in docs/SESSION-STATE.md — and every run was somebody
# typing eight commands from memory. That drifts: three clippy lints that did
# not exist when the engine was written turned the documented `-D warnings`
# run red on rustc 1.95, and the only reason it was noticed is that someone
# happened to type the command. This script is what CI runs and what a
# contributor runs, so the enforced list and the documented list are the same
# list.
#
# Everything here is software. **A green run says nothing about where toner
# lands on paper**: release gate G-1 is a physical measurement on real
# hardware (docs/GOLDEN-VALIDATION.md section 4, runbook in
# docs/G1-MEASUREMENT.md), and neither this script nor CI can take it. See
# docs/CI.md for what is and is not covered.
#
# Usage:
#   scripts/run-checks.sh              every group below, in order
#   scripts/run-checks.sh test probes  only the named groups
#   scripts/run-checks.sh --list       the group names and what each covers
#
# Needs: cargo, pkg-config, libpappl-dev in the supported range, cc,
# libcups2-dev, ipptool (cups-ipp-utils), dpkg-deb, python3.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

ALL_GROUPS="fmt clippy test features goldens probes security deb"

# `--offline` is deliberate and load bearing: this workspace has no
# third-party crates at all (Cargo.lock holds exactly the five local ones),
# spl2-core is required to stay dependency free, and the FFI crates are hand
# written rather than generated. Offline keeps CI hermetic and turns "somebody
# added a dependency" into a visible, deliberate change to this file.
CARGO_OFFLINE="--offline --locked"

usage() {
    cat <<'EOF'
scripts/run-checks.sh [group...]

Groups, in the order a full run uses:

  fmt       cargo fmt --all --check
  clippy    cargo clippy --workspace --all-targets -- -D warnings
  test      cargo test --workspace (163 tests, includes the golden corpus)
  features  spl2-core built and tested both with and without `golden-replay`
            (decision Q-6), and the qpdl-decode example the G-1 harness uses
  goldens   sha256sum -c over goldens/SHA256SUMS, so a blessed corpus cannot
            drift from its recorded checksums
  probes    the hardware-free harnesses: p5-probe (3 modes, and its output
            diffed against the committed docs/P5-MEASUREMENTS.json),
            transport-probe (plain plus both injections), server-probe (every
            web page served with the server still alive, and a printer
            restored from state without being advertised) and g1-probe --all
            (44 cases, plus all three injections)
  security  security-probe.py — reproduces the two libpappl 1.3.1 overflows.
            A FAILURE here most likely means libpappl was fixed, which is the
            signal to retire the matching row in docs/SECURITY-REVIEW.md, not
            a defect in this tree.
  deb       sh -n over the maintainer scripts, then scripts/build-deb.sh

No group needs a printer. Release gate G-1 is physical and is not covered.

  --self-test  prove this runner still stops at the first failure. Runs a
               hidden group whose middle command fails and whose last command
               would succeed, and requires the run to abort with a non-zero
               status. That is the defect this script shipped with for one
               afternoon; see docs/CI.md.
EOF
}

# The hidden group --self-test runs. `false` is deliberately not the last
# command: a runner that only reports the status of a group's last command
# passes this group, which is exactly the bug being tested for.
SPECIAL=""
case "${1-}" in
--list | -l | --help | -h)
    usage
    exit 0
    ;;
--self-test)
    SPECIAL=self-test
    ;;
esac

GROUPS=${*:-$ALL_GROUPS}
[ -n "$SPECIAL" ] && GROUPS=""

# `__selftest` is runnable when named explicitly but is not in ALL_GROUPS, so
# a full run never selects it. It has to go through the runner loop below like
# any other group: a self-test that called its group function directly would
# pass even with the `|| fail` runner this script started with, which is a
# mistake this file made once already.
for g in $GROUPS; do
    case " $ALL_GROUPS __selftest " in
    *" $g "*) ;;
    *)
        echo "run-checks: unknown group '$g'; try --list" >&2
        exit 2
        ;;
    esac
done

say() {
    printf '\n=== %s\n' "$*"
}

# Which group is running, and the scratch directory the probe group needs, both
# read by the exit trap below.
CURRENT=""
WORK=""

# The first version of this script called each group as `run_$g || fail $g`.
# That reads like a fail-fast runner and is the opposite of one: putting a
# function on the left of `||` disables `set -e` for its whole body, so a
# command that failed in the middle of a group was ignored and the group's
# status became that of its last command. It was caught by mutating
# docs/P5-MEASUREMENTS.json and watching the run pass anyway — which is the
# argument for mutating a new harness before trusting it, made against this
# harness. Groups are now called plainly, `set -e` ends the run, and the trap
# only reports which group was running. Do not reintroduce `|| fail`;
# `--self-test` exists to catch it if anyone does.
on_exit() {
    status=$?
    if [ -n "$WORK" ]; then
        rm -rf "$WORK"
    fi
    if [ "$status" -ne 0 ] && [ -n "$CURRENT" ]; then
        printf '\nrun-checks: FAILED in group %s (exit %s)\n' "$CURRENT" "$status" >&2
    fi
}
trap on_exit EXIT

# The probe scripts want the debug binary; build it once for every group that
# does, rather than once per script.
app_built=no
build_app() {
    [ "$app_built" = yes ] && return 0
    cargo build $CARGO_OFFLINE -p ml216x-printer-app
    app_built=yes
}

run_fmt() {
    say "fmt"
    cargo fmt --all --check
}

run_clippy() {
    # `--features` for the same reason `run_test` gives: without it, the replay
    # loop and the two test files behind `golden-replay` are not compiled and
    # so are not linted. `--all-targets` alone does not reach a cfg'd-out file.
    say "clippy (-D warnings)"
    cargo clippy $CARGO_OFFLINE --workspace --all-targets \
        --features spl2-core/golden-replay -- -D warnings
}

run_test() {
    # `golden-replay` is named explicitly. It used to arrive for free: the root
    # package was the 1.x filter and it depended on spl2-core with the feature
    # on, so a workspace build unified it in. P11 deleted that package, and
    # without this flag the golden corpus and the filter's 61 tests are cfg'd
    # out of the run and pass by not existing. Q-6 keeps the feature
    # non-default, so the shipping application still compiles without it —
    # `run_features` is what proves that.
    say "test: the whole workspace"
    cargo test $CARGO_OFFLINE --workspace --features spl2-core/golden-replay
}

run_features() {
    # Q-6: `raster.rs` lives behind a non-default feature, not `#[cfg(test)]`,
    # so both configurations have to be built. The example is what
    # scripts/g1-probe.py decodes with, and it only exists under the feature.
    say "features: spl2-core without golden-replay"
    cargo build $CARGO_OFFLINE -p spl2-core
    say "features: spl2-core with golden-replay"
    cargo test $CARGO_OFFLINE -p spl2-core --features golden-replay
    cargo build $CARGO_OFFLINE -p spl2-core --features golden-replay --example qpdl-decode
}

run_goldens() {
    say "goldens: recorded checksums"
    # LC_ALL=C so the OK/FAILED lines read the same everywhere; the exit
    # status is what decides, but a translated log is harder to review.
    (cd goldens && LC_ALL=C sha256sum -c SHA256SUMS)
}

run_probes() {
    build_app
    # Cleaned up by the exit trap, so a failure part way through does not leave
    # the directory behind and does not need a trap of its own — a nested trap
    # would replace the one that reports the failing group.
    WORK=$(mktemp -d)
    work=$WORK

    say "probes: p5-probe, default mode"
    python3 scripts/p5-probe.py --output "$work/p5.json"
    # The script measures; it does not compare. docs/P5-MEASUREMENTS.json is
    # the committed record of what PAPPL delivered, and P5's whole claim is
    # that a rerun reproduces it byte for byte, so CI is where that gets
    # checked rather than asserted.
    diff -u docs/P5-MEASUREMENTS.json "$work/p5.json"

    say "probes: p5-probe --spl (QPDL page headers)"
    python3 scripts/p5-probe.py --spl --output "$work/p5-spl.json"

    say "probes: p5-probe --device-failure (job must abort)"
    python3 scripts/p5-probe.py --device-failure --output "$work/p5-fail.json"

    say "probes: transport-probe (file == socket)"
    python3 scripts/transport-probe.py
    # Both injections must be *detected*; each script exits 0 when it caught
    # the defect it planted, so a harness that stopped working fails here.
    say "probes: transport-probe --inject truncate"
    python3 scripts/transport-probe.py --inject truncate
    say "probes: transport-probe --inject flip"
    python3 scripts/transport-probe.py --inject flip

    say "probes: server-probe (every web page, and state without DNS-SD)"
    # No injection flag: the two previous releases are the injection, and
    # `--application` is how the script was shown to go red against them. See
    # its docstring.
    python3 scripts/server-probe.py

    say "probes: g1-probe --all (every medium and resolution)"
    python3 scripts/g1-probe.py --all
    for inject in shift crop scale; do
        say "probes: g1-probe --inject $inject"
        python3 scripts/g1-probe.py --inject "$inject"
    done

}

run_security() {
    build_app
    say "security: reproduce the two libpappl 1.3.1 overflows"
    python3 scripts/security-probe.py
}

run_deb() {
    say "deb: maintainer scripts parse"
    for s in packaging/debian/postinst packaging/debian/prerm packaging/debian/postrm scripts/build-deb.sh scripts/run-checks.sh; do
        sh -n "$s"
    done
    say "deb: build the package"
    scripts/build-deb.sh
}

run___selftest() {
    say "selftest: a failing command in the middle of a group"
    false
    say "selftest: THIS-LINE-MUST-NOT-BE-REACHED"
}

self_test() {
    out=$("$0" __selftest 2>&1) && status=0 || status=$?
    if [ "$status" -eq 0 ]; then
        echo "run-checks --self-test: FAILED — a group with a failing command exited 0" >&2
        printf '%s\n' "$out" >&2
        exit 1
    fi
    case $out in
    *THIS-LINE-MUST-NOT-BE-REACHED*)
        echo "run-checks --self-test: FAILED — the run continued past the failing command" >&2
        printf '%s\n' "$out" >&2
        exit 1
        ;;
    esac
    case $out in
    *"FAILED in group __selftest"*) ;;
    *)
        echo "run-checks --self-test: FAILED — the failing group was not named" >&2
        printf '%s\n' "$out" >&2
        exit 1
        ;;
    esac
    echo "run-checks --self-test: PASS (exit $status, stopped at the failing command, group named)"
}

case "$SPECIAL" in
self-test)
    self_test
    exit 0
    ;;
esac

for g in $GROUPS; do
    CURRENT=$g
    "run_$g"
done
CURRENT=""

printf '\nrun-checks: PASS (%s)\n' "$GROUPS"
printf 'This is the software half only. Release gate G-1 is a measurement on\n'
printf 'paper and is still open; see docs/G1-MEASUREMENT.md.\n'
