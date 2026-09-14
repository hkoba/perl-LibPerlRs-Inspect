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

# Normalize `when` to [[cond_index, 0/1], ...] (works around JSON::PP::Boolean)
sub whens {
    my ($p) = @_;
    [map { [$_->{cond}, $_->{value} ? 1 : 0] } @{$p->{when}}];
}

# Look up the path index of the truth-table row whose inputs match
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

subtest 'boolean operators in a return expression are decomposed' => sub {
    my $l = logic_of('sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }');
    is_deeply $l->{conds}, ['$a', '$b', '$c'], 'one atom per operand';
    my @p = @{$l->{paths}};
    is scalar @p, 3, '3 short-circuit routes';
    is $p[$_]{kind}, 'return', "path $_ kind" for 0 .. 2;
    is_deeply $p[0]{exprs}, ['!$c'], 'route 0: $a false -> !$c';
    is_deeply whens($p[0]), [[0, 0]], 'route 0 when';
    is_deeply $p[1]{exprs}, ['$b'], 'route 1: $a && $b true -> $b';
    is_deeply whens($p[1]), [[0, 1], [1, 1]], 'route 1 when';
    is_deeply $p[2]{exprs}, ['!$c'], 'route 2: $a true, $b false -> !$c';
    is_deeply whens($p[2]), [[0, 1], [1, 0]], 'route 2 when';
    is scalar @{$l->{table}}, 8, '2^3 rows';
    is table_path($l, 0, 1, 1), 0, '$a false';
    is table_path($l, 1, 0, 0), 2, '$a true, $b false';
    is table_path($l, 1, 1, 0), 1, '$a and $b true';
};

subtest 'compound guard in if/elsif shares atoms' => sub {
    my $l = logic_of('sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }');
    is_deeply $l->{conds}, ['$x', '$y'], 'atoms shared across the chain';
    my @p = @{$l->{paths}};
    is scalar @p, 3, '3 paths';
    is_deeply $p[0]{exprs}, ['"a"'], 'a expr';
    is_deeply whens($p[0]), [[0, 1], [1, 0]], 'a when';
    is_deeply $p[1]{exprs}, ['"b"'], 'b expr';
    is_deeply whens($p[1]), [[0, 1], [1, 1]], 'b when';
    is_deeply $p[2]{exprs}, ['"c"'], 'c expr';
    is_deeply whens($p[2]), [[0, 0]], 'c when';
    is scalar @{$l->{table}}, 4, '2^2 rows';
    ok((!grep { !defined $_->{path} } @{$l->{table}}), 'no unreachable rows');
    is table_path($l, 1, 0), 0, 'a';
    is table_path($l, 1, 1), 1, 'b';
    is table_path($l, 0, 1), 2, 'c';
};

subtest 'early return with compound guard fans out the continuation' => sub {
    my $l = logic_of('sub { my ($x, $y) = @_; return if $x && $y; "rest" }');
    is_deeply $l->{conds}, ['$x', '$y'], 'conds';
    my @p = @{$l->{paths}};
    is scalar @p, 3, '1 return + 2 fall-through routes';
    is $p[0]{kind}, 'return', 'return path';
    is_deeply whens($p[0]), [[0, 1], [1, 1]], 'return when both true';
    is_deeply $p[1]{exprs}, ['"rest"'], 'rest (route 1)';
    is_deeply whens($p[1]), [[0, 0]], 'rest when $x false';
    is_deeply $p[2]{exprs}, ['"rest"'], 'rest (route 2)';
    is_deeply whens($p[2]), [[0, 1], [1, 0]], 'rest when $x true, $y false';
    is table_path($l, 1, 1), 0, 'table: return';
    is table_path($l, 0, 1), 1, 'table: rest via $x false';
    is table_path($l, 1, 0), 2, 'table: rest via $y false';
};

subtest 'contradictory literals prune unreachable arms' => sub {
    my $l = logic_of('sub { my ($x) = @_; return unless $x; if ($x) { "a" } else { "b" } }');
    is_deeply $l->{conds}, ['$x'], 'single atom';
    my @p = @{$l->{paths}};
    is scalar @p, 2, 'the else arm is unreachable and not reported';
    is $p[0]{kind}, 'return', 'early return';
    is_deeply whens($p[0]), [[0, 0]], 'return when !$x';
    is_deeply $p[1]{exprs}, ['"a"'], 'a';
    is_deeply whens($p[1]), [[0, 1]], 'a when $x (no duplicate literal)';
};

subtest 'negated compound return' => sub {
    my $l = logic_of('sub { my ($a, $b) = @_; return !($a && $b) }');
    is_deeply $l->{conds}, ['$a', '$b'], 'conds';
    my @p = @{$l->{paths}};
    is scalar @p, 2, '2 routes';
    is_deeply $p[0]{exprs}, ['!$a'], 'negated lhs';
    is_deeply whens($p[0]), [[0, 0]], 'when $a false';
    is_deeply $p[1]{exprs}, ['!$b'], 'negated rhs';
    is_deeply whens($p[1]), [[0, 1]], 'when $a true';
};

subtest 'ternary inside a return' => sub {
    my $l = logic_of('sub { my ($x, $y) = @_; return $x ? $y : 0 }');
    is_deeply $l->{conds}, ['$x'], 'arm values are not atoms';
    my @p = @{$l->{paths}};
    is_deeply $p[0]{exprs}, ['$y'], 'true arm';
    is_deeply whens($p[0]), [[0, 1]], 'true arm when';
    is_deeply $p[1]{exprs}, ['0'], 'false arm';
    is_deeply whens($p[1]), [[0, 0]], 'false arm when';
};

done_testing;
