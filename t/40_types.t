use strict;
use warnings;
use Test::More;
use OpTree::Analyzer;

sub types_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    OpTree::Analyzer::analyze($code)->{types};
}

sub var_of {
    my ($types, $name) = @_;
    my ($v) = grep { $_->{name} eq $name } @{$types->{vars}};
    $v;
}

subtest 'arrayref vs hashref conflict (受入条件)' => sub {
    my $t = types_of('sub { my ($x) = @_; $x->[0] + $x->{k} }');
    my $x = var_of($t, '$x');
    ok $x, 'var $x reported' or diag explain $t;
    ok +(grep { $_ eq 'ARRAYref' } @{$x->{facets}}), 'ARRAYref facet';
    ok +(grep { $_ eq 'HASHref' } @{$x->{facets}}), 'HASHref facet';
    ok scalar @{$x->{conflicts}}, 'conflict reported';
    my @deref_ev = grep { $_->{facet} =~ /ref$/ } @{$x->{evidence}};
    cmp_ok scalar @deref_ev, '>=', 2, 'two evidences';
    ok defined $deref_ev[0]{line}, 'evidence has line number';
};

subtest 'declared type via PadnameTYPE (my TFoo $x)' => sub {
    my $t = types_of('package TFoo {} sub { my TFoo $x = shift; $x }');
    my $x = var_of($t, '$x');
    ok $x, 'typed var reported' or diag explain $t;
    is $x->{declared_type}, 'TFoo', 'declared_type captured';
};

subtest 'method inventory (typo 検出の下地)' => sub {
    my $t = types_of('sub { my ($obj) = @_; $obj->frobnicate(1); $obj->save }');
    my $o = var_of($t, '$obj');
    ok $o, 'var $obj reported' or diag explain $t;
    ok +(grep { $_ eq 'Object' } @{$o->{facets}}), 'Object facet';
    is_deeply [sort @{$o->{methods}}], ['frobnicate', 'save'], 'method names collected';
};

subtest 'numeric and string facets' => sub {
    my $t = types_of('sub { my ($n, $s) = @_; my $a = $n + 1; my $b = $s . "x"; ($a, $b) }');
    my $n = var_of($t, '$n');
    my $s = var_of($t, '$s');
    ok +(grep { $_ eq 'Num' } @{$n->{facets}}), '$n has Num' or diag explain $t;
    ok +(grep { $_ eq 'Str' } @{$s->{facets}}), '$s has Str' or diag explain $t;
};

subtest 'chained deref renders and does not crash' => sub {
    my $t = types_of('sub { my ($x) = @_; $x->[0]{k} }');
    my $x = var_of($t, '$x');
    ok $x, 'var reported' or diag explain $t;
    ok +(grep { $_ eq 'ARRAYref' } @{$x->{facets}}), 'first step ARRAYref';
    my $expected_chain = '$x->[0]{k}';
    like $x->{evidence}[0]{why}, qr/\Q$expected_chain\E/, 'rendered chain in evidence'
        or diag explain $x;
};

done_testing;
