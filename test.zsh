#!/bin/zsh
set -e
cd $0:a:h

# Development one-shot: build with the dev profile + test directly against blib.
# For a release install: perl Makefile.PL && make && make test && make install
[[ -f Makefile ]] || perl Makefile.PL
make debug
prove -b t/
