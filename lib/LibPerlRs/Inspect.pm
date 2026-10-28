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

# Truth table of the statements on source lines $start..$end of $code,
# treated as one block: routes leaving the block at its end are
# "fallthrough" outcomes. Dies when no statement touches the range.
sub analyze_logic_range {
    my ($code, $start, $end) = @_;
    $end = $start unless defined $end;
    $_json->decode(analyze_logic_range_json($code, $start, $end));
}

# The logic (truth table) of $code with options:
#   alias_modifies => 1   treat f($x) / $x->m (aliasing through @_) as a
#                         modification of $x, so conditions on $x before
#                         and after the call are kept apart
#   identity => 'strict'  (default) two sites are one condition when they
#                         test the same value: a reassignment of $x in
#                         between keeps them apart, and `defined($x = f())`
#                         is the same as a later `defined($x)`
#   identity => 'expr'    reassignments are ignored; an assignment is the
#                         expression it is written as, so
#                         `defined($x = a())` and `defined($x = b())` are
#                         two conditions but `$x >= 3` and a later `$x < 3`
#                         are one
#   identity => 'var'     reassignments are ignored and an assignment stands
#                         for its variable: `defined($x = a())`,
#                         `defined($x = b())` and `defined($x)` are one
#                         condition, shown as `defined($x)`
sub analyze_logic_opts {
    my ($code, $opts) = @_;
    $opts ||= {};
    $_json->decode(analyze_logic_opts_json($code, $opts->{alias_modifies} ? 1 : 0,
                                           $opts->{identity} // 'strict'));
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

=item require_trace_start()

Start recording every C<require> (and so every C<use>) executed from now
on, clearing the events recorded so far. Only C<require> ops compiled
after this call are seen, so start before loading the code to inspect
(C<BEGIN { LibPerlRs::Inspect::require_trace_start() }>, or from a module
loaded with C<-M>).

=item require_trace_stop()

Stop recording. The events are kept.

=item require_events()

Arrayref of the recorded requires, in the order they ran:

    {
        name      => 'My/Util.pm',      # the %INC key
        module    => 'My::Util',        # for *.pm files
        from_file => 'lib/My/App.pm',   # where the require / use is
        from_line => 4,
        status    => 'load',            # see below
        stack     => ['My/App.pm'],     # files being loaded, outermost first
        path      => 'lib/My/Util.pm',  # load only: the file to be read
        reload_of => 'bin/app.pl',      # load only, see below
        snapshot  => { ... },           # in_progress / reload, see below
    }

C<status> is C<load> (not yet loaded: loading starts), C<cached> (already
loaded, nothing happens), C<in_progress> (in C<%INC> but still being
loaded: the require closes a compile-time dependency cycle, and the target
module is only partly defined at this point) or C<failed> (an earlier load
failed). C<require VERSION> is not recorded, nor is C<do FILE>.

For a C<load>, C<path> is the file perl will read, found by searching
C<@INC> again (C<@INC> hooks are skipped). C<reload_of> is set when that
file has been compiled already: to C<$0> when it is the main program (a
module compiled as the main program is not in C<%INC> under its module
name, so a dependency cycle leading back to it compiles it a second
time), or to the other C<%INC> key it was loaded under. Files are
compared by canonical path.

For C<in_progress> and reloads, C<snapshot> records what the target
package (named after the C<*.pm> file) provided at that moment, which is
what the requiring side gets to see:

    {
        package   => 'My::Util',
        subs      => ['import'],          # defined so far, imported ones too
        import    => 'Exporter::import',  # what ->import resolves to, or undef
        isa       => [],
        export    => [],                  # @EXPORT
        export_ok => [],                  # @EXPORT_OK
    }

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
