package Imp::Quiet;
# Imp::Listener does `use Imp::Quiet;` before @EXPORT is set: it compiles,
# but nothing is imported.
use strict;
use Exporter 'import';
use Imp::Listener;
our @EXPORT;
BEGIN { @EXPORT = qw(shout) }
sub shout { 'shout' }
1;
