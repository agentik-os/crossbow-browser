//! ES01-P1 Variant A preparation, not an extension-compatibility test.
//! Run only from its separate macOS bundle with a fresh temporary data directory.
//! `HELP` below gives its arguments and the two-launch procedure.

#[cfg(not(all(target_os = "macos", feature = "sandbox")))]
compile_error!("host_probe requires macOS and the existing sandbox feature");

#[cfg(all(target_os = "macos", feature = "sandbox"))]
mod probe {
    use cef::{application_mac::*, args::Args, *};
    use objc2::{
        ClassType, MainThreadMarker, define_class, msg_send,
        rc::Retained,
        runtime::{AnyObject, Bool},
    };
    use objc2_app_kit::{NSApplication, NSEvent};
    use std::{
        cell::{Cell, RefCell},
        ffi::CStr,
        fs::{self, File},
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        path::{Component, Path, PathBuf},
        sync::mpsc,
        time::Duration,
    };

    const APP_NAME: &str = "Crossbow Host Probe";
    const API_HASH: &str = "e5cd28047fc1f8bbb972e54692fc665040c7f034";
    const COMMIT_HASH: &str = "14c5a089e8452874cb1a556dc3fde26ed2d87723";
    const HELP: &str = "host_probe --data-dir ABSOLUTE_NEW_TEMP_PATH --ack-disposable --seconds 5..120 --size normal|compact\nThe data-directory name must start with crossbow-host-probe-. Existing targets, symlinks, home/default profiles and arbitrary Chromium arguments are refused. Run twice with two different fresh directories; never use cargo run.";

    thread_local! {
        static BROWSERS: RefCell<Vec<Browser>> = const { RefCell::new(Vec::new()) };
        static RECEIPT: RefCell<Option<File>> = const { RefCell::new(None) };
        static EXPECTED_CACHE: RefCell<PathBuf> = RefCell::new(PathBuf::new());
        static CREATED: Cell<u32> = const { Cell::new(0) };
        static CLOSED: Cell<u32> = const { Cell::new(0) };
        static FAILED: Cell<bool> = const { Cell::new(false) };
        static CLOSING: Cell<bool> = const { Cell::new(false) };
        static HANDLING_EVENT: Cell<bool> = const { Cell::new(false) };
    }

    // Match CefScopedSendingEvent's save/restore contract, including nested events.
    define_class!(
        #[unsafe(super(NSApplication))]
        #[name = "CrossbowHostProbeApplication"]
        struct ProbeApplication;

        unsafe impl CrAppProtocol for ProbeApplication {
            #[unsafe(method(isHandlingSendEvent))]
            unsafe fn is_handling_send_event(&self) -> Bool {
                Bool::new(HANDLING_EVENT.get())
            }
        }
        unsafe impl CrAppControlProtocol for ProbeApplication {
            #[unsafe(method(setHandlingSendEvent:))]
            unsafe fn set_handling_send_event(&self, value: Bool) {
                HANDLING_EVENT.set(value.as_bool());
            }
        }
        unsafe impl CefAppProtocol for ProbeApplication {}

        impl ProbeApplication {
            #[unsafe(method(sendEvent:))]
            fn send_event(&self, event: &NSEvent) {
                let previous = HANDLING_EVENT.replace(true);
                unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
                HANDLING_EVENT.set(previous);
            }

            #[unsafe(method(terminate:))]
            fn terminate_probe(&self, _sender: Option<&AnyObject>) {
                request_close();
            }
        }
    );

    #[derive(Debug)]
    struct Options {
        data_dir: PathBuf,
        seconds: u64,
        compact: bool,
    }

