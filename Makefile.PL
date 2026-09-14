use strict;
use warnings;
use ExtUtils::MakeMaker 6.64;    # TEST_REQUIRES
use Config;
use File::Spec;

# The real build is a Rust cargo workspace (crates/inspect-xs produces the cdylib).
# The cargo_build target in MY::postamble does "cargo build + staging into blib"
# and hooks into EUMM's dynamic :: (a pure-EUMM translation of the
# Module::Install::Rust approach).
#
# Separation of build and install:
#   - `make` / `make test`     = release build + staging + tests (as a normal user)
#   - `make debug`             = dev profile (fast, for development iteration)
#   - `make install`           = copy from blib onward **only** (cargo does not run)
# EUMM 7.76 pulls the build chain into install via pure_*_install :: all /
# doc_*_install :: all, so MY::install strips `all` and substitutes the
# blib_ready check. Running cargo as root changes CARGO_HOME (forcing a full
# rebuild) and leaves root-owned files under target/.
#
# Caution: do NOT specify linkext => { LINKTYPE => '' }.
# It breaks the pure_all -> linkext -> dynamic chain, and the postamble hook
# is no longer invoked by make.

# ---- External toolchain check (CPAN Testers "NA" convention: exit 0 without writing a Makefile) ----

my $cargo = $ENV{CARGO} || 'cargo';
my $cargo_ver = `$cargo --version 2>&1`;
unless (defined $cargo_ver && $? == 0) {
    warn "OS unsupported: the Rust toolchain (cargo) is required to build this module.\n"
       . "Install it from https://rustup.rs/ and re-run `perl Makefile.PL`.\n";
    exit 0;
}
print "Using $cargo_ver";

my $perl_h = File::Spec->catfile($Config{archlibexp}, 'CORE', 'perl.h');
unless (-e $perl_h) {
    warn "OS unsupported: $perl_h not found.\n"
       . "Install your vendor's perl development package (e.g. `dnf install perl-devel`).\n";
    exit 0;
}

WriteMakefile(
    NAME             => 'LibPerlRs::Inspect',
    VERSION_FROM     => 'lib/LibPerlRs/Inspect.pm',
    ABSTRACT_FROM    => 'lib/LibPerlRs/Inspect.pm',
    AUTHOR           => 'KOBAYASI, Hiroaki <buribullet@gmail.com>',
    LICENSE          => 'perl_5',
    MIN_PERL_VERSION => '5.042',    # golden tests assume the 5.42 optree shape
    CONFIGURE_REQUIRES => {
        'ExtUtils::MakeMaker' => '6.64',
    },
    PREREQ_PM => {
        'JSON::PP' => 0,    # core
        'XSLoader' => 0,    # core
    },
    TEST_REQUIRES => {
        'Test::More' => 0,
    },
    META_MERGE => {
        'meta-spec'    => { version => 2 },
        dynamic_config => 1,
        no_index       => { directory => ['crates'] },
        resources      => {
            repository => {
                type => 'git',
                url  => 'https://github.com/hkoba/perl-LibPerlRs-Inspect.git',
                web  => 'https://github.com/hkoba/perl-LibPerlRs-Inspect',
            },
            bugtracker => {
                web => 'https://github.com/hkoba/perl-LibPerlRs-Inspect/issues',
            },
        },
        x_external_build_requirements => {
            cargo   => 'Rust toolchain (edition 2024, cargo >= 1.85)',
            network => 'crates.io access at build time (see README.md)',
        },
    },
    clean => { FILES => 'LibPerlRs-Inspect-*.tar.gz' },
);

# MY:: overrides. SUPER:: is resolved relative to the package at compile
# time, so `sub MY::install {...}` (while still in package main) would make
# SUPER point at main. Always define these inside a package MY block.
package MY;

sub postamble {
    my ($self) = @_;
    return sprintf <<'POSTAMBLE', $cargo;
# ---- Rust cdylib (cargo workspace) ------------------------------------
CARGO              = %s
# `make debug` switches to the dev profile (see the debug target below)
CARGO_PROFILE      = release
RUST_TARGET_SUBDIR = release
# To update dependencies, drop --locked with `make CARGO_BUILD_FLAGS=`
CARGO_BUILD_FLAGS  = --locked
RUST_DYLIB         = target/$(RUST_TARGET_SUBDIR)/libinspect_xs.$(SO)
INST_RUSTDYLIB     = $(INST_ARCHAUTODIR)/$(DLBASE).$(DLEXT)
DEBUG_MARKER       = $(INST_ARCHAUTODIR)/.debug-build

# EUMM: all -> pure_all -> linkext -> dynamic (appended with double colon).
# cargo_build always runs; freshness is left to cargo. Staging (cp) is also
# done unconditionally in the same recipe: mtime comparison becomes unsound
# across profile switches, so this is deliberately not a file rule.
dynamic :: cargo_build
	$(NOECHO) $(NOOP)

# PERL=... is read by libperl-config >= 0.3.3; the PATH prefix is a fallback for <= 0.3.2.
# Note: EUMM expands $(FULLPERL) with quotes included, so do not quote it again here.
cargo_build ::
	PERL=$(FULLPERL) PATH="`dirname $(FULLPERL)`:$$PATH" \
	  $(CARGO) build -p inspect-xs --profile $(CARGO_PROFILE) $(CARGO_BUILD_FLAGS)
	$(NOECHO) $(MKPATH) $(INST_ARCHAUTODIR)
	$(CP) $(RUST_DYLIB) $(INST_RUSTDYLIB)
	$(CHMOD) $(PERM_RWX) $(INST_RUSTDYLIB)
	$(NOECHO) sh -c 'if test "$(CARGO_PROFILE)" = release; \
	  then rm -f $(DEBUG_MARKER); else touch $(DEBUG_MARKER); fi'

# Development: run all (including staging) with the dev profile. Test with prove -b t/
debug ::
	$(MAKE) all CARGO_PROFILE=dev RUST_TARGET_SUBDIR=debug

# Precondition check for make install (MY::install substitutes this for
# pure_*_install's all): blib must be staged, and it must be a release build.
blib_ready ::
	$(NOECHO) sh -c 'test -f $(INST_RUSTDYLIB) || \
	  { echo "*** blib is not staged yet. Run \"make\" (and \"make test\") first." >&2; exit 1; }'
	$(NOECHO) sh -c 'test ! -f $(DEBUG_MARKER) || \
	  { echo "*** blib holds a debug build. Run plain \"make\" before \"make install\"." >&2; exit 1; }'

.PHONY: cargo_build debug blib_ready

# clean removes all of target/ (realclean :: depends on clean, so it does too).
# Note that the next build will be a full build including bindgen/macrogen.
clean ::
	$(CARGO) clean
POSTAMBLE
}

sub install {
    my $self = shift;
    my $section = $self->SUPER::install(@_);
    # Detach the build (all) from the install chain: `sudo make install` must
    # not run cargo (as root, CARGO_HOME changes, forcing a full rebuild and
    # polluting target/ with root-owned files). Check via blib_ready instead
    # that a release build has been staged.
    $section =~ s/^(pure_(?:perl|site|vendor)_install ::) all\b/$1 blib_ready/mg;
    $section =~ s/^(doc_(?:perl|site|vendor)_install ::) all\b/$1/mg;
    return $section;
}
