# LibPerlRs::Inspect

Analyze a Perl subroutine reference (including anonymous subs returned
by `eval`) at the OP-tree level, powered by Rust. Part of the
`LibPerlRs::*` family built on
[libperl-rs](https://github.com/hkoba/libperl-rs), alongside
[`LibPerlRs::PartialEval`](https://github.com/hkoba/perl-LibPerlRs-PartialEval).
Reports:

- argument specification (signatures, `my (...) = @_`, `shift`/`pop`,
  `$_[n]` — min/max arity, parameter names, defaults)
- return-value / exception specification (`return` sites, implicit last
  expression, `die` / `Carp::croak`, `wantarray` use)
- truth tables for guard conditions, with `&&` / `||` / `!` / `//` / `?:`
  decomposed into atomic conditions (see "Truth tables" below)
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

## Truth tables

The `logic` section of the report describes every way a sub can finish.
Branch conditions and the boolean operators reachable from them, from a
`return` expression, or from the implicit final expression are broken
down into *atomic conditions* (`conds`), and each outcome is listed with
the conjunction of atom values that leads to it (`paths[].when`). With at
most 6 atoms the report also contains a full `table`, one row per truth
assignment, pointing at the first matching path.
`examples/truth_table.pl` renders that table:

```console
$ perl -Mblib examples/truth_table.pl 'sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }'
source: sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }

$a | $b | $c | outcome
---+----+----+--------
F  | F  | F  | #0 L1 return !$c
F  | F  | T  | #0 L1 return !$c
F  | T  | F  | #0 L1 return !$c
F  | T  | T  | #0 L1 return !$c
T  | F  | F  | #2 L1 return !$c
T  | F  | T  | #2 L1 return !$c
T  | T  | F  | #1 L1 return $b
T  | T  | T  | #1 L1 return $b

kinds: return = explicit return; die/croak/confess = exception; implicit = last evaluated expression (the sub's implicit return value)
```

The outcome column reads `#<path index> L<line> <kind> <expr>` (`-` when
no path matches the row). `implicit` is a path that ends without an
explicit `return`: the sub's value is the last expression evaluated on
it. As in Perl, `&&` and `||` yield the value of the operand that decided
them, so the outcome shows `$b`, not a boolean. The same atoms are
shared across an `if` / `elsif` chain, so `if ($x && !$y) {...} elsif
($x) {...} else {...}` gets a 4-row table with no unreachable rows.
Statements executed inside a branch arm before the outcome are listed
under the table as `paths with side effects` (they are also in the
report as `paths[].stmts`):

```console
$ perl -Mblib examples/truth_table.pl 'sub { my ($x) = @_; if ($x) { log_it("a"); note(); return 1 } "z" }'
...
$x | outcome
---+--------
F  | #1 L1 implicit "z"
T  | #0 L1 return 1

paths with side effects:
  #0 L1 return 1
      L1 log_it("a")
      L1 note()
```

Expressions are rendered by a small deparser that covers the ops seen in
practice (calls, `print` / `say` / `printf` with filehandles, string
interpolation, `grep` / `map`, anonymous hashes and lists, list
assignment, file tests, ...) and prints anything else as
`name(args, ...)`. Regex patterns are not captured, so a match renders as
`$x =~ m/.../`, and two different patterns on the same variable count as
one atom.

The corresponding report fragment (`analyze($sub)->{logic}`):

```perl
{
  conds => ['$a', '$b', '$c'],
  paths => [
    { kind => 'return', line => 1, exprs => ['!$c'], when => [{cond => 0, value => 0}], stmts => [] },
    { kind => 'return', line => 1, exprs => ['$b'],  when => [{cond => 0, value => 1}, {cond => 1, value => 1}], stmts => [] },
    { kind => 'return', line => 1, exprs => ['!$c'], when => [{cond => 0, value => 1}, {cond => 1, value => 0}], stmts => [] },
  ],
  table => [ { inputs => [0, 0, 0], path => 0 }, ... ],   # 2**3 rows
}
```

Each short-circuit route is its own path, so one `return` site can
appear several times; count return *sites* with the `returns` section
instead. Atoms are identified by their rendered text, so no semantic
implication (`$v > 10` implies `$v > 5`) is applied. Decomposition covers
guard conditions, single-expression `return`s and the implicit final
value; assignments, call arguments, `die` messages, loop conditions and
the `&&=` / `||=` / `//=` family stay opaque.

## perl-inspect CLI

`crates/inspect-cli` provides `perl-inspect`, a standalone binary with
its own embedded perl: it *compiles* the target (BEGIN/`use` run, the
main body does not — the same trust model as `perl -c`), walks the
symbol table, and prints a JSON report (`schema_version: 1`) of every
package/sub with a provenance tag (`file` / `imported` / `xs`), source
line ranges, and the argument-spec analysis for subs defined in the
target file (`--deep` embeds the full report: args / returns / logic /
types / lints). The report also reconstructs the file's own `use`
statements **with their import arguments** (`uses`, via BEGIN capture +
`B::Deparse::begin_is_use` — the technique shared with
`LibPerlRs::PartialEval`'s `compile_info`), plus the line numbers of
opaque `BEGIN` blocks (`opaque_begins`).

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
directly (a re-link step would be needed).

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
