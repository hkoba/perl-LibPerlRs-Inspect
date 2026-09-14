use strict;
use warnings;
use Test::More;
use LibPerlRs::Inspect;

# The mini deparser is exercised through the implicit return value of a
# sub whose last statement is the expression under test.
sub implicit_of {
    my ($src) = @_;
    my $code = eval $src;
    die "fixture failed: $@" if $@ or ref $code ne 'CODE';
    my ($imp) = grep { $_->{kind} eq 'implicit' }
        @{LibPerlRs::Inspect::analyze($code)->{returns}{returns}};
    $imp ? join(', ', @{$imp->{exprs}}) : undef;
}

my @cases = (
    # [description, source, expected rendering]
    ['print LIST',        'sub { print "x" }',                              'print "x"'],
    ['print FH LIST',     'sub { print STDERR "x" }',                       'print STDERR "x"'],
    ['print {$fh} LIST',  'sub { my ($fh, $y) = @_; print {$fh} "x", $y }', 'print {$fh} "x", $y'],
    ['print $fh LIST',    'sub { my ($fh, $y) = @_; print $fh $y }',        'print {$fh} $y'],
    ['say',               'use feature "say"; sub { say "x" }',             'say "x"'],
    ['printf',            'sub { my ($x) = @_; printf "%d", $x }',          'printf "%d", $x'],
    ['warn (generic)',    'sub { warn "w" }',                               'warn("w")'],
    ['die (generic)',     'sub { my ($x) = @_; $x || die() }',              '$x || die()'],
    ['interpolation',     'sub { my ($x) = @_; "a$x b" }',                  '"a$x b"'],
    ['concat with consts','sub { my ($x) = @_; "prefix: " . $x . "\n" }',   '"prefix: " . $x . "\n"'],
    ['append',            'sub { my ($x, $y) = @_; $x .= $y }',             '$x .= $y'],
    ['my $s = "..."',     'sub { my ($x) = @_; my $s = "a$x" }',            'my $s = "a$x"'],
    ['concat with call',  'sub { my ($x) = @_; "a" . f($x) . "b" }',        '"a" . f($x) . "b"'],
    ['stringify',         'sub { my ($x) = @_; "$x" }',                     '"$x"'],
    ['match',             'sub { my ($x) = @_; $x =~ /re/ }',               '$x =~ m/.../'],
    ['match on $_',       'sub { /re/ }',                                   'm/.../'],
    ['subst',             'sub { my ($x) = @_; $x =~ s/a/b/ }',             '$x =~ s/.../b/'],
    ['subst with var',    'sub { my ($x, $y) = @_; $x =~ s/a/$y/ }',        '$x =~ s/.../$y/'],
    ['negated match',     'sub { my ($x) = @_; $x !~ /a/ }',                '$x !~ m/.../'],
    ['length',            'sub { my ($x) = @_; length $x }',                'length($x)'],
    ['ref eq',            'sub { my ($x) = @_; ref($x) eq "HASH" }',        'ref($x) eq "HASH"'],
    ['substr_left',       'sub { my ($s) = @_; substr($s, 0, 3) }',         'substr($s, 0, 3)'],
    ['substr',            'sub { my ($s) = @_; substr($s, 1, 2) }',         'substr($s, 1, 2)'],
    ['-d',                'sub { my ($d) = @_; -d $d }',                    '-d $d'],
    ['-l',                'sub { my ($d) = @_; -l $d }',                    '-l $d'],
    ['chdir',             'sub { my ($d) = @_; chdir $d }',                 'chdir($d)'],
    ['opendir',           'sub { my $dh; opendir $dh, "." }',               'opendir($dh, ".")'],
    ['grep BLOCK',        'sub { my @v = @_; grep { $_ > 1 } @v }',         'grep { $_ > 1 } @v'],
    ['grep EXPR',         'sub { my @v = @_; grep /x/, @v }',               'grep m/.../, @v'],
    ['map BLOCK',         'sub { my @v = @_; map { $_ * 2 } @v }',          'map { $_ * 2 } @v'],
    ['anonhash',          'sub { { a => 1 } }',                             '{"a" => 1}'],
    ['anonlist',          'sub { [1, 2] }',                                 '[1, 2]'],
    ['empty hash',        'sub { +{} }',                                    '{}'],
    ['empty list',        'sub { [] }',                                     '[]'],
    ['my (...) = @_',     'sub { my ($p, $q) = @_ }',                       'my ($p, $q) = @_'],
    ['my @a = (...)',     'sub { my @a = (1, 2) }',                         'my @a = (1, 2)'],
    ['list assign',       'sub { my ($p, $q) = @_; ($p, $q) = ($q, $p) }',  '($p, $q) = ($q, $p)'],
    ['postinc',           'sub { my ($i) = @_; $i++ }',                     '$i++'],
    ['preinc',            'sub { my ($i) = @_; ++$i }',                     '++$i'],
    ['postdec',           'sub { my ($i) = @_; $i-- }',                     '$i--'],
    ['join',              'sub { my @v = @_; join(",", @v) }',              'join(",", @v)'],
    ['sprintf',           'sub { my ($x) = @_; sprintf("%d", $x) }',        'sprintf("%d", $x)'],
    ['push',              'sub { my @v = @_; push @v, 1 }',                 'push(@v, 1)'],
    ['scalar',            'sub { my @v = @_; scalar(@v) }',                 'scalar(@v)'],
    ['keys',              'sub { my %h = @_; keys %h }',                    'keys(%h)'],
    ['exists',            'sub { my %h = @_; exists $h{k} }',               'exists $h{k}'],
    ['delete',            'sub { my %h = @_; delete $h{k} }',               'delete $h{k}'],
    ['SUPER::',           'sub { my $s = shift; $s->SUPER::new(@_) }',      '$s->SUPER::new(@_)'],
    ['method',            'sub { my $s = shift; $s->frob(1) }',             '$s->frob(1)'],
    ['code ref call',     'sub { my ($c) = @_; $c->(1) }',                  '$c->(1)'],
    ['do BLOCK',          'sub { do { 1 } }',                               'do { 1 }'],
    ['ternary with do',   'sub { my ($x, $y) = @_; $x ? do { f(); 1 } : $y }', '$x ? do { ...; 1 } : $y'],
    ['if/else',           'sub { my ($x) = @_; if ($x) { 1 } else { 2 } }', 'if ($x) { 1 } else { 2 }'],
    ['if without else',   'sub { my ($x) = @_; if ($x) { f() } }',          'if ($x) { f() }'],
    ['unless',            'sub { my ($x) = @_; unless ($x) { f() } }',      'unless ($x) { f() }'],
    ['multi-stmt block',  'sub { my ($x) = @_; if ($x) { f(); g(); 1 } else { 2 } }', 'if ($x) { ...; 1 } else { 2 }'],
    ['${$x}',             'sub { my ($x) = @_; ${$x} }',                    '${$x}'],
    ['@{$x}',             'sub { my ($x) = @_; @{$x} }',                    '@{$x}'],
    ['\\$x',              'sub { my ($x) = @_; \$x }',                      '\$x'],
    ['\\@v',              'sub { my @v = @_; \@v }',                        '\@v'],
    ['stub',              'sub { () }',                                     '()'],
    ['eval BLOCK',        'sub { eval { 1 } }',                             'eval { 1 }'],
    ['lexical aelem',     'sub { my @v = @_; $v[1] }',                      '$v[1]'],
    ['package aelem',     'sub { $ARGV[0] }',                               '$ARGV[0]'],
    ['stack multideref',  'sub { f()->{x} }',                               'f()->{x}'],
    ['generic listop',    'sub { my ($x) = @_; sort @{$x} }',               'sort(@{$x})'],
    ['generic leaf',      'sub { time }',                                   '<time>'],
);

