package Deps::B;
use strict;
use Deps::A ();
sub b {
    require Deps::C;
    my $name = 'Deps/C.pm';
    require $name;
    1;
}
1;
