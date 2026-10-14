package Cyc::Main;
# Loads fine as a module (`use Cyc::Main`). Compiled as the main program
# (`perl -c Cyc/Main.pm`) it is not in %INC, so the cycle below loads
# this file a second time and the fatal `redefine` warning kills the
# compilation.
use strict;
use warnings FATAL => 'redefine';
sub early { 'defined before the cycle' }
use base 'Cyc::Gate';
sub late { 'defined after the cycle' }
1;