for my $c (@cases) {
    my ($name, $src, $want) = @$c;
    is implicit_of($src), $want, $name;
}

subtest 'print chain: every arm has a distinct outcome' => sub {
    my $code = eval q{sub { my ($x, $y, $z) = @_; if ($x < 0) { print "x < 0" } elsif ($y < 0) { print "y < 0" } elsif ($z < 0) { print "z < 0" } else { print "x,y,z >= 0" } }};
    my $r = LibPerlRs::Inspect::analyze($code);
    my $l = $r->{logic};
    is_deeply $l->{conds}, ['$x < 0', '$y < 0', '$z < 0'], 'conds';
    is_deeply [map { $_->{exprs}[0] } @{$l->{paths}}],
        ['print "x < 0"', 'print "y < 0"', 'print "z < 0"', 'print "x,y,z >= 0"'],
        'four distinct outcomes';
    my %by_inputs = map { join('', map { $_ ? 1 : 0 } @{$_->{inputs}}) => $_->{path} } @{$l->{table}};
    is $by_inputs{'000'}, 3, 'all >= 0';
    is $by_inputs{'001'}, 2, 'z < 0';
    is $by_inputs{'010'}, 1, 'y < 0';
    is $by_inputs{'011'}, 1, 'y < 0 dominates z';
    is $by_inputs{'111'}, 0, 'x < 0 dominates';
    my ($imp) = grep { $_->{kind} eq 'implicit' } @{$r->{returns}{returns}};
    is $imp->{exprs}[0],
        'if ($x < 0) { print "x < 0" } elsif ($y < 0) { print "y < 0" } elsif ($z < 0) { print "z < 0" } else { print "x,y,z >= 0" }',
        'the chain renders in statement form';
};

done_testing;
