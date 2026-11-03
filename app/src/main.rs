//! Crossbow browser entry point.
//!
//! Order matters: CEF must start before any window exists. When this process is
//! a Chromium sub-process (renderer, GPU, …) `crossbow_engine::start` runs it and
//! we exit immediately.

mod app;
mod ai;
mod ai_features;
mod ask_bar;
mod ask_page;
mod chat;
mod app_icon;
mod content_blocking;
mod backups;
mod capture;
mod downloads;
mod favicons;
mod feel;
mod import;
mod input;
mod hotkey;
mod links;
mod link_preview;
mod new_document;
mod link_prefs;
mod native_window;
mod window_appearance;
mod display_geometry;
mod passwords;
mod site_permissions;
mod performance;
mod quit;
mod quote;
mod search_engines;
mod shortcuts;
mod numbered_tabs;
mod sidebar_prefs;
mod suggestions;
mod sync_glue;
mod split_session;
mod tabs;
mod tidy_tabs;
mod texture_retirement;
mod tidy_downloads;
mod translate;
mod ui;
mod unread;
mod updates;
mod website_appearance;
mod window_host;
#[cfg(test)]
mod test_codes;

use crossbow_engine::{EngineOptions, Startup};

fn main() -> std::process::ExitCode {
    // The updater runs a new version once with --update-probe: answer and stop.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(code) = crossbow_core::update::install::answer_probe(&args) {
        return std::process::ExitCode::from(code as u8);
    }
    let data_dir = crossbow_core::data_dir();
    // Before the engine starts, so a version that crashes there is still
    // counted: one that keeps failing to start is swapped back for the old one.
    let subprocess = args.iter().any(|arg| arg.to_string_lossy().starts_with("--type="));
    if !subprocess && updates::on_start(&data_dir) {
        return std::process::ExitCode::SUCCESS;
    }
    let engine = match crossbow_engine::start(&EngineOptions {
        data_dir: data_dir.clone(),
        locale: "en-US".into(),
        remote_debugging_port: std::env::var("CROSSBOW_REMOTE_DEBUGGING_PORT").ok().and_then(|p| p.parse().ok()),
    }) {
        Startup::SubProcess(code) => return std::process::ExitCode::from(code as u8),
        Startup::Browser(guard) => guard,
    };

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,crossbow=info")).init();
    let _ = std::fs::create_dir_all(&data_dir);
    // Before the event loop, so the link that launched Crossbow waits in the queue.
    links::install_native_receiver();
    links::receive_arguments(links::inbox(), std::env::args().skip(1));

    let options = eframe::NativeOptions {
        viewport: ui::window::main_viewport(),
        renderer: eframe::Renderer::Wgpu,
        persist_window: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Crossbow",
        options,
        Box::new(move |cc| Ok(Box::new(app::CrossbowApp::new(cc, data_dir)))),
    );
    if let Err(e) = result {
        eprintln!("crossbow: {e}");
    }
    // Let closing browsers finish before CEF shuts down (when `engine` drops).
    crossbow_engine::drain(std::time::Duration::from_millis(400));
    drop(engine);
    std::process::ExitCode::SUCCESS
}
