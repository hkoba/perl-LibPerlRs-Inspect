#!/bin/zsh
set -e
cd $0:a:h

make all
prove -b t/
