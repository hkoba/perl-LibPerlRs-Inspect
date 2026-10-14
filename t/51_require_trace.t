use strict;
use warnings;
use Test::More;
use FindBin;
use lib "$FindBin::Bin/lib-deps";
use LibPerlRs::Inspect;

# Trace the loading of Deps::A, which uses Deps::B, which uses Deps::A
# back while Deps::A is still compiling.
BEGIN { LibPerlRs::Inspect::require_trace_start() }
use Deps::A;

my $here = __FILE__;
sub events {
    [ map { { %$_{qw(name status stack)} } }
        grep { $_->{name} =~ /^Deps\// } @{ LibPerlRs::Inspect::require_events() } ];
}

subtest 'compile-time cycle' => sub {
    is_deeply events(), [
        { name => 'Deps/A.pm', status => 'load',        stack => [] },
        { name => 'Deps/B.pm', status => 'load',        stack => ['Deps/A.pm'] },
        { name => 'Deps/A.pm', status => 'in_progress', stack => ['Deps/A.pm', 'Deps/B.pm'] },
    ], 'events';
    my ($first, $closing) = grep { ($_->{module} // '') eq 'Deps::A' }
        @{ LibPerlRs::Inspect::require_events() };
    is $first->{from_file}, $here, 'from_file of the use in this file';
    is $first->{from_line}, 11, 'from_line';
    like $closing->{from_file}, qr{Deps/B\.pm\z}, 'from_file of the closing use';
    is $closing->{from_line}, 3, 'from_line of the closing use';
    my %status = map { $_->{name} => $_->{status} } @{ LibPerlRs::Inspect::require_events() };
    is $status{'strict.pm'}, 'cached', 'already loaded module';
    ok !grep({ /^5/ } keys %status), 'version requires are not recorded';
};

subtest 'run time' => sub {
    LibPerlRs::Inspect::require_trace_start();
    # require ops compiled while tracing (Deps::B::b) are traced
    Deps::B::b();
    is_deeply events(), [
        { name => 'Deps/C.pm', status => 'load',   stack => [] },
        { name => 'Deps/C.pm', status => 'cached', stack => [] },
    ], 'static and dynamic require in a sub';

    LibPerlRs::Inspect::require_trace_start();
    for (1, 2) {
        eval q{ require Deps::Broken; 1 } and fail 'Deps::Broken loaded';
    }
    is_deeply [ map { $_->{status} } @{ events() } ], ['load', 'failed'],
        'a failed load';
};

subtest 'path and reload_of' => sub {
    LibPerlRs::Inspect::require_trace_start();
    # the same file under two %INC keys: Cyc/Router.pm is reached again
    # through the cycle Router -> Main -> Gate -> Hub -> Router
    eval q{ require "./t/lib-deps/Cyc/Router.pm"; 1 } or die $@;
    my @ev = grep { $_->{status} eq 'load' } @{ LibPerlRs::Inspect::require_events() };
    my ($first) = grep { $_->{name} eq './t/lib-deps/Cyc/Router.pm' } @ev;
    is $first->{path}, './t/lib-deps/Cyc/Router.pm', 'explicit path';
    ok !exists $first->{reload_of}, 'first load';
    my ($gate) = grep { $_->{name} eq 'Cyc/Gate.pm' } @ev;
    like $gate->{path}, qr{lib-deps/Cyc/Gate\.pm\z}, 'path found in @INC';
    my ($again) = grep { $_->{name} eq 'Cyc/Router.pm' } @ev;
    is $again->{reload_of}, './t/lib-deps/Cyc/Router.pm', 'reload of the other key';
};

subtest 'stop' => sub {
    LibPerlRs::Inspect::require_trace_start();
    LibPerlRs::Inspect::require_trace_stop();
    eval q{ require Deps::C; 1 } or die $@;
    is_deeply events(), [], 'nothing recorded after stop';
};

done_testing;
