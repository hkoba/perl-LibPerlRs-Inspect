use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

# Names are rendered as written in the source: a sub, package variable or
# filehandle of the sub's own package is bare, anything else qualified.

package Foo;
our ($flag, %h);
sub helper { $_[0] }
sub check {
    my ($x) = @_;
    return 1 if helper($x) && main::outside($x) && Bar::other($x);
    return 2 if $Foo::flag || $main::g || $h{k};
    print STDERR "x";
    return 3 if $ENV{HOME} eq $_;
    0;
}

package main;
sub outside { 1 }
sub Bar::other { 1 }
our $g = 1;
sub check_main { my ($x) = @_; return 1 if outside($x) && Foo::helper($x) && $Foo::flag; 0 }

my $l = LibPerlRs::Inspect::analyze(\&Foo::check)->{logic};
is_deeply $l->{conds},
    ['helper($x)', 'main::outside($x)', 'Bar::other($x)', '$flag', '$main::g', '$h{k}', '$ENV{HOME} eq $_'],
    'in package Foo' or diag explain $l->{conds};
my @stmts = map { $_->{text} } map { @{$_->{stmts}} } @{$l->{paths}};
ok((grep { $_ eq 'print STDERR "x"' } @stmts), 'STDERR stays bare in another package') or diag explain \@stmts;

my $m = LibPerlRs::Inspect::analyze(\&main::check_main)->{logic};
is_deeply $m->{conds}, ['outside($x)', 'Foo::helper($x)', '$Foo::flag'], 'in package main'
    or diag explain $m->{conds};

my $cap = LibPerlRs::Inspect::capture(\&Foo::check);
is $cap->{package}, 'Foo', 'the sub\'s package is captured';

done_testing;
