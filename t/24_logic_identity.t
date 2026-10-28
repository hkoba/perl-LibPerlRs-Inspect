use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

# Atom identity: the same condition spelled twice is one column unless the
# variable was modified in between; `a >= b` is the negation of `a < b`.

sub logic_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    LibPerlRs::Inspect::analyze($code)->{logic};
}

sub whens {
    my ($p) = @_;
    [map { [$_->{cond}, $_->{value} ? 1 : 0] } @{$p->{when}}];
}

sub site_negs {
    my ($l, $i) = @_;
    [map { $_->{neg} ? 1 : 0 } @{$l->{cond_sites}[$i]}];
}

subtest 'defined(my $y = f()) and defined($y) are one atom' => sub {
    my $l = logic_of('sub { my ($x) = @_; return 0 unless defined(my $y = f($x)); return 1 if defined($y); 2 }');
    is scalar @{$l->{conds}}, 1, 'one cond' or diag explain $l->{conds};
    is scalar @{$l->{cond_sites}[0]}, 2, 'two sites';
    is_deeply site_negs($l, 0), [0, 0], 'same polarity';
    is_deeply [map { $_->{text} } @{$l->{cond_sites}[0]}],
        [$l->{conds}[0], 'defined($y)'], 'site texts';
    is_deeply [map { whens($_) } @{$l->{paths}}], [[[0, 0]], [[0, 1]]], 'return 0 / return 1';
    is scalar @{$l->{paths}}, 2, 'the implicit 2 is unreachable';
};

subtest 'an assignment in between keeps them apart' => sub {
    my $l = logic_of('sub { my ($x) = @_; return 0 unless defined(my $y = f($x)); $y = g(); return 1 if defined($y); 2 }');
    is scalar @{$l->{conds}}, 2, 'two conds' or diag explain $l->{conds};
    is scalar @{$l->{paths}}, 3, 'three outcomes';
};

subtest 'length($s) < 8 and length($s) >= 8 are one atom' => sub {
    my $l = logic_of('sub { my ($s) = @_; return "short" if length($s) < 8; return "ok" if length($s) >= 8; "x" }');
    is_deeply $l->{conds}, ['length($s) < 8'], 'first spelling names the column';
    is_deeply site_negs($l, 0), [0, 1], 'second site is negated';
    is $l->{cond_sites}[0][1]{text}, 'length($s) >= 8', 'site text';
    is_deeply [map { [$_->{exprs}[0], whens($_)] } @{$l->{paths}}],
        [['"short"', [[0, 1]]], ['"ok"', [[0, 0]]]], 'ok when !(length < 8); "x" unreachable';
    ok !(grep { !defined $_->{path} } @{$l->{table}}), 'no row without an outcome';
};

subtest '$a > $b and $b >= $a' => sub {
    my $l = logic_of('sub { my ($a, $b) = @_; if ($a > $b) { "a" } elsif ($b >= $a) { "b" } else { "c" } }');
    is_deeply $l->{conds}, ['$a > $b'], 'one cond';
    is_deeply [map { $_->{exprs}[0] } @{$l->{paths}}], ['"a"', '"b"'], '"c" unreachable';
    is_deeply site_negs($l, 0), [0, 1], 'negated site';
    # `$b <= $a` is a different condition (differs when $a == $b)
    my $m = logic_of('sub { my ($a, $b) = @_; if ($a > $b) { "a" } elsif ($b <= $a) { "b" } else { "c" } }');
    is scalar @{$m->{conds}}, 2, '$b <= $a is not !($a > $b)';
};

subtest 'string comparison and operand order' => sub {
    my $l = logic_of('sub { my ($a, $b) = @_; return 1 if $a eq $b; return 2 if $b ne $a; 3 }');
    is scalar @{$l->{conds}}, 1, 'eq / ne with swapped operands';
    is_deeply site_negs($l, 0), [0, 1], 'ne is negated';
    my $m = logic_of('sub { my ($a, $b) = @_; return 1 if $a lt $b; return 2 if $b gt $a; 3 }');
    is scalar @{$m->{conds}}, 1, 'lt / gt swapped';
    is_deeply site_negs($m, 0), [0, 0], 'same polarity';
};

