use libperl_config::*;

fn main() {
    // The PL_savebegin write in begins.rs takes a different form depending
    // on the threading mode, so raise the perl_useithreads cfg in this crate too
    let cfg = PerlConfig::default();
    cfg.emit_features(&["useithreads"]);
}
