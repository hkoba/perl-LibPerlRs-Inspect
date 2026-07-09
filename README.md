# OpTree::Analyzer

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
use OpTree::Analyzer;

my $sub    = eval 'sub { my ($x) = @_; return unless $x; $x + 1 }';
my $report = OpTree::Analyzer::analyze($sub);   # native hashref
say $report->{args}{min_arity};

my $names = OpTree::Analyzer::op_names($sub);   # execution-order op names
```

The implementation is a Rust cargo workspace (`crates/`) built on
[libperl-rs](https://github.com/hkoba/libperl-rs); the XS glue is a
`cdylib` loaded via XSLoader like any other XS module.

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
$ perl Makefile.PL --debug      # dev profile: much faster cargo builds
$ make test
$ ./test.zsh                    # the same, as a one-shot wrapper
$ UPDATE_GOLDEN=1 make test     # regenerate t/golden/*.json snapshots
```

`make realclean` also runs `cargo clean`. If you switch to a different
perl, run `cargo clean` manually once (stale bindgen output is not yet
re-keyed by interpreter path).

## License

This library is free software; you can redistribute it and/or modify it
under the same terms as Perl itself.
