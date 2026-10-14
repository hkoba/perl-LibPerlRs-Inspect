package LibPerlRs::Inspect;
use strict;
use warnings;

our $VERSION = '0.01';

require XSLoader;
XSLoader::load('LibPerlRs::Inspect', $VERSION);

require JSON::PP;

# Strings returned from the XS side carry the UTF8 flag (they are not byte
# strings), so use string-mode decode rather than decode_json
my $_json = JSON::PP->new;

sub op_names {
    my ($code) = @_;
    $_json->decode(op_names_json($code));
}

sub capture {
    my ($code) = @_;
    $_json->decode(capture_json($code));
}

# analyze() returns a native hashref directly from the XS side (no JSON round trip).
# Use analyze_json() if you want the JSON string.

# The `use` statement a BEGIN block was compiled from, as text: undef for
# a BEGIN that is not of use form, "" for a hint-bit pragma (strict,
# warnings, feature, ...). Loads B::Deparse on first use, which itself
# runs BEGIN blocks and loads modules: call it after collecting begins().
my $_deparse;
sub begin_stmt {
    my ($code) = @_;
    $_deparse ||= do {
        require B;
        require B::Deparse;
        B::Deparse->new;
    };
    my $stmt = $_deparse->begin_is_use(B::svref_2object($code));
    $stmt =~ s/\n\z// if defined $stmt;
    $stmt;
}

1;
__END__

=head1 NAME

LibPerlRs::Inspect - analyze a subroutine reference at the OP tree level

=head1 SYNOPSIS

    use LibPerlRs::Inspect;

    my $sub = eval 'sub { my ($x) = @_; $x + 1 }';
    my $names = LibPerlRs::Inspect::op_names($sub);   # execution-order op names

=head1 DESCRIPTION

Rust-powered OP tree analysis for subroutine references, including
anonymous subs returned by C<eval>. See the project README.

=head1 DEPENDENCY PRIMITIVES

Building blocks for tools that inspect module dependencies (dependency
graphs, cycle detection, class hierarchies). They report facts about the
running interpreter; building a graph out of them is left to the caller.
See C<examples/> for complete tools.

=over 4

=item begin_capture($on)

Turn BEGIN capture (C<PL_savebegin>) on or off. BEGIN blocks that run
while it is on, including the ones C<use> compiles into, are kept instead
of being freed. Turn it on before loading the code to inspect:

    BEGIN { LibPerlRs::Inspect::begin_capture(1) }
    require My::App;

=item begins()

Arrayref of the BEGIN blocks saved while capture was on, in the order
they ran, from every file (modules loaded transitively included):

    {
        file     => 'lib/My/App.pm',  # CvFILE of the BEGIN
        line     => 3,                # its first statement
        code     => CODE,             # the BEGIN itself
        kind     => 'use',            # use / no / opaque
        module   => 'File::Basename', # use / no only
        version  => '2',              # use Foo VERSION
        args     => ['basename'],     # import / unimport arguments
        requires => [ ... ],          # require_sites() of the BEGIN
    }

C<args> is missing when C<import> is not called (C<use Foo ()>) or an
argument is not a constant. An C<opaque> BEGIN is a hand-written
C<BEGIN { ... }>; its C<requires> still lists any C<require> in it.
A C<use> that closes a dependency cycle is listed too, although the
C<require> it performs is a no-op (the file is already in C<%INC>).

=item require_sites($coderef)

Arrayref of the C<require> ops of a sub, each
C<< { line, file, module } >> (C<file> is the C<%INC> key, C<module> is
set for C<*.pm> files), C<< { line, version } >> for C<require VERSION>,
or C<< { line, dynamic => 1 } >> for C<require $var>. A C<require> inside
a string eval is not visible.

=item begin_stmt($code)

The C<use> statement text of a BEGIN (C<B::Deparse::begin_is_use>): undef
for an opaque BEGIN, C<""> for a hint-bit pragma such as C<strict>. Loads
B::Deparse on first call.

=item packages()

List of every package reachable from C<main::>.

=item inheritance($package)

C<< { isa => [...], linear_isa => [...], mro => 'dfs' | 'c3' } >> for a
package with a non-empty C<@ISA>, otherwise undef. C<linear_isa> is the
method resolution order, the package itself first.

=back

Known limits: C<#line> directives change the file a BEGIN is attributed
to; code compiled by string eval, C<require $var> and C<use if> are not
resolved statically.

=cut
