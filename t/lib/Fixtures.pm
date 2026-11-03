package Fixtures;
use strict;
use warnings;

# Fixtures shared by all tests (name => source string to eval).
# Matching the primary use case (analyzing the anonymous sub returned by
# eval), they are always compiled via eval.
our @FIXTURES = (
    [empty        => 'sub {}'],
    [shift_style  => 'sub { my $self = shift; my $n = shift; $self + $n }'],
    [unpack_args  => 'sub { my ($x, $y) = @_; $x * $y }'],
    [signature    => 'use v5.36; sub ($x, $y = 5, @rest) { $x + $y }'],
    [branches     => 'sub { my ($v) = @_; return unless $v; if ($v > 10) { "big" } elsif ($v > 5) { "mid" } else { "small" } }'],
    [ternary      => 'sub { my ($x) = @_; $x ? "yes" : "no" }'],
    [foreach_loop => 'sub { my $sum = 0; $sum += $_ for @_; $sum }'],
    [eval_die     => 'sub { my ($x) = @_; eval { die "boom\n" if $x }; $@ }'],
    [method_call  => 'sub { my ($obj) = @_; $obj->frobnicate(1, 2) }'],
    [closure      => 'sub { my $c = 0; sub { $c++ } }'],
    [deref_chain  => 'sub { my ($x, $i) = @_; $x->[0]{k} + $x->{h}[$i] }'],
    [bool_return  => 'sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }'],
    [compound_guard => 'sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }'],
    [print_chain  => q{sub { my ($x, $y, $z) = @_; if ($x < 0) { print "x < 0" } elsif ($y < 0) { print "y < 0" } elsif ($z < 0) { print "z < 0" } else { print "x,y,z >= 0" } }}],
    [interp       => 'sub { my ($x) = @_; "a$x b" }'],
    [arm_stmts    => 'sub { my ($x) = @_; if ($x) { log_it("a"); note(); return 1 } "z" }'],
    # Multi-line fixtures for range analysis (logic::analyze_logic_range)
    [range_guard  => "sub {\n my (\$x, \$y) = \@_;\n return 0 unless \$x;\n log_it(\"go\");\n if (\$y) {\n  note();\n  return 1;\n }\n done();\n return 2;\n}"],
    [range_nested_if => "sub {\n my (\$a, \$b) = \@_;\n if (\$a) {\n  log_it(\"a\");\n  die \"bad\" if \$b;\n  note();\n } else {\n  done();\n }\n finish();\n}"],
    # Atom identity (logic_canon): the same condition spelled twice
    [ident_defined_assign => "sub {\n my (\$x) = \@_;\n return 0 unless defined(my \$y = f(\$x));\n return 1 if defined(\$y);\n 2;\n}"],
    [ident_range_split => "sub {\n my (\$s) = \@_;\n return \"short\" if length(\$s) < 8;\n return \"ok\" if length(\$s) >= 8;\n \"x\";\n}"],
    [ident_loop_assign => "sub {\n my (\$x, \@a) = \@_;\n return 0 if \$x > 1;\n for my \$i (\@a) {\n  \$x = \$i;\n  return 1 if \$x > 1;\n }\n return 2 if \$x > 1;\n 3;\n}"],
    [ident_elem_assign => "sub {\n my (\%h) = \@_;\n return 1 if \$h{k};\n \$h{k} = 1;\n return 2 if \$h{k};\n 3;\n}"],
    [range_loop_next => "sub {\n my (\@items) = \@_;\n my \$n = 0;\n for my \$it (\@items) {\n  next unless \$it;\n  last if \$it eq \"stop\";\n  \$n++;\n }\n return \$n;\n}"],
    # Seven independent guards fan out past MAX_PATH_SET (64) routes
    [trace_fanout => 'sub { my ($a, $b, $c, $d, $e, $f, $g) = @_; s1() if $a; s2() if $b; s3() if $c; s4() if $d; s5() if $e; s6() if $f; s7() if $g; return done() }'],
    # Full trace (logic full_trace): prefix, loop body, eval body
    [range_trace => "sub {\n my (\$x, \@items) = \@_;\n setup();\n return 0 unless \$x;\n my \$n = 0;\n for my \$it (\@items) {\n  next unless \$it;\n  tally(\$it);\n  last if \$it eq \"stop\";\n  \$n++;\n }\n eval { risky() };\n return \$n;\n}"],
    # Argument extraction over the whole body (passes::symexec / argspec v2)
    [args_shift_loop => "sub {\n my \$sum = shift;\n foreach my \$val (\@_) {\n  \$sum += \$val;\n }\n \$sum\n}"],
    [args_opts_hash => "sub {\n my (\$self, \%args) = \@_;\n my \$x = \$args{foo} // 'dflt';\n my \$y = \$args{bar};\n return \$x . \$y . (exists \$args{baz} ? 1 : 0);\n}"],
    [args_hash_defaults => "sub {\n my \%args = (color => 'red', \@_);\n \$args{color} . \$args{size}\n}"],
    [args_hashref => "sub {\n my (\$self, \$opts) = \@_;\n \$opts->{verbose} // 0\n}"],
    [args_pop_splice => "sub {\n my \$last = pop;\n my \$first = splice(\@_, 0, 1);\n my \$n = \@_;\n [\$first, \$last, \$n]\n}"],
    [args_defaults => "sub {\n my \$a = shift // 'da';\n my \$b = shift || 'db';\n my \$c = \@_ ? shift : 'dc';\n my \$d = shift;\n \$d //= 'dd';\n my \$e = shift;\n \$e = 'de' unless defined \$e;\n [\$a, \$b, \$c, \$d, \$e]\n}"],
    [args_forward => "sub {\n my \$self = shift;\n \$self->SUPER::new(\@_);\n other(\@_);\n &other2;\n goto &other3;\n}"],
    [args_checks => "use Carp; sub {\n croak 'usage' unless \@_ == 2;\n die 'too many' if \@_ > 5;\n my (\$x, \$y) = \@_;\n \$x + \$y\n}"],
    [args_conditional => "sub {\n my \$x = shift;\n my \$opt;\n if (ref \$x eq 'HASH') {\n  \$opt = shift;\n }\n my \$z = \$_[0];\n [\$x, \$opt, \$z]\n}"],
    [args_elem_after_shift => "sub {\n my \$first = shift;\n return \$_[0] + \$_[1];\n}"],
    # Call sites (passes::callsites)
    [calls_mixed => "sub {\n my (\$o, \@list) = \@_;\n my (\%h, \$cb);\n foo(1, 2);\n Bar::baz(\$o, \@list);\n \$o->meth(a => 1, b => 2);\n Klass->new(\%h);\n &foo;\n foo(\@_);\n &\$cb(3);\n}"],
    # Argument helpers inlined by passes::symexec (bodies of
    # MOP4Import::Opts::m4i_opts / m4i_opts_alike / m4i_args, as local subs)
    [args_helper_m4i_opts => "sub Opts () {'MOP4Import::Opts'}\nsub m4i_opts {\n my (\$arg) = \@_;\n if (not ref \$arg) {\n  Opts->new(caller => \$arg);\n } elsif (UNIVERSAL::isa(\$arg, Opts)) {\n  \$arg\n } elsif (ref \$arg eq 'ARRAY') {\n  Opts->new(caller => \$arg);\n } elsif (ref \$arg eq 'HASH' and m4i_opts_alike(\$arg)) {\n  \$arg;\n } else {\n  Carp::croak('Unknown argument!');\n }\n}\n\\&m4i_opts"],
    [args_helper_m4i_opts_alike => "sub m4i_opts_alike {\n my (\$arg) = \@_;\n not grep {not exists \$arg->{\$_}} qw(caller callpack filename line)\n}\n\\&m4i_opts_alike"],
    [args_helper_m4i_args => "sub m4i_args {\n (\$_[0], m4i_opts(\$_[1]), \@_[2..\$#_]);\n}\n\\&m4i_args"],
    [args_helper_m4i => "sub {\n my (\$pack, \$opts, \@rest) = m4i_args(\@_);\n \$opts->{destpkg} . \$pack . scalar(\@rest)\n}"],
);

sub compile {
    my ($name, $src) = @_;
    my $code = eval $src;
    die "fixture $name failed to compile: $@" if $@ or ref $code ne 'CODE';
    $code;
}

1;
