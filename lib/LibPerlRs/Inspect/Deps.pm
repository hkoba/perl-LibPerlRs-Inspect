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
        # also runs after a failed compilation: write what was seen so far
        my $data = eval { __PACKAGE__->collect }
            || { nodes => {}, edges => [], collect_error => "$@" };
        __PACKAGE__->write_json($_out, $data);
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
    my $program = $0;
    my @events  = @{ LibPerlRs::Inspect::require_events() };

    # CopFILE of a loaded file is its %INC value (or, when the load
    # failed and %INC no longer has it, the path the trace resolved);
    # map it back to the key. The main program maps to the key it was
    # reloaded as, which makes it the same node as that module.
    my (%key_of, %path_of);
    for my $key (keys %INC) {
        my $path = $INC{$key};
        next unless defined $path && !ref $path;
        $key_of{$path} = $key;
        $path_of{$key} = $path;
    }
    for my $ev (@events) {
        next unless $ev->{status} eq 'load';
        if (defined $ev->{path}) {
            $key_of{ $ev->{path} }   //= $ev->{name};
            $path_of{ $ev->{name} } //= $ev->{path};
        }
        $key_of{$program} //= $ev->{name}
            if defined $ev->{reload_of} && $ev->{reload_of} eq $program;
    }

    my (%nodes, @edges);
    my $node_of_file = sub {
        my ($file) = @_;
        my $key = $key_of{$file};
        my $id  = defined $key ? (_module_of($key) // $key) : $file;
        $nodes{$id} //= { file => $file, defined $key ? (inc => $key) : () };
        $nodes{$id}{program} = 1 if $file eq $program;
        $id;
    };
    my $node_of_key = sub {
        my ($key) = @_;
        my $id = _module_of($key) // $key;
        $nodes{$id} //= {
            (defined $path_of{$key} ? (file => $path_of{$key}) : ()),
            inc => $key,
        };
        $id;
    };

    my @begins = @{ LibPerlRs::Inspect::begins() };
    my (%use_at, %parent_at);
    for my $b (@begins) {
        next if $b->{kind} eq 'opaque' || !defined $b->{file};
        $use_at{ join "\0", $b->{file}, $b->{line} // 0, $b->{module} } = $b;
        # use parent / use base: the classes they load
        next unless $b->{module} =~ /^(?:parent|base)\z/ && $b->{args};
        my @args = @{ $b->{args} };
        next if grep { $_ eq '-norequire' } @args;
        $parent_at{ $b->{file} }{$_} //= $b for grep { !/^-/ } @args;
    }

    # 1. requires that ran, `use` / `no` told apart by their BEGIN. The
    # require base.pm / parent.pm do is attributed to the `use base` /
    # `use parent` of the file being compiled at that moment.
    my %traced;    # files whose loading was traced
    my %covered;   # parent BEGIN x class already turned into an edge
    for my $ev (@events) {
        next unless defined $ev->{from_file};
        my %edge = (
            to     => $node_of_key->($ev->{name}),
            inc    => $ev->{name},
            status => $ev->{status},
            stack  => $ev->{stack},
            (defined $ev->{reload_of} ? (reload_of => $ev->{reload_of}) : ()),
            ($ev->{snapshot} ? (snapshot => $ev->{snapshot}) : ()),
        );
        my $from_key = $key_of{ $ev->{from_file} } // '';
        my $user     = @{ $ev->{stack} } ? $path_of{ $ev->{stack}[-1] } : $program;
        my $pb       = $from_key =~ /^(?:parent|base)\.pm\z/ && defined $user
            && defined $ev->{module} && $parent_at{$user}{ $ev->{module} };
        if ($pb) {
            $covered{"$pb\0$ev->{module}"} = 1;
            push @edges, {
                %edge,
                from => $node_of_file->($user),
                kind => 'parent',
                file => $user,
                line => $pb->{line},
            };
        } else {
            my $b = $use_at{ join "\0", $ev->{from_file}, $ev->{from_line},
                $ev->{module} // '' };
            push @edges, {
                %edge,
                from => $node_of_file->($ev->{from_file}),
                kind => $b ? $b->{kind} : 'require',
                file => $ev->{from_file},
                line => $ev->{from_line},
                ($b && $b->{args} ? (args => $b->{args}) : ()),
            };
        }
        $traced{ $ev->{from_file} } = 1;
        $traced{ $path_of{ $ev->{name} } } = 1
            if $ev->{status} eq 'load' && defined $path_of{ $ev->{name} };
    }

    # 2. use parent / use base whose require was not seen (e.g. ran
    # before tracing started)
    for my $file (sort keys %parent_at) {
        for my $class (sort keys %{ $parent_at{$file} }) {
            my $b = $parent_at{$file}{$class};
            next if $covered{"$b\0$class"};
            (my $key = "$class.pm") =~ s{::}{/}g;
            push @edges, {
                from => $node_of_file->($file),
                to   => $node_of_key->($key),
                inc  => $key,
                kind => 'parent',
                file => $file,
                line => $b->{line},
            };
        }
    }

    # a load that did not complete (the file failed to compile, or the
    # compilation was aborted while it was in progress)
    for my $e (@edges) {
        next unless ($e->{status} // '') eq 'load';
        $e->{failed} = 1 unless defined $INC{ $e->{inc} };
    }

    # what a partly compiled target was missing when it was required
    _compare_snapshot($_) for grep { $_->{snapshot} } @edges;

    # 3. static requires inside the subs of traced files (lazy loading;
    # they may never run)
    require B;
    for my $pkg (LibPerlRs::Inspect::packages()) {
        no strict 'refs';
        my $stash = \%{"${pkg}::"};
        for my $name (sort keys %$stash) {
            next if $name =~ /::\z/;
            my $code = *{"${pkg}::$name"}{CODE} or next;
            # declared only (`use subs`, `sub foo;`, or left undefined by
            # a failed compile): no op tree
            next unless defined &$code;
            my $cv = B::svref_2object($code);
            next if $cv->XSUB;
            my $file = $cv->FILE;
            next unless defined $file && $traced{$file};
            # imported subs are listed under their own package
            next unless $cv->GV->STASH->NAME eq $pkg;
            my $sites = eval { LibPerlRs::Inspect::require_sites($code) } or next;
            for my $site (@$sites) {
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

    { program => $program, nodes => \%nodes, edges => \@edges };
}

# The state of a package now: the same fields as a require_events()
# snapshot
sub _package_state {
    my ($pkg) = @_;
    no strict 'refs';
    my $stash = \%{"${pkg}::"};
    require B;
    # defined subs, imported ones included; not method cache entries
    my @subs = grep {
        !/::\z/ && defined &{"${pkg}::$_"}
            && !(ref \$stash->{$_} eq 'GLOB' && B::svref_2object(\$stash->{$_})->CVGEN)
    } keys %$stash;
    return {
        subs      => [ sort @subs ],
        isa       => [ @{"${pkg}::ISA"} ],
        export    => [ @{"${pkg}::EXPORT"} ],
        export_ok => [ @{"${pkg}::EXPORT_OK"} ],
    };
}

# Add to an edge with a snapshot:
#   later   => what the target set up after that moment ({ subs, isa,
#              export, export_ok }, only non-empty lists)
#   missing => names the `use` asked for that the target could not
#              provide at that moment (Exporter-style import, or no import
#              at all); for a `use` without arguments, the @EXPORT names
#              that were not there yet
sub _compare_snapshot {
    my ($edge) = @_;
    my $snap = $edge->{snapshot};
    my $now  = _package_state($snap->{package});
    my %later;
    for my $field (qw(subs isa export export_ok)) {
        my %had = map { $_ => 1 } @{ $snap->{$field} };
        my @new = grep { !$had{$_} } @{ $now->{$field} };
        $later{$field} = \@new if @new;
    }
    $edge->{later} = \%later if %later;

    return unless $edge->{kind} eq 'use' && $edge->{args};
    my $import = $snap->{import} // '';
    my @args = @{ $edge->{args} };
    my @missing;
    if ($import eq '' || $import eq 'UNIVERSAL::import') {
        @missing = @args;
    } elsif ($import =~ /^Exporter(?:::Heavy)?::/) {
        if (!@args) {
            @missing = @{ $later{export} || [] };
        } else {
            my %ok = map { (my $n = $_) =~ s/^&//; ($n => 1) }
                @{ $snap->{export} }, @{ $snap->{export_ok} };
            @missing = grep {
                (my $n = $_) =~ s/^&//;
                !/^[:!\/]/ && !$ok{$n};
            } @args;
        }
    }
    $edge->{missing} = \@missing if @missing;
}

# How the file an edge requires came to be required: the edges that
# loaded each file on its loading stack, outermost first, then the edge
# itself. Works on the data returned by collect (or read back from JSON).
sub chain {
    my ($class, $deps, $edge) = @_;
    my @stack = @{ $edge->{stack} || [] };
    my @hops;
    for my $i (0 .. $#stack) {
        my ($hop) = grep {
            ($_->{status} // '') eq 'load'
                && $_->{inc} eq $stack[$i]
                && @{ $_->{stack} } == $i
                && join("\0", @{ $_->{stack} }) eq join("\0", @stack[0 .. $i - 1])
        } @{ $deps->{edges} };
        push @hops, $hop if $hop;
    }
    (@hops, $edge);
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

# When the program in perl arguments (as for run_perl) is a *.pm file
# under one of the -I directories, the module name it has there: such a
# module compiled as the main program can be loaded a second time.
sub program_module {
    my ($class, @args) = @_;
    my (@dirs, $program);
    while (@args) {
        my $arg = shift @args;
        if ($arg eq '--') {
            $program = shift @args;
            last;
        } elsif ($arg =~ /^-I(.*)\z/s) {
            push @dirs, length $1 ? $1 : shift @args;
        } elsif ($arg =~ /^-[eE]/) {
            return undef;
        } elsif ($arg !~ /^-/) {
            $program = $arg;
            last;
        }
    }
    return undef unless defined $program && $program =~ /\.pm\z/;
    require File::Spec;
    my $abs = File::Spec->rel2abs($program);
    for my $dir (@dirs) {
        my $rel = File::Spec->abs2rel($abs, File::Spec->rel2abs($dir));
        next if $rel =~ m{^\.\.} || File::Spec->file_name_is_absolute($rel);
        my $module = _module_of(join '/', File::Spec->splitdir($rel)) or next;
        return $module;
    }
    undef;
}

# Collect the dependencies of a program in a child perl:
#   run_perl('-Ilib', 'bin/app.pl')  /  run_perl('-Ilib', '-e', 'use My::App')
# Only compiles it (perl -c). Returns the decoded data; when compilation
# fails, the data collected up to that point with the child's STDERR in
# `compile_error`.
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
    return $class->read_json("$tmp") if $rc == 0;
    seek $err, 0, 0;
    my $msg = do { local $/; <$err> };
    die "perl -c failed:\n$msg" unless -s "$tmp";
    my $data = $class->read_json("$tmp");
    $data->{compile_error} = $msg;
    $data;
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
        program => 'bin/app.pl',          # $0
        nodes => {
            'My::App' => { file => 'lib/My/App.pm', inc => 'My/App.pm' },
            'bin/app.pl' => { file => 'bin/app.pl', program => 1 },
            ...
        },
        edges => [
            {
                from      => 'bin/app.pl',
                to        => 'My::App',
                inc       => 'My/App.pm',    # the %INC key of the target
                kind      => 'use',          # see below
                file      => 'bin/app.pl',   # where the dependency is written
                line      => 3,
                status    => 'load',         # edges from require_events only:
                stack     => [],             #   see LibPerlRs::Inspect
                reload_of => ...,            #   require_events
            },
            ...
        ],
    }

A node is named after its package for a C<*.pm> file, after its C<%INC>
key for other loaded files, and after its path for the main program,
unless the main program is reloaded as a module (C<reload_of>), in which
case both are the module's node (marked C<program>).

An edge whose target was still compiling or is reloaded also has
C<snapshot> (what the target package provided at that moment, see
C<require_events>), C<later> (what it set up afterwards: lists of
C<subs>, C<isa>, C<export>, C<export_ok>) and, for a C<use> with an
Exporter-style or no C<import>, C<missing>: the requested names that were
not exportable yet (for a C<use> without arguments, the C<@EXPORT> names
added later, i.e. what it silently failed to import). C<use> edges carry
the import C<args>.

A C<load> edge whose file did not finish loading (it failed to compile,
or the compilation was aborted while it was in progress) has C<failed>.

C<< LibPerlRs::Inspect::Deps->program_module(@perl_args) >> returns the
module name of the program in C<run_perl>-style arguments when it is a
F<*.pm> file under one of the C<-I> directories (undef otherwise): such a
module compiled as the main program can be loaded a second time.

C<< LibPerlRs::Inspect::Deps->chain($deps, $edge) >> returns how the
target of an edge with a C<stack> came to be required: the edges that
loaded each file on the stack, outermost first, then the edge itself.
The first hop out of the target in that list shows the line where the
target's own compilation was suspended.

Edge kinds:

=over 4

=item use / no

A C<use> / C<no> statement that ran during compilation.

=item require

Any other C<require> that ran (in a BEGIN block, at the top level of a
module, from a C<use parent> or C<base> on behalf of the user, ...).

=item parent

A class loaded by C<use parent> / C<use base> (no edge with
C<-norequire>). The C<require> F<parent.pm> / F<base.pm> perform is
attributed to the C<use> line, so the edge also carries its C<status>.

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
