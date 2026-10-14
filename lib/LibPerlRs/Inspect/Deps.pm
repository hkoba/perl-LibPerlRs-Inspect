package LibPerlRs::Inspect::Deps;
use strict;
use warnings;

use LibPerlRs::Inspect;

our $VERSION = '0.01';

# Output target given by -MLibPerlRs::Inspect::Deps=FILE (undef: none)
my $_out;

sub import {
    my ($class, $out) = @_;
    return unless defined $out;
    $_out = $out;
    $class->start;
}

# Runs after the CHECK blocks of everything compiled later (CHECK blocks
# run in reverse order of definition), i.e. once the whole program has
# been compiled. Also runs under `perl -c`.
CHECK {
    if (defined $_out) {
        __PACKAGE__->stop;
        __PACKAGE__->write_json($_out, __PACKAGE__->collect);
    }
}

sub start {
    LibPerlRs::Inspect::begin_capture(1);
    LibPerlRs::Inspect::require_trace_start();
}

sub stop {
    LibPerlRs::Inspect::begin_capture(0);
    LibPerlRs::Inspect::require_trace_stop();
}

# Package name of a *.pm %INC key
sub _module_of {
    my ($key) = @_;
    return undef unless defined $key && $key =~ m{^(\w+(?:/\w+)*)\.pm\z};
    (my $m = $1) =~ s{/}{::}g;
    $m;
}