subtest 'element assignment modifies the root variable' => sub {
    my $l = logic_of('sub { my (%h) = @_; return 1 if $h{k}; $h{k} = 1; return 2 if $h{k}; 3 }');
    is scalar @{$l->{conds}}, 2, '$h{k} = 1 separates';
    my $m = logic_of('sub { my ($r) = @_; return 1 unless defined $r; $r->{x} = 1; return 2 unless defined $r; 3 }');
    is scalar @{$m->{conds}}, 2, '$r->{x} = 1 separates defined($r) (conservative)';
    my $n = logic_of('sub { my ($r) = @_; return 1 unless $r->{a}; return 2 unless $r->{a}; 3 }');
    is scalar @{$n->{conds}}, 1, 'the same element read twice';
};

subtest 'sub-call aliasing is a modification only on request' => sub {
    my $src = 'sub { my ($x) = @_; return 1 unless defined $x; f($x); return 2 unless defined $x; 3 }';
    my $l = logic_of($src);
    is scalar @{$l->{conds}}, 1, 'f($x) does not separate by default';
    my $code = eval $src or die $@;
    my $m = LibPerlRs::Inspect::analyze_logic_opts($code, { alias_modifies => 1 });
    is scalar @{$m->{conds}}, 2, 'alias_modifies separates';
};

subtest 'a loop separates before / inside / after' => sub {
    my $l = logic_of('sub { my ($x, @a) = @_; return 0 if $x > 1; for my $i (@a) { $x = $i; return 1 if $x > 1 } return 2 if $x > 1; 3 }');
    is scalar @{$l->{conds}}, 3, 'three columns' or diag explain $l->{conds};
    my $m = logic_of('sub { my ($x, @a) = @_; return 0 if $x > 1; for my $i (@a) { g($i) } return 2 if $x > 1; 3 }');
    is scalar @{$m->{conds}}, 1, 'a loop that does not touch $x keeps one column';
};

subtest 'an inner my is another variable' => sub {
    my $l = logic_of('sub { my ($x) = @_; return 1 if $x; { my $x = 2; return 2 if $x } 3 }');
    is scalar @{$l->{conds}}, 2, 'two columns';
};

subtest 'every kind of modification separates' => sub {
    my @mods = ('$x .= "a";', '$x ||= 1;', '$x++;', '++$x;', 'chomp $x;', '$x =~ s/a/b/;', '$x = 1;',
                'undef $x;', '($x) = (1);', '$x += 2;', 'my $r = \$x;', 'for $x (1) { }', 'chop($x);');
    for my $mod (@mods) {
        my $l = logic_of(qq{sub { my (\$x) = \@_; return 1 if \$x eq "a"; $mod return 2 if \$x eq "a"; 3 }});
        is scalar @{$l->{conds}}, 2, "$mod separates" or diag explain $l->{conds};
    }
    my @gmods = ('$main::g = 2;', 'local $main::g = 1;', 'for $main::g (1) { }', '$main::g .= "a";', '$main::g++;');
    for my $mod (@gmods) {
        my $l = logic_of(qq{sub { return 1 if \$main::g eq "a"; $mod return 2 if \$main::g eq "a"; 3 }});
        is scalar @{$l->{conds}}, 2, "$mod separates a package variable" or diag explain $l->{conds};
    }
    my $same = logic_of('sub { my ($x) = @_; return 1 if $x eq "a"; print $x; return 2 if $x eq "a"; 3 }');
    is scalar @{$same->{conds}}, 1, 'a read in between does not separate';
};

subtest 'branch arms' => sub {
    my $l = logic_of('sub { my ($x) = @_; if ($x) { $x = 0 } return 1 if $x; 2 }');
    is scalar @{$l->{conds}}, 2, 'an assignment in an arm separates what follows';
    my $m = logic_of('sub { my ($x) = @_; if ($x) { a() } else { return 1 if $x } return 2 if $x; 3 }');
    is scalar @{$m->{conds}}, 1, 'no modification: header, arm and after are one column';
    is scalar @{$m->{cond_sites}[0]}, 3, 'three sites';
};

done_testing;
