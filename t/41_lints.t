use strict;
use warnings;
use Test::More;
use OpTree::Analyzer;

sub lints_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    OpTree::Analyzer::analyze($code)->{lints}{lints};
}

subtest 'my $x = EXPR if COND fires (受入条件)' => sub {
    my $l = lints_of('sub { my $x = 5 if $_[0]; $x }');
    is scalar @$l, 1, 'one lint' or diag explain $l;
    is $l->[0]{id}, 'my-in-conditional-statement', 'lint id';
    is $l->[0]{severity}, 'error', 'severity';
    is $l->[0]{var}, '$x', 'variable name';
    ok defined $l->[0]{line}, 'has line';
};

subtest 'unless form fires' => sub {
    my $l = lints_of('sub { my $y = 1 unless $_[0]; $y }');
    is scalar @$l, 1, 'one lint' or diag explain $l;
    is $l->[0]{var}, '$y', 'variable name';
};

subtest 'block if does NOT fire (受入条件)' => sub {
    my $l = lints_of('sub { if ($_[0]) { my $x = 5; return $x } 0 }');
    is_deeply $l, [], 'no lint for block-scoped my' or diag explain $l;
};

subtest 'plain my does NOT fire' => sub {
    my $l = lints_of('sub { my $x = 5; $x }');
    is_deeply $l, [], 'no lint' or diag explain $l;
};

subtest 'my with list assignment under if-modifier fires' => sub {
    my $l = lints_of('sub { my ($a, $b) = (1, 2) if $_[0]; $a }');
    cmp_ok scalar @$l, '>=', 1, 'lint fires for padrange intro' or diag explain $l;
};

done_testing;
