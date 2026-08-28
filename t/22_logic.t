use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

sub logic_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    LibPerlRs::Inspect::analyze($code)->{logic};
}

# when を [[cond添字, 0/1], ...] に正規化 (JSON::PP::Boolean 対策)
sub whens {
    my ($p) = @_;
    [map { [$_->{cond}, $_->{value} ? 1 : 0] } @{$p->{when}}];
}

# 真理値表から inputs が一致する行の path 添字を引く
sub table_path {
    my ($l, @in) = @_;
  ROW: for my $r (@{$l->{table}}) {
        my @i = map { $_ ? 1 : 0 } @{$r->{inputs}};
        for my $j (0 .. $#in) {
            next ROW if $i[$j] != $in[$j];
        }
        return $r->{path};
    }
    return;
}

subtest 'early return + if/elsif/else chain' => sub {
    my $l = logic_of(q{sub { my ($v) = @_; return unless $v; if ($v > 10) { "big" } elsif ($v > 5) { "mid" } else { "small" } }});
    is_deeply $l->{conds}, ['$v', '$v > 10', '$v > 5'], 'conds in order of appearance';

    my @p = @{$l->{paths}};
    is scalar @p, 4, '4 paths';
    is $p[0]{kind}, 'return', 'path 0 kind';
    is_deeply whens($p[0]), [[0, 0]], 'return unless $v';
    is $p[1]{kind}, 'implicit', 'path 1 kind';
    is_deeply $p[1]{exprs}, ['"big"'], 'big expr';
    is_deeply whens($p[1]), [[0, 1], [1, 1]], 'big when';
    is_deeply $p[2]{exprs}, ['"mid"'], 'mid expr';
    is_deeply whens($p[2]), [[0, 1], [1, 0], [2, 1]], 'mid when';
    is_deeply $p[3]{exprs}, ['"small"'], 'small expr';
    is_deeply whens($p[3]), [[0, 1], [1, 0], [2, 0]], 'small when';

    is scalar @{$l->{table}}, 8, '2^3 rows';
    is table_path($l, 0, 0, 0), 0, 'falsy $v -> early return';
    is table_path($l, 0, 1, 1), 0, 'falsy $v dominates later conds';
    is table_path($l, 1, 1, 0), 1, 'big';
    is table_path($l, 1, 0, 1), 2, 'mid';
    is table_path($l, 1, 0, 0), 3, 'small';
};

subtest 'ternary in tail position' => sub {
    my $l = logic_of('sub { my ($x) = @_; $x ? "yes" : "no" }');
    is_deeply $l->{conds}, ['$x'], 'single cond';
    is scalar @{$l->{paths}}, 2, '2 paths';
    is_deeply $l->{paths}[0]{exprs}, ['"yes"'], 'true arm';
    is_deeply whens($l->{paths}[0]), [[0, 1]], 'true arm when';
    is_deeply $l->{paths}[1]{exprs}, ['"no"'], 'false arm';
    is table_path($l, 1), 0, 'table true row';
    is table_path($l, 0), 1, 'table false row';
};

subtest 'die guard: die if $x' => sub {
    my $l = logic_of(q{sub { my ($x) = @_; die "boom\n" if $x; "ok" }});
    is_deeply $l->{conds}, ['$x'], 'cond';
    is $l->{paths}[0]{kind}, 'die', 'die path';
    like $l->{paths}[0]{exprs}[0], qr/boom/, 'die message';
    is_deeply whens($l->{paths}[0]), [[0, 1]], 'die when $x';
    is $l->{paths}[1]{kind}, 'implicit', 'fallthrough path';
    is_deeply $l->{paths}[1]{exprs}, ['"ok"'], 'ok expr';
    is_deeply whens($l->{paths}[1]), [[0, 0]], 'ok when !$x';
};

subtest 'croak guard' => sub {
    my $l = logic_of(q{use Carp; sub { my ($x) = @_; croak("bad") if $x < 0; $x }});
    is_deeply $l->{conds}, ['$x < 0'], 'cond';
    is $l->{paths}[0]{kind}, 'croak', 'croak path';
    is_deeply whens($l->{paths}[0]), [[0, 1]], 'croak when';
    is $l->{paths}[1]{kind}, 'implicit', 'implicit path';
    is_deeply $l->{paths}[1]{exprs}, ['$x'], 'implicit expr';
    is_deeply whens($l->{paths}[1]), [[0, 0]], 'implicit when';
};

subtest 'defined-or in tail position' => sub {
    my $l = logic_of(q{sub { my ($x) = @_; $x // "default" }});
    is_deeply $l->{conds}, ['defined($x)'], 'dor atom';
    is_deeply $l->{paths}[0]{exprs}, ['"default"'], 'rhs when undef';
    is_deeply whens($l->{paths}[0]), [[0, 0]], 'rhs when';
    is_deeply $l->{paths}[1]{exprs}, ['$x'], 'lhs value when defined';
    is_deeply whens($l->{paths}[1]), [[0, 1]], 'lhs when';
};

subtest 'no conditions' => sub {
    my $l = logic_of('sub { 42 }');
    is_deeply $l->{conds}, [], 'no conds';
    is scalar @{$l->{paths}}, 1, 'single path';
    is $l->{paths}[0]{kind}, 'implicit', 'implicit';
    is_deeply $l->{paths}[0]{exprs}, ['42'], 'expr';
    is_deeply whens($l->{paths}[0]), [], 'unconditional';
    ok !defined $l->{table}, 'no table for 0 conds';

    my $e = logic_of('sub {}');
    is_deeply $e->{paths}, [], 'empty sub: no paths';
};

subtest 'die inside eval is guarded but execution continues' => sub {
    my $l = logic_of(q{sub { my ($x) = @_; eval { die "boom\n" if $x }; $@ }});
    is_deeply $l->{conds}, ['$x'], 'cond from inside eval';
    is $l->{paths}[0]{kind}, 'die', 'die path recorded';
    is_deeply whens($l->{paths}[0]), [[0, 1]], 'die when $x';
    is $l->{paths}[1]{kind}, 'implicit', 'final $@';
    is_deeply $l->{paths}[1]{exprs}, ['$@'], '$@ expr';
    is_deeply whens($l->{paths}[1]), [], 'reached regardless (die was caught)';
};

done_testing;
