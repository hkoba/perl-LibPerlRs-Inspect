#!/bin/zsh
set -e
cd $0:a:h

# 開発用ワンショット: 初回のみ Makefile を生成 (dev プロファイル)。
# release でのインストールは perl Makefile.PL && make && make test && make install
[[ -f Makefile ]] || perl Makefile.PL --debug
make test