    impl Options {
        fn parse(args: &[String]) -> Result<Self, &'static str> {
            let mut data_dir = None;
            let mut seconds = None;
            let mut compact = None;
            let mut acknowledged = false;
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--data-dir" if data_dir.is_none() => {
                        data_dir = Some(PathBuf::from(args.next().ok_or("missing_data_dir")?));
                    }
                    "--seconds" if seconds.is_none() => {
                        let n = args
                            .next()
                            .ok_or("missing_duration")?
                            .parse::<u64>()
                            .map_err(|_| "invalid_duration")?;
                        if !(5..=120).contains(&n) {
                            return Err("invalid_duration");
                        }
                        seconds = Some(n);
                    }
                    "--size" if compact.is_none() => {
                        compact = Some(match args.next().map(String::as_str) {
                            Some("normal") => false,
                            Some("compact") => true,
                            _ => return Err("invalid_size"),
                        });
                    }
                    "--ack-disposable" if !acknowledged => acknowledged = true,
                    _ => return Err("unknown_or_duplicate_argument"),
                }
            }
            if !acknowledged {
                return Err("disposable_ack_required");
            }
            Ok(Self {
                data_dir: data_dir.ok_or("data_dir_required")?,
                seconds: seconds.ok_or("duration_required")?,
                compact: compact.ok_or("size_required")?,
            })
        }
    }

    fn validate_data_path(path: &Path) -> Result<PathBuf, &'static str> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err("absolute_normal_path_required");
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid_fixture_name")?;
        if !name.starts_with("crossbow-host-probe-")
            || name.len() <= "crossbow-host-probe-".len()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid_fixture_name");
        }
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("fixture_target_reuse_refused"),
        }
        let parent = path.parent().ok_or("fixture_parent_required")?;
        let canonical_parent = parent
            .canonicalize()
            .map_err(|_| "fixture_parent_missing")?;
        if parent != canonical_parent {
            return Err("symlink_or_noncanonical_parent_refused");
        }
        let temporary = [std::env::temp_dir(), PathBuf::from("/private/tmp")]
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .any(|root| canonical_parent.starts_with(root));
        if !temporary {
            return Err("temporary_root_required");
        }
        if let Some(home) =
            std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok())
        {
            if canonical_parent.starts_with(home) {
                return Err("home_profile_refused");
            }
        }
        if path.to_str().is_none() {
            return Err("utf8_path_required");
        }
        Ok(canonical_parent.join(name))
    }

    fn prepare_data(path: &Path) -> Result<PathBuf, &'static str> {
        let path = validate_data_path(path)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "fixture_create_failed_or_reused")?;
        // Do not delete even partial/failed runs. A new attempt must use a new path.
        let receipt = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join("runtime.jsonl"))
            .map_err(|_| "receipt_create_failed")?;
        RECEIPT.with_borrow_mut(|file| *file = Some(receipt));
        Ok(path)
    }

    fn record(json: &str) {
        let result = RECEIPT.with_borrow_mut(|file| {
            let Some(file) = file.as_mut() else {
                return Ok(());
            };
            writeln!(file, "{json}").and_then(|_| file.flush())
        });
        if result.is_err() {
            eprintln!("{{\"event\":\"receipt_write_failed\"}}");
            std::process::exit(73);
        }
        println!("{json}");
    }

    fn fail(event: &'static str) {
        FAILED.set(true);
        record(&format!("{{\"event\":\"{event}\"}}"));
    }

    fn request_close() {
        if CLOSING.replace(true) {
            return;
        }
        record("{\"event\":\"close_requested\",\"force_close\":false}");
        let browsers = BROWSERS.with_borrow(Clone::clone);
        if browsers.is_empty() {
            fail("no_live_browser_at_close");
            quit_message_loop();
        }
        for browser in browsers {
            if let Some(host) = browser.host() {
                host.close_browser(0);
            }
        }
    }

    wrap_task! {
        struct CloseTask;
        impl Task {
            fn execute(&self) { request_close(); }
        }
    }

    wrap_life_span_handler! {
        struct ProbeLifeSpan;
        impl LifeSpanHandler {
            fn on_after_created(&self, browser: Option<&mut Browser>) {
                let Some(browser) = browser else { fail("missing_browser"); return; };
                CREATED.set(CREATED.get() + 1);
                let Some(host) = browser.host() else { fail("missing_browser_host"); return; };
                let style = host.runtime_style();
                let windowless = host.is_window_rendering_disabled() != 0;
                let style_name = if style == RuntimeStyle::CHROME { "chrome" }
                    else if style == RuntimeStyle::ALLOY { "alloy" } else { "default_or_unknown" };
                record(&format!("{{\"event\":\"browser_created\",\"runtime_style\":\"{style_name}\",\"windowless\":{windowless}}}"));
                BROWSERS.with_borrow_mut(|browsers| browsers.push(browser.clone()));
                if style != RuntimeStyle::CHROME || windowless { fail("unexpected_runtime_style"); }
                if CLOSING.get() { host.close_browser(0); }
            }

            fn on_before_close(&self, browser: Option<&mut Browser>) {
                let Some(browser) = browser else { fail("missing_closing_browser"); return; };
                let id = browser.identifier();
                BROWSERS.with_borrow_mut(|browsers| browsers.retain(|b| b.identifier() != id));
                CLOSED.set(CLOSED.get() + 1);
                record("{\"event\":\"browser_closed\"}");
                if BROWSERS.with_borrow(Vec::is_empty) { quit_message_loop(); }
            }
        }
    }

    wrap_client! {
        struct ProbeClient;
        impl Client {
            fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(ProbeLifeSpan::new()) }
        }
    }

    wrap_browser_process_handler! {
        struct ProbeProcess {
            seconds: u64,
            compact: bool,
        }
        impl BrowserProcessHandler {
            fn default_client(&self) -> Option<Client> { Some(ProbeClient::new()) }

            fn on_context_initialized(&self) {
                record("{\"event\":\"context_initialized\"}");
                let cache_matches = request_context_get_global_context().is_some_and(|context| {
                    let actual = CefString::from(&context.cache_path()).to_string();
                    EXPECTED_CACHE.with_borrow(|expected| Path::new(&actual) == expected)
                });
                if !cache_matches {
                    fail("global_cache_path_not_retained");
                    quit_message_loop();
                    return;
                }
                record("{\"event\":\"disposable_cache_path_retained\"}");
                let (width, height) = if self.compact { (640, 420) } else { (1360, 860) };
                let window = WindowInfo {
                    window_name: APP_NAME.into(),
                    bounds: Rect { x: 100, y: 100, width, height },
                    runtime_style: RuntimeStyle::CHROME,
                    // A top-level window: parent_view is null; OSR stays disabled.
                    ..Default::default()
                };
                let accepted = browser_host_create_browser(
                    Some(&window), Some(&mut ProbeClient::new()), Some(&"chrome://extensions/".into()),
                    Some(&BrowserSettings::default()), None, None,
                );
                if accepted != 1 {
                    fail("browser_creation_rejected");
                    quit_message_loop();
                    return;
                }
                if post_delayed_task(ThreadId::UI, Some(&mut CloseTask::new()), (self.seconds * 1000) as i64) != 1 {
                    fail("close_task_rejected");
                    request_close();
                }
            }
        }
    }

    wrap_app! {
        struct ProbeApp {
            seconds: u64,
            compact: bool,
        }
        impl App {
            fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
                if process_type.is_some_and(|value| !value.to_string().is_empty()) { return; }
                if let Some(command_line) = command_line {
                    for flag in ["no-startup-window", "no-first-run", "no-default-browser-check"] {
                        command_line.append_switch(Some(&flag.into()));
                    }
                }
            }

            fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
                Some(ProbeProcess::new(self.seconds, self.compact))
            }
        }
    }

    fn bundle_framework(exe: &Path) -> Result<PathBuf, &'static str> {
        if exe.file_name().and_then(|name| name.to_str()) != Some(APP_NAME)
            || exe
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                != Some("MacOS")
        {
            return Err("separate_probe_bundle_required");
        }
        let contents = exe
            .parent()
            .and_then(Path::parent)
            .ok_or("invalid_probe_bundle")?;
        if contents.file_name().and_then(|name| name.to_str()) != Some("Contents")
            || contents
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                != Some("Crossbow Host Probe.app")
        {
            return Err("separate_probe_bundle_required");
        }
        let frameworks = contents.join("Frameworks");
        let framework =
            frameworks.join("Chromium Embedded Framework.framework/Chromium Embedded Framework");
        let helper = frameworks
            .join("Crossbow Host Probe Helper.app/Contents/MacOS/Crossbow Host Probe Helper");
        if !framework.is_file() || !helper.is_file() {
            return Err("framework_or_helper_missing");
        }
        Ok(framework)
    }

    fn check_loaded_api() -> Result<(), &'static str> {
        if sys::CEF_API_VERSION_LAST != 15400 {
            return Err("binding_api_changed");
        }
        for (entry, expected) in [(0, API_HASH), (2, COMMIT_HASH)] {
            let hash = api_hash(sys::CEF_API_VERSION_LAST, entry);
            if hash.is_null() {
                return Err("runtime_hash_unavailable");
            }
            // CEF owns these NUL-terminated immutable strings for its loaded lifetime.
            if unsafe { CStr::from_ptr(hash) }.to_bytes() != expected.as_bytes() {
                return Err("runtime_api_or_commit_mismatch");
            }
        }
        if api_version() != 15400 {
            return Err("runtime_api_version_mismatch");
        }
        record(&format!(
            "{{\"event\":\"runtime_api_verified\",\"api_version\":15400,\"platform_hash\":\"{API_HASH}\",\"commit_hash\":\"{COMMIT_HASH}\"}}"
        ));
        record(
            "{\"event\":\"compiled_version\",\"cef\":\"154.0.34\",\"chromium\":\"154.0.8037.98\",\"runtime_version_components_queried\":false}",
        );
        Ok(())
    }

    fn run_browser(options: Options) -> Result<(), &'static str> {
        let exe = std::env::current_exe().map_err(|_| "executable_path_unavailable")?;
        bundle_framework(&exe)?;
        let data_dir = prepare_data(&options.data_dir)?;
        record(
            "{\"event\":\"probe_start\",\"variant\":\"A\",\"sandbox_requested\":true,\"remote_debugging\":false,\"extension_compatibility\":\"not_tested\"}",
        );
        let (done, wait) = mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            if matches!(
                wait.recv_timeout(Duration::from_secs(options.seconds + 30)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                eprintln!("{{\"event\":\"hard_deadline_exceeded\",\"clean_shutdown\":false}}");
                std::process::exit(74);
            }
        });
        let loader = library_loader::LibraryLoader::new(&exe, false);
        if !loader.load() {
            return Err("framework_load_failed");
        }
        check_loaded_api()?;
        let args = Args::new();
        let mut app = ProbeApp::new(options.seconds, options.compact);
        if execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        ) >= 0
        {
            return Err("unexpected_browser_process_result");
        }
        MainThreadMarker::new().ok_or("main_thread_required")?;
        // CEF's macOS client contract requires this subclass before initialize.
        let native_app: Retained<ProbeApplication> =
            unsafe { msg_send![ProbeApplication::class(), sharedApplication] };
        let correct_class: bool =
            unsafe { msg_send![&*native_app, isKindOfClass: ProbeApplication::class()] };
        if !correct_class {
            return Err("cef_application_subclass_not_active");
        }
        let root = data_dir.join("Chromium");
        EXPECTED_CACHE.with_borrow_mut(|cache| *cache = root.join("Default"));
        let settings = Settings {
            no_sandbox: 0,
            command_line_args_disabled: 1,
            root_cache_path: root.to_str().ok_or("invalid_root")?.into(),
            cache_path: root
                .join("Default")
                .to_str()
                .ok_or("invalid_cache_path")?
                .into(),
            log_severity: LogSeverity::DISABLE,
            locale: "en-US".into(),
            ..Default::default()
        };
        if initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        ) != 1
        {
            return Err("cef_initialize_failed");
        }
        // CefString fields must be released while the framework is still loaded.
        drop(settings);
        run_message_loop();
        let balanced =
            BROWSERS.with_borrow(Vec::is_empty) && CREATED.get() == 1 && CLOSED.get() == 1;
        if !balanced {
            fail("browser_close_barrier_failed");
        }
        if !BROWSERS.with_borrow(Vec::is_empty) {
            record("{\"event\":\"live_browsers_remain\",\"clean_shutdown\":false}");
            // Never unload CEF beneath live browser references or call shutdown early.
            std::process::exit(75);
        }
        drop(app);
        shutdown();
        record("{\"event\":\"shutdown_completed\",\"extension_compatibility\":\"not_tested\"}");
        drop(native_app);
        drop(loader);
        let _ = done.send(());
        let _ = watchdog.join();
        if FAILED.get() {
            return Err("host_control_failed");
        }
        Ok(())
    }

    pub fn main() -> std::process::ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args == ["--help"] {
            println!("{HELP}");
            return std::process::ExitCode::SUCCESS;
        }
        if args.iter().any(|arg| arg.starts_with("--type=")) {
            let valid_helper = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_owned()))
                .and_then(|n| {
                    n.to_str()
                        .map(|s| s.starts_with("Crossbow Host Probe Helper"))
                })
                .unwrap_or(false);
            if !valid_helper {
                eprintln!("{{\"event\":\"helper_entry_refused\"}}");
                return std::process::ExitCode::from(64);
            }
            // Existing production bootstrap initializes the sandbox before CEF loading.
            return std::process::ExitCode::from(crossbow_engine::run_helper().max(0) as u8);
        }
        match Options::parse(&args).and_then(run_browser) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(reason) => {
                eprintln!("{{\"event\":\"probe_failed\",\"reason\":\"{reason}\"}}");
                std::process::ExitCode::from(1)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(extra: &[&str]) -> Vec<String> {
            [
                "--data-dir",
                "/private/tmp/crossbow-host-probe-test",
                "--ack-disposable",
                "--seconds",
                "20",
                "--size",
                "normal",
            ]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect()
        }

        #[test]
        fn requires_explicit_isolation_and_bounds() {
            assert!(Options::parse(&[]).is_err());
            assert!(Options::parse(&args(&[])).is_ok());
            assert!(Options::parse(&args(&["--no-sandbox"])).is_err());
            assert!(Options::parse(&args(&["--data-dir", "/private/tmp/other"])).is_err());
            for n in ["0", "121", "-1", "invalid"] {
                let mut arguments = args(&[]);
                arguments[4] = n.into();
                assert!(Options::parse(&arguments).is_err());
            }
            let mut arguments = args(&[]);
            arguments.remove(2);
            assert!(Options::parse(&arguments).is_err());
        }

        #[test]
        fn rejects_personal_default_relative_and_traversal_paths() {
            for path in [
                ".",
                "Chromium/Default",
                "/",
                "/private/tmp/Default",
                "/private/tmp/../crossbow-host-probe-x",
                "/Users/example/crossbow-host-probe-x",
            ] {
                assert!(validate_data_path(Path::new(path)).is_err(), "{path}");
            }
        }

        #[test]
        fn fresh_target_is_accepted_but_existing_and_symlink_targets_are_not() {
            let root = std::env::var_os("CROSSBOW_HOST_PROBE_TEST_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .canonicalize()
                .unwrap();
            let parent = root.join(format!(
                "crossbow-host-probe-guard-test-{}",
                std::process::id()
            ));
            fs::DirBuilder::new().mode(0o700).create(&parent).unwrap();
            let target = parent.join("crossbow-host-probe-fresh");
            assert_eq!(validate_data_path(&target).unwrap(), target);
            fs::create_dir(&target).unwrap();
            assert_eq!(
                validate_data_path(&target).unwrap_err(),
                "fixture_target_reuse_refused"
            );
            fs::remove_dir(&target).unwrap();
            std::os::unix::fs::symlink(parent.join("missing"), &target).unwrap();
            assert_eq!(
                validate_data_path(&target).unwrap_err(),
                "fixture_target_reuse_refused"
            );
            fs::remove_file(&target).unwrap();
            fs::remove_dir(&parent).unwrap();
        }

        #[test]
        fn rejects_unbundled_or_product_executables() {
            assert!(bundle_framework(Path::new("/private/tmp/host_probe")).is_err());
            assert!(
                bundle_framework(Path::new(
                    "/Applications/Crossbow.app/Contents/MacOS/Crossbow"
                ))
                .is_err()
            );
        }
    }
}

fn main() -> std::process::ExitCode {
    probe::main()
}
