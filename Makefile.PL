use strict;
use warnings;
use ExtUtils::MakeMaker 6.64;    # TEST_REQUIRES
use Config;
use File::Spec;

# 実体は Rust の cargo workspace (crates/analyzer-xs が cdylib を生成)。
# MY::postamble で cargo を呼び、成果物を $(INST_ARCHAUTODIR)/$(DLBASE).$(DLEXT)
# にコピーして EUMM の dynamic :: にフックする (Module::Install::Rust 方式の
# 純 EUMM 翻訳)。
#
# 注意: linkext => { LINKTYPE => '' } を指定してはならない。
# pure_all → linkext → dynamic の連鎖が切れて postamble のフックが
# make から呼ばれなくなる。

# ---- 外部ツールチェーン検査 (CPAN Testers "NA" 慣行: Makefile を書かず exit 0) ----

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

# ---- cargo プロファイル選択 ----
# 開発時: `perl Makefile.PL --debug` で dev プロファイル (高速ビルド)。
# 非 KEY=VALUE 引数は EUMM parse_args が黙って無視し、Makefile 再生成
# ルールには @ARGV 原文が保存されるので、strip せずそのまま通す
# (strip すると Makefile.PL 更新時の再生成で release に化ける)。

my $debug          = grep { $_ eq '--debug' } @ARGV;
my $cargo_profile  = $debug ? 'dev'   : 'release';
my $target_subdir  = $debug ? 'debug' : 'release';    # profile dev は target/debug に出る

WriteMakefile(
    NAME             => 'OpTree::Analyzer',
    VERSION_FROM     => 'lib/OpTree/Analyzer.pm',
    ABSTRACT_FROM    => 'lib/OpTree/Analyzer.pm',
    AUTHOR           => 'KOBAYASI, Hiroaki <buribullet@gmail.com>',
    LICENSE          => 'perl_5',
    MIN_PERL_VERSION => '5.042',    # golden テストが 5.42 の optree 形状前提
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
        # resources.repository は GitHub 公開時に追記
        x_external_build_requirements => {
            cargo   => 'Rust toolchain (edition 2024, cargo >= 1.85)',
            network => 'crates.io access at build time (see README.md)',
        },
    },
    clean => { FILES => 'OpTree-Analyzer-*.tar.gz' },
);

sub MY::postamble {
    my ($self) = @_;
    return sprintf <<'POSTAMBLE', $cargo, $cargo_profile, $target_subdir;
# ---- Rust cdylib (cargo workspace) ------------------------------------
CARGO              = %s
CARGO_PROFILE      = %s
RUST_TARGET_SUBDIR = %s
# 依存を更新したい時は `make CARGO_BUILD_FLAGS=` で --locked を外す
CARGO_BUILD_FLAGS  = --locked
RUST_DYLIB         = target/$(RUST_TARGET_SUBDIR)/libanalyzer_xs.$(SO)
INST_RUSTDYLIB     = $(INST_ARCHAUTODIR)/$(DLBASE).$(DLEXT)

# EUMM: all -> pure_all -> linkext -> dynamic (双コロンで追記)
dynamic :: $(INST_RUSTDYLIB)
	$(NOECHO) $(NOOP)

# 依存なし双コロン = 毎回実行し、鮮度判定は cargo に委ねる。
# PERL=... は libperl-config >= 0.3.3 が参照。PATH 前置は 0.3.2 以前用の保険。
# 注: EUMM の $(FULLPERL) はクォート込みで展開されるので、ここでは重ねない。
$(RUST_DYLIB) ::
	PERL=$(FULLPERL) PATH="`dirname $(FULLPERL)`:$$PATH" \
	  $(CARGO) build -p analyzer-xs --profile $(CARGO_PROFILE) $(CARGO_BUILD_FLAGS)

# blibdirs は .PHONY なので依存に使わず、-j 安全のため自前 MKPATH (冪等)
$(INST_RUSTDYLIB) : $(RUST_DYLIB)
	$(NOECHO) $(MKPATH) $(INST_ARCHAUTODIR)
	$(CP) $(RUST_DYLIB) $@
	$(CHMOD) $(PERM_RWX) $@

# clean では cargo clean しない (bindgen/macrogen の再生成が高コスト)
realclean ::
	$(CARGO) clean
POSTAMBLE
}
