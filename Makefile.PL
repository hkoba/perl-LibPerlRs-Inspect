use strict;
use warnings;
use ExtUtils::MakeMaker 6.64;    # TEST_REQUIRES
use Config;
use File::Spec;

# 実体は Rust の cargo workspace (crates/analyzer-xs が cdylib を生成)。
# MY::postamble の cargo_build ターゲットが「cargo ビルド + blib への staging」
# を行い、EUMM の dynamic :: にフックする (Module::Install::Rust 方式の
# 純 EUMM 翻訳)。
#
# ビルドと install の分離:
#   - `make` / `make test`     = release ビルド + staging + テスト (通常ユーザで)
#   - `make debug`             = dev プロファイル (開発イテレーション用に高速)
#   - `make install`           = blib から先へのコピー**のみ** (cargo は走らない)
# EUMM 7.76 は pure_*_install :: all / doc_*_install :: all で install から
# ビルド連鎖を引き込むため、MY::install で `all` を剥がし blib_ready 検査に
# 差し替えている。root で cargo が走ると CARGO_HOME が変わって全再ビルドに
# なる上、target/ に root 所有ファイルが残るため。
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

# MY:: の override 群。SUPER:: はコンパイル時のパッケージ基準で解決される
# ため、`sub MY::install {...}` (package main のまま) では SUPER が main を
# 指してしまう。必ず package MY ブロック内で定義する。
package MY;

sub postamble {
    my ($self) = @_;
    return sprintf <<'POSTAMBLE', $cargo;
# ---- Rust cdylib (cargo workspace) ------------------------------------
CARGO              = %s
# `make debug` で dev プロファイルに切り替わる (下記 debug ターゲット参照)
CARGO_PROFILE      = release
RUST_TARGET_SUBDIR = release
# 依存を更新したい時は `make CARGO_BUILD_FLAGS=` で --locked を外す
CARGO_BUILD_FLAGS  = --locked
RUST_DYLIB         = target/$(RUST_TARGET_SUBDIR)/libanalyzer_xs.$(SO)
INST_RUSTDYLIB     = $(INST_ARCHAUTODIR)/$(DLBASE).$(DLEXT)
DEBUG_MARKER       = $(INST_ARCHAUTODIR)/.debug-build

# EUMM: all -> pure_all -> linkext -> dynamic (双コロンで追記)。
# cargo_build は常時実行し、鮮度判定は cargo に委ねる。staging (cp) も
# 同一レシピで無条件に行う: プロファイル切替をまたぐと mtime 比較が
# 不健全になるため、file ルールにはしない。
dynamic :: cargo_build
	$(NOECHO) $(NOOP)

# PERL=... は libperl-config >= 0.3.3 が参照。PATH 前置は 0.3.2 以前用の保険。
# 注: EUMM の $(FULLPERL) はクォート込みで展開されるので、ここでは重ねない。
cargo_build ::
	PERL=$(FULLPERL) PATH="`dirname $(FULLPERL)`:$$PATH" \
	  $(CARGO) build -p analyzer-xs --profile $(CARGO_PROFILE) $(CARGO_BUILD_FLAGS)
	$(NOECHO) $(MKPATH) $(INST_ARCHAUTODIR)
	$(CP) $(RUST_DYLIB) $(INST_RUSTDYLIB)
	$(CHMOD) $(PERM_RWX) $(INST_RUSTDYLIB)
	$(NOECHO) sh -c 'if test "$(CARGO_PROFILE)" = release; \
	  then rm -f $(DEBUG_MARKER); else touch $(DEBUG_MARKER); fi'

# 開発用: dev プロファイルで all (staging 含む) を回す。テストは prove -b t/
debug ::
	$(MAKE) all CARGO_PROFILE=dev RUST_TARGET_SUBDIR=debug

# make install の前提検査 (MY::install が pure_*_install の all をこれに
# 差し替える): staging 済みで、かつ release ビルドであること。
blib_ready ::
	$(NOECHO) sh -c 'test -f $(INST_RUSTDYLIB) || \
	  { echo "*** blib is not staged yet. Run \"make\" (and \"make test\") first." >&2; exit 1; }'
	$(NOECHO) sh -c 'test ! -f $(DEBUG_MARKER) || \
	  { echo "*** blib holds a debug build. Run plain \"make\" before \"make install\"." >&2; exit 1; }'

.PHONY: cargo_build debug blib_ready

# clean で target/ ごと削除 (realclean :: clean 依存なので realclean も同様)。
# 次のビルドは bindgen/macrogen 込みのフルビルドになる点に注意。
clean ::
	$(CARGO) clean
POSTAMBLE
}

sub install {
    my $self = shift;
    my $section = $self->SUPER::install(@_);
    # install 連鎖からビルド (all) を切り離す: `sudo make install` で cargo を
    # 走らせない (root では CARGO_HOME が変わり全再ビルド + root 所有の
    # target/ 汚染になる)。代わりに blib_ready で staged 済み release かを検査。
    $section =~ s/^(pure_(?:perl|site|vendor)_install ::) all\b/$1 blib_ready/mg;
    $section =~ s/^(doc_(?:perl|site|vendor)_install ::) all\b/$1/mg;
    return $section;
}