sub collect {
    my ($class) = @_;

    # CopFILE of a loaded file is its %INC value; map it back to the key
    my %key_of;
    for my $key (keys %INC) {
        my $path = $INC{$key};
        $key_of{$path} = $key if defined $path && !ref $path;
    }
    my (%nodes, @edges);
    my $node_of_file = sub {
        my ($file) = @_;
        my $key = $key_of{$file};
        my $id  = defined $key ? (_module_of($key) // $key) : $file;
        $nodes{$id} //= { file => $file, defined $key ? (inc => $key) : () };
        $id;
    };
    my $node_of_key = sub {
        my ($key) = @_;
        my $id = _module_of($key) // $key;
        $nodes{$id} //= {
            (defined $INC{$key} && !ref $INC{$key} ? (file => $INC{$key}) : ()),
            inc => $key,
        };
        $id;
    };

    my @begins = @{ LibPerlRs::Inspect::begins() };
    my %use_at;
    for my $b (@begins) {
        next if $b->{kind} eq 'opaque' || !defined $b->{file};
        $use_at{ join "\0", $b->{file}, $b->{line} // 0, $b->{module} } = $b;
    }

    # 1. requires that ran, `use` / `no` told apart by their BEGIN
    my %traced;    # files whose loading was traced
    for my $ev (@{ LibPerlRs::Inspect::require_events() }) {
        next unless defined $ev->{from_file};
        my $b = $use_at{ join "\0", $ev->{from_file}, $ev->{from_line},
            $ev->{module} // '' };
        push @edges, {
            from   => $node_of_file->($ev->{from_file}),
            to     => $node_of_key->($ev->{name}),
            kind   => $b ? $b->{kind} : 'require',
            file   => $ev->{from_file},
            line   => $ev->{from_line},
            status => $ev->{status},
        };
        $traced{ $ev->{from_file} } = 1;
        $traced{ $INC{ $ev->{name} } } = 1
            if $ev->{status} eq 'load' && defined $INC{ $ev->{name} } && !ref $INC{ $ev->{name} };
    }

    # 2. use parent / use base: the classes they load
    for my $b (@begins) {
        next unless ($b->{module} // '') =~ /^(?:parent|base)\z/ && $b->{args};
        my @args = @{ $b->{args} };
        next if grep { $_ eq '-norequire' } @args;
        for my $class (grep { !/^-/ } @args) {
            (my $key = "$class.pm") =~ s{::}{/}g;
            push @edges, {
                from => $node_of_file->($b->{file}),
                to   => $node_of_key->($key),
                kind => 'parent',
                file => $b->{file},
                line => $b->{line},
            };
        }
    }

    # 3. static requires inside the subs of traced files (lazy loading;
    # they may never run)
    require B;
    for my $pkg (LibPerlRs::Inspect::packages()) {
        no strict 'refs';
        my $stash = \%{"${pkg}::"};
        for my $name (sort keys %$stash) {
            next if $name =~ /::\z/;
            my $code = *{"${pkg}::$name"}{CODE} or next;
            my $cv = B::svref_2object($code);
            next if $cv->XSUB;
            my $file = $cv->FILE;
            next unless defined $file && $traced{$file};
            # imported subs are listed under their own package
            next unless $cv->GV->STASH->NAME eq $pkg;
            for my $site (@{ LibPerlRs::Inspect::require_sites($code) }) {
                next unless defined $site->{file};
                push @edges, {
                    from => $node_of_file->($file),
                    to   => $node_of_key->($site->{file}),
                    kind => 'require_static',
                    file => $file,
                    line => $site->{line},
                    sub  => "${pkg}::$name",
                };
            }
        }
    }

    { nodes => \%nodes, edges => \@edges };
}

sub write_json {
    my ($class, $out, $data) = @_;
    require JSON::PP;
    my $json = JSON::PP->new->canonical->pretty->encode($data);
    if ($out eq '-') {
        print $json;
        return;
    }
    open my $fh, '>', $out or die "Can't write $out: $!";
    print $fh $json;
    close $fh or die "Can't write $out: $!";
}

sub read_json {
    my ($class, $file) = @_;
    require JSON::PP;
    open my $fh, '<', $file or die "Can't read $file: $!";
    JSON::PP->new->decode(do { local $/; <$fh> });
}

# Collect the dependencies of a program in a child perl:
#   run_perl('-Ilib', 'bin/app.pl')  /  run_perl('-Ilib', '-e', 'use My::App')
# Only compiles it (perl -c). Returns the decoded data.
sub run_perl {
    my ($class, @args) = @_;
    require File::Temp;
    require Config;
    my $tmp = File::Temp->new(SUFFIX => '.json');
    close $tmp;
    # let the child find this module the way we did
    local $ENV{PERL5LIB} = join $Config::Config{path_sep}, grep { !ref } @INC;
    my @cmd = ($^X, "-MLibPerlRs::Inspect::Deps=$tmp", '-c', @args);
    # perl -c reports "syntax OK" on STDERR: keep it for error messages only
    my $err = File::Temp->new;
    open my $saved, '>&', \*STDERR or die "Can't dup STDERR: $!";
    open STDERR, '>&', $err or die "Can't redirect STDERR: $!";
    my $rc = system @cmd;
    open STDERR, '>&', $saved or die "Can't restore STDERR: $!";
    if ($rc != 0) {
        seek $err, 0, 0;
        die "perl -c failed:\n", do { local $/; <$err> };
    }
    $class->read_json("$tmp");
}

1;
__END__

=head1 NAME

LibPerlRs::Inspect::Deps - collect the module dependencies of a program

=head1 SYNOPSIS

    # compile only, write the dependencies as JSON when compilation ends
    perl -MLibPerlRs::Inspect::Deps=deps.json -c -Ilib bin/app.pl

    # in-process
    use LibPerlRs::Inspect::Deps;
    LibPerlRs::Inspect::Deps->start;
    require My::App;
    LibPerlRs::Inspect::Deps->stop;
    my $deps = LibPerlRs::Inspect::Deps->collect;

    # in a child perl (perl -c), from a tool
    my $deps = LibPerlRs::Inspect::Deps->run_perl('-Ilib', 'bin/app.pl');

    # the JSON written by the first form
    my $deps = LibPerlRs::Inspect::Deps->read_json('deps.json');

=head1 DESCRIPTION

A thin layer over the dependency primitives of L<LibPerlRs::Inspect>
(C<begins>, C<require_events>, C<require_sites>) that turns them into a
single edge list. It only reports facts; graph algorithms are left to
the tools built on it (see C<examples/module_cycles.pl> and
C<examples/module_graph.pl>).

The result of C<collect> is:

    {
        nodes => {
            'My::App' => { file => 'lib/My/App.pm', inc => 'My/App.pm' },
            'bin/app.pl' => { file => 'bin/app.pl' },
            ...
        },
        edges => [
            {
                from   => 'bin/app.pl',
                to     => 'My::App',
                kind   => 'use',          # see below
                file   => 'bin/app.pl',   # where the dependency is written
                line   => 3,
                status => 'load',         # require kinds only
            },
            ...
        ],
    }

A node is named after its package for a C<*.pm> file, after its C<%INC>
key for other loaded files, and after its path for the main program.

Edge kinds:

=over 4

=item use / no

A C<use> / C<no> statement that ran during compilation.

=item require

Any other C<require> that ran (in a BEGIN block, at the top level of a
module, from a C<use parent> or C<base> on behalf of the user, ...).

=item parent

A class loaded by C<use parent> / C<use base> (no edge with
C<-norequire>). The C<require> those modules perform is also listed as a
C<require> edge from F<parent.pm> / F<base.pm>.

=item require_static

A constant C<require> inside a sub of a traced file (lazy loading that
may never run). C<sub> names the sub.

=back

C<status> (from C<require_events>) is C<load>, C<cached>, C<failed>, or
C<in_progress>: the edge closes a cycle while its target is still being
compiled, so the target is only partly defined at that point.

Modules loaded before tracing starts (this module, L<LibPerlRs::Inspect>
and their dependencies such as L<strict>, L<warnings>, L<JSON::PP>) are
not traced themselves; edges to them are still recorded.

=cut
