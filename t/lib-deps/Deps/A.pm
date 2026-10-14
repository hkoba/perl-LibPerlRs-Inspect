package Deps::A;
use strict;
use Deps::B;
sub a { Deps::B::b() }
1;
