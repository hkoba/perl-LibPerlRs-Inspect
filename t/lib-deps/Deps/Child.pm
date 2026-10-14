package Deps::Child;
use strict;
use parent -norequire, 'Deps::C';
use File::Basename 2.0 qw(basename dirname);
BEGIN { require Deps::C }
1;
