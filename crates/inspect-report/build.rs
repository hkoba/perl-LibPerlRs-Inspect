use libperl_config::*;

fn main() {
    // ldopts are needed so that this crate's own test binaries (which
    // embed perl) link against libperl
    let cfg = PerlConfig::default();
    cfg.emit_cargo_ldopts();
    cfg.emit_features(&["useithreads"]);
}
