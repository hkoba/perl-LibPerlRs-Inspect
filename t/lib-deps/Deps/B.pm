package Deps::B;
use strict;
use Deps::A ();
sub b {
    require Deps::C;
    my $name = 'Deps::C';
    require $name;
    1;
}
1;
