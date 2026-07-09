#!/bin/zsh
set -e
cd $0:a:h

# 開発用ワンショット: dev プロファイルでビルド + blib 直テスト。
# release でのインストールは perl Makefile.PL && make && make test && make install
[[ -f Makefile ]] || perl Makefile.PL
make debug
prove -b t/
