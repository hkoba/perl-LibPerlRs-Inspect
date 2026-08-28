use libperl_config::*;

fn main() {
    // begins.rs の PL_savebegin 書き込みが threading モードで形を変える
    // ため、perl_useithreads cfg をこの crate にも立てる
    let cfg = PerlConfig::default();
    cfg.emit_features(&["useithreads"]);
}
