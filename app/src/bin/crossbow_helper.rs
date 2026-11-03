//! Chromium sub-process executable used inside the macOS app bundle
//! (`Crossbow Helper*.app`). Windows and Linux re-launch the main executable.

fn main() {
    let code = crossbow_engine::run_helper();
    std::process::exit(code.max(0));
}
