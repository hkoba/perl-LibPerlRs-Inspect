package OpTree::Analyzer;
use strict;
use warnings;

our $VERSION = '0.01';

require XSLoader;
XSLoader::load('OpTree::Analyzer', $VERSION);

require JSON::PP;

sub op_names {
    my ($code) = @_;
    JSON::PP::decode_json(op_names_json($code));
}

sub capture {
    my ($code) = @_;
    JSON::PP::decode_json(capture_json($code));
}

1;
__END__

=head1 NAME

OpTree::Analyzer - analyze a subroutine reference at the OP tree level

=head1 SYNOPSIS

    use OpTree::Analyzer;

    my $sub = eval 'sub { my ($x) = @_; $x + 1 }';
    my $names = OpTree::Analyzer::op_names($sub);   # execution-order op names

=head1 DESCRIPTION

Rust-powered OP tree analysis for subroutine references, including
anonymous subs returned by C<eval>. See the project README.

=cut
