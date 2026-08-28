# LibPerlRs::Inspect

*(formerly `OpTree::Analyzer` — renamed 2026-08 into the `LibPerlRs::*`
family namespace, alongside
[`LibPerlRs::PartialEval`](https://github.com/hkoba/perl-LibPerlRs-PartialEval);
see `docs/rename-libperlrs-inspect-2026-08.md`)*

Analyze a Perl subroutine reference (including anonymous subs returned
by `eval`) at the OP-tree level, powered by Rust. Reports:

- argument specification (signatures, `my (...) = @_`, `shift`/`pop`,
  `$_[n]` — min/max arity, parameter names, defaults)
- return-value / exception specification (`return` sites, implicit last
  expression, `die` / `Carp::croak`, `wantarray` use)
- truth tables for statement-level guard conditions
- best-effort variable type facets with conflict detection
  (e.g. a hashref later used as an arrayref, with line numbers)
- lints (e.g. `my $x = EXPR if COND`)

```perl
use LibPerlRs::Inspect;

my $sub    = eval 'sub { my ($x) = @_; return unless $x; $x + 1 }';
my $report = LibPerlRs::Inspect::analyze($sub);   # native hashref
say $report->{args}{min_arity};

my $names = LibPerlRs::Inspect::op_names($sub);   # execution-order op names
```

The implementation is a Rust cargo workspace (`crates/`) built on
[libperl-rs](https://github.com/hkoba/libperl-rs); the XS glue is a
`cdylib` loaded via XSLoader like any other XS module.

## perl-inspect CLI

`crates/inspect-cli` provides `perl-inspect`, a standalone binary with
its own embedded perl: it *compiles* the target (BEGIN/`use` run, the
main body does not — the same trust model as `perl -c`), walks the
symbol table, and prints a JSON report (`schema_version: 1`) of every
package/sub with a provenance tag (`file` / `imported` / `xs`), source
line ranges, and the argument-spec analysis for subs defined in the
target file (`--deep` embeds the full report: args / returns / logic /
types / lints).

```console
$ cargo build -p inspect-cli --release
$ ./target/release/perl-inspect lib/Foo.pm | jq '.packages."Foo".subs'
$ ./target/release/perl-inspect -e 'sub add { my ($x, $y) = @_; $x + $y }'
```

(Not yet staged by `make install`; for now it is a cargo-built
artifact.)

## Build requirements

- Perl >= 5.42, built with development headers
  (`dnf install perl-devel` / `apt install libperl-dev`).
  Verified on a threaded (ithreads) perl; the build adapts to the
  configuration of the perl that runs `Makefile.PL`.
- Rust toolchain with cargo >= 1.85 (edition 2024) — https://rustup.rs/
- libclang, required by bindgen (`dnf install clang-devel` /
  `apt install libclang-dev`)
- **Network access to crates.io at build time** (Rust dependencies are
  fetched by cargo; see "Offline builds" below)

Linux is the supported platform for now. macOS is untested: perl expects
loadable modules in `.bundle` format there, which rustc does not emit
directly (a re-link step would be needed — see `docs/` notes).

## Installation

```console
$ perl Makefile.PL
$ make
$ make test
$ make install
```

The Rust parts are built with `cargo --locked` against the bundled
`Cargo.lock` for reproducibility, linking against the perl that ran
`Makefile.PL` (the `PERL` environment variable is passed down to the
cargo build scripts).

`make install` only copies the staged files out of `blib/` — it never
invokes cargo, so `sudo make install` runs no build as root. Build and
test as a normal user first (`make && make test`); `make install`
refuses to run if nothing is staged or if `blib/` holds a debug build.

### Offline builds

Vendor the Rust dependencies once while online, then point cargo at the
vendor directory:

```console
$ cargo vendor vendor/
$ mkdir -p .cargo && cat > .cargo/config.toml <<'EOT'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
EOT
$ perl Makefile.PL && make CARGO_BUILD_FLAGS='--locked --offline'
```

## Development

```console
$ perl Makefile.PL
$ make debug                    # dev profile: much faster cargo builds
$ prove -b t/
$ ./test.zsh                    # the same, as a one-shot wrapper
$ UPDATE_GOLDEN=1 prove -b t/   # regenerate t/golden/*.json snapshots
```

Plain `make` (and `make test`) always builds the release profile;
`make debug` stages a dev-profile build into `blib/` for fast
iteration. Run plain `make` again before `make install` — the install
guard rejects a debug-staged `blib/`.

`make clean` (and `make realclean`) also runs `cargo clean`, removing
`target/` — the next build is a full rebuild including bindgen. This
also covers switching to a different perl (stale bindgen output is not
yet re-keyed by interpreter path).

## License

This library is free software; you can redistribute it and/or modify it
under the same terms as Perl itself.
