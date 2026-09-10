# P11 — what deleting the 1.x filter means, and what has to be true first

Decision Q-5 keeps the 1.x CUPS filter in the tree and says the removal needs
its own approval and its own list. This is that list, written **before** the
gate opens rather than during it, and reviewed on 2026-09-10 with the
preparation it called for now done.

## The gate

**P11 may not be taken until release gate G-1 has been measured on paper**
(`docs/G1-MEASUREMENT.md`). The 1.x filter is the reference the 2.0 output is
judged against; deleting the reference before the comparison is made would
leave nothing to compare with. Q-7 records a second reason: the 1.x static
musl binary is the only build that runs on a distribution the `.deb` does not
target, so it is also the fallback until the package's reach is not a question.

## Preparation, done 2026-09-10

The removal used to be entangled with the evidence. `src/golden.rs` — the
harness that freezes 32 SPL2 streams byte for byte — lived in the same package
as the filter and drove it through `crate::process_with_margin`, so a literal
`git rm` of the 1.x code would have deleted the corpus's only reader along with
it.

That is no longer true:

* The page loop moved to `crates/spl2-core/src/replay.rs`, behind the
  `golden-replay` feature (Q-6), unchanged. `src/main.rs` calls it through two
  wrappers and supplies the stderr sink.
* The harness moved to `crates/spl2-core/tests/golden.rs` and drives
  `spl2_core::replay` directly. All 32 goldens matched byte for byte across the
  move, `goldens/SHA256SUMS` did not change, and the filter's own 61 tests still
  pass.

So P11 is now a deletion of front-end code, not of evidence.

## The list

Delete:

| Path | What it is |
|---|---|
| `src/main.rs` | argv parsing, stdin/stdout wiring, the stderr `Log` impl, and the filter's own test module |
| `Cargo.toml` root `[package]`, `[[bin]]` and `[dependencies]` | the `rastertospl-rust` package itself; the file becomes a virtual workspace manifest |
| `docs/LEGACY-FILTER.md` | how to build and run the reference |

Keep, and do not confuse with the above:

| Path | Why it stays |
|---|---|
| `crates/spl2-core/src/replay.rs` | the loop the corpus is replayed through |
| `crates/spl2-core/tests/golden.rs`, `goldens/` | the byte-for-byte evidence; permanent |
| `ppd/samsung-ml2160.ppd` | permanent project data (Q-5), and the source of the hard-margin table |
| the `v1.x-final` tag | the recovery anchor; the 1.x line is reachable from it forever |

## What the deletion itself has to do

1. Move the filter tests worth keeping. `src/main.rs`'s test module holds
   checks that are not about argv: the PPD-versus-limits tests, and the
   `validate_page_header` cases. The PPD cross-check already has a second home
   in `crates/ml216x-printer-app/src/media_table.rs`; the header-validation
   cases belong next to `validate_page_geometry` in `spl2-core`. Decide
   per test, do not delete in bulk.
2. Turn the root manifest into a virtual workspace, and drop `default-members`,
   which exists only to stop `cargo build` selecting the filter.
3. Update `scripts/run-checks.sh`, `.github/workflows/checks.yml`,
   `CONTRIBUTING.md`, `README.md` and `packaging/debian/copyright` — the last
   one names `src/main.rs` in the SpliX derivation stanza, and that stanza must
   keep naming every file that carries transcribed material.
4. Re-run the full check list. The goldens must be untouched by the deletion:
   if a `.spl` file moves, something was deleted that was not front-end code.

## What it does not do

It does not remove the SpliX derivation, the GPL-2.0-only licence, or the
`ppd/` directory, and it does not touch the goldens. The 1.x *line* is not
being abandoned in git: `v1.x-final` (tag object `7c0cf2c`, commit `33d4ff2`)
is annotated, pushed, and the only ref that names it.
