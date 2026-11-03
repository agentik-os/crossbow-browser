//! Profile-bound, device-local provider preferences and opt-in diagnostics.
//! Saving a selection is not authorization to send browser content or run a CLI.

use crossbow_ai::local::{Diagnostic, DiagnosticResult, DiagnosticTask, LocalEndpoint, LocalError, LocalProvider, valid_model_id};
use crossbow_ai::providers::{self, Discovery, DiscoveryRoots};
use crossbow_core::library::Library;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock, atomic::{AtomicBool, Ordering}, mpsc::{self, Receiver}};
use std::time::{Duration, Instant};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
static DISCOVERY_WORKER: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

struct DiscoveryControl {
    cancelled: AtomicBool,
    deadline: Instant,
}

impl DiscoveryControl {
    fn stopped(&self) -> bool { self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline }
}

// Only the worker owns this permit. Dropping a draft cannot free a slot while a
// filesystem syscall is still blocked; cancelled/timed-out scans are not queued.
struct DiscoveryPermit(Arc<AtomicBool>);

impl Drop for DiscoveryPermit {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}

struct DiscoveryTask {
    receiver: Receiver<Option<Discovery>>,
    control: Arc<DiscoveryControl>,
    ctx: egui::Context,
}

impl DiscoveryTask {
    fn poll(&self, now: Instant) -> Option<Result<Discovery, &'static str>> {
        if now >= self.control.deadline {
            self.control.cancelled.store(true, Ordering::Release);
            return Some(Err("Provider scan timed out. Filesystem access may still be pending; wait before trying Scan Again."));
        }
        match self.receiver.try_recv() {
            Ok(Some(result)) => Some(Ok(result)),
            Ok(None) | Err(mpsc::TryRecvError::Disconnected) => Some(Err("The provider scan stopped. Try Scan Again.")),
            Err(mpsc::TryRecvError::Empty) => {
                // Earlier input/animation repaints consume egui's next wake.
                // Re-arm the remaining deadline on every pending UI pass.
                self.ctx.request_repaint_after(self.control.deadline.saturating_duration_since(now));
                None
            }
        }
    }
}

impl Drop for DiscoveryTask {
    fn drop(&mut self) { self.control.cancelled.store(true, Ordering::Release); }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub endpoint: LocalEndpoint,
    pub model: Option<String>,
}

fn key(profile: &str) -> String { format!("ai.providers.v1.{profile}") }

impl Preferences {
    fn validate(&self) -> Result<(), String> {
        self.endpoint.validate().map_err(|error| error.to_string())?;
        if self.model.as_ref().is_some_and(|model| !valid_model_id(model)) { return Err(LocalError::InvalidModel.to_string()); }
        Ok(())
    }
}

/// Only an explicitly saved, valid selection can be used for a conversation.
/// This reads device-local preferences, never provider credentials or page data.
pub fn saved_selection(lib: &Library, profile: &str) -> Result<Preferences, String> {
    let raw = lib.store.kv_get(&key(profile)).map_err(|_| "Could not read the saved provider. Reopen AI Providers and retry.".to_string())?
        .ok_or_else(|| "Choose a provider and model in AI Providers, then Save Selection.".to_string())?;
    let preferences: Preferences = serde_json::from_str(&raw).map_err(|_| "Saved provider preferences are invalid. Configure and save a new selection.".to_string())?;
    preferences.validate()?;
    if preferences.model.is_none() { return Err("Choose a model in AI Providers, then Save Selection.".into()); }
    Ok(preferences)
}

struct InFlight {
    endpoint: LocalEndpoint,
    diagnostic: Diagnostic,
    task: DiagnosticTask,
}

pub struct SettingsDraft {
    pub profile: String,
    pub saved: Option<Preferences>,
    pub draft: Preferences,
    pub error: Option<String>,
    pub reveal_error: bool,
    pub answer: Option<String>,
    pub models: Vec<String>,
    pub discovery: Option<Discovery>,
    pub discovery_error: Option<String>,
    pub reveal_discovery_error: bool,
    baseline: Option<String>,
    writable: bool,
    catalogue_endpoint: Option<LocalEndpoint>,
    diagnostic: Option<InFlight>,
    discovery_task: Option<DiscoveryTask>,
}

impl SettingsDraft {
    pub fn load(lib: &Library, profile: &str) -> Self {
        let raw = if lib.profiles.contains_key(profile) {
            lib.store.kv_get(&key(profile)).map_err(|_| "Could not read provider preferences. Retry loading before saving.")
        } else { Err("This profile is unavailable. Select a space with an available profile.") };
        let writable = raw.is_ok();
        let baseline = raw.as_ref().ok().cloned().flatten();
        let result = raw.and_then(|raw| match raw {
            Some(raw) => serde_json::from_str::<Preferences>(&raw).ok().filter(|preferences| preferences.validate().is_ok())
                .ok_or("Saved provider preferences are invalid. Choose new values and Save, or leave them unchanged."),
            None => Ok(Preferences::default()),
        });
        let saved = result.as_ref().ok().cloned();
        Self {
            profile: profile.into(), draft: saved.clone().unwrap_or_default(), saved,
            error: result.err().map(str::to_owned), reveal_error: true, answer: None, models: Vec::new(),
            discovery: None, discovery_error: None, reveal_discovery_error: false, baseline, writable, catalogue_endpoint: None,
            diagnostic: None, discovery_task: None,
        }
    }

    pub fn changed(&self) -> bool { self.saved.as_ref() != Some(&self.draft) }
    pub fn busy(&self) -> bool { self.diagnostic.is_some() }
    pub fn discovering(&self) -> bool { self.discovery_task.is_some() }
    pub fn catalogue_loaded(&self) -> bool { self.catalogue_endpoint == Some(self.draft.endpoint) }
    pub fn testing(&self) -> bool { self.diagnostic.as_ref().is_some_and(|job| matches!(job.diagnostic, Diagnostic::ConnectionTest { .. })) }
    pub fn can_test(&self) -> bool {
        !self.busy() && self.catalogue_loaded() && self.draft.model.as_ref().is_some_and(|model| self.models.contains(model))
    }

    fn fail(&mut self, message: impl Into<String>) { self.error = Some(message.into()); self.reveal_error = true; }

    fn check_owner(&mut self, lib: &mut Library, active_profile: Option<&str>) -> bool {
        if active_profile != Some(self.profile.as_str()) || lib.refresh_profile(&self.profile).is_err() {
            self.diagnostic = None;
            self.answer = None;
            self.fail("This draft belongs to a different or unavailable profile. Reopen AI Providers in the intended space.");
            return false;
        }
        true
    }

    pub fn save(&mut self, lib: &mut Library, active_profile: Option<&str>) {
        if !self.check_owner(lib, active_profile) { return; }
        let result = (|| {
            if !self.writable { return Err("Retry loading provider preferences before saving.".to_string()); }
            self.draft.validate()?;
            let raw = serde_json::to_string(&self.draft).map_err(|_| "Could not encode provider preferences.".to_string())?;
            let saved = lib.store.kv_compare_and_set(&key(&self.profile), self.baseline.as_deref(), &raw)
                .map_err(|_| "Could not save provider preferences. Your draft is unchanged. Check disk space and permissions, then retry Save or Cancel.".to_string())?;
            if !saved { return Err("Provider preferences changed elsewhere. Cancel to reload them before making a new choice.".into()); }
            Ok(raw)
        })();
        match result {
            Ok(raw) => { self.baseline = Some(raw); self.saved = Some(self.draft.clone()); self.error = None; }
            Err(error) => self.fail(error),
        }
    }

    pub fn reload(&mut self, lib: &Library) { *self = Self::load(lib, &self.profile); }

    pub fn set_provider(&mut self, provider: LocalProvider) {
        if self.draft.endpoint.provider == provider { return; }
        self.set_endpoint(LocalEndpoint { provider, port: provider.default_port() });
    }

    pub fn set_endpoint(&mut self, endpoint: LocalEndpoint) {
        if self.draft.endpoint == endpoint { return; }
        self.diagnostic = None;
        self.draft.endpoint = endpoint;
        self.draft.model = None;
        self.catalogue_endpoint = None;
        self.models.clear();
        self.answer = None;
        self.error = None;
    }

    pub fn set_model(&mut self, model: Option<String>) {
        if self.draft.model == model { return; }
        self.diagnostic = None;
        self.draft.model = model;
        self.answer = None;
        self.error = None;
    }

    pub fn start(&mut self, lib: &mut Library, active_profile: Option<&str>, diagnostic: Diagnostic, ctx: &egui::Context) {
        if self.busy() || !self.check_owner(lib, active_profile) { return; }
        if let Diagnostic::ConnectionTest { model } = &diagnostic {
            if !self.can_test() || self.draft.model.as_ref() != Some(model) {
                self.fail("Refresh models and select one before sending the test phrase.");
                return;
            }
        } else {
            // A failed refresh must not leave a stale catalogue eligible for testing.
            self.catalogue_endpoint = None;
            self.models.clear();
        }
        self.answer = None;
        let ctx = ctx.clone();
        match DiagnosticTask::spawn(self.draft.endpoint, diagnostic.clone(), move || ctx.request_repaint()) {
            Ok(task) => {
                self.diagnostic = Some(InFlight { endpoint: self.draft.endpoint, diagnostic, task });
                self.error = None;
            }
            Err(error) => self.fail(error),
        }
    }

    pub fn cancel_request(&mut self) {
        if self.diagnostic.take().is_some() { self.fail(LocalError::Cancelled.to_string()); }
    }

    fn fail_discovery(&mut self, message: &'static str) {
        self.discovery_error = Some(message.into());
        self.reveal_discovery_error = true;
    }

    pub fn cancel_scan(&mut self) {
        if self.discovery_task.take().is_some() {
            self.fail_discovery("Provider scan cancelled. Any pending filesystem access must finish before another scan can start.");
        }
    }

    pub fn scan(&mut self, roots: DiscoveryRoots, ctx: &egui::Context) {
        self.scan_with(&DISCOVERY_WORKER, move |control| providers::discover_cancellable(&roots, || control.stopped()), ctx);
    }

    fn scan_with(&mut self, gate: &Arc<AtomicBool>, discover: impl FnOnce(&DiscoveryControl) -> Option<Discovery> + Send + 'static, ctx: &egui::Context) {
        if self.discovering() { return; }
        self.discovery = None;
        self.discovery_error = None;
        self.reveal_discovery_error = false;
        if gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            self.fail_discovery("A previous provider scan is still finishing. Wait before trying Scan Again.");
            return;
        }
        let permit = DiscoveryPermit(Arc::clone(gate));
        let control = Arc::new(DiscoveryControl { cancelled: AtomicBool::new(false), deadline: Instant::now() + DISCOVERY_TIMEOUT });
        let (sender, receiver) = mpsc::channel();
        let worker_control = Arc::clone(&control);
        let worker_ctx = ctx.clone();
        match std::thread::Builder::new().name("crossbow-ai-discovery".into()).spawn(move || {
            let _permit = permit;
            let result = if worker_control.stopped() { None } else { discover(&worker_control) };
            let _ = sender.send(result);
            worker_ctx.request_repaint();
        }) {
            Ok(_) => {
                self.discovery_task = Some(DiscoveryTask { receiver, control, ctx: ctx.clone() });
                // A stalled filesystem lookup cannot wake egui to show its deadline.
                ctx.request_repaint_after(DISCOVERY_TIMEOUT);
            }
            Err(_) => self.fail_discovery("Could not scan installed providers. Try Scan Again."),
        }
    }

    fn poll_discovery(&mut self, now: Instant) {
        if let Some(result) = self.discovery_task.as_ref().and_then(|task| task.poll(now)) {
            self.discovery_task = None;
            match result {
                Ok(result) => self.discovery = Some(result),
                Err(error) => self.fail_discovery(error),
            }
        }
    }

    pub fn poll(&mut self) {
        self.poll_discovery(Instant::now());
        let Some(job) = &self.diagnostic else { return };
        let Some(result) = job.task.poll() else { return };
        let current = job.endpoint == self.draft.endpoint && match &job.diagnostic {
            Diagnostic::Models => true,
            Diagnostic::ConnectionTest { model } => self.draft.model.as_ref() == Some(model),
        };
        self.diagnostic = None;
        if !current { return; }
        match result {
            Ok(DiagnosticResult::Models(models)) => {
                if self.draft.model.as_ref().is_some_and(|model| !models.contains(model)) { self.draft.model = None; }
                self.models = models;
                self.catalogue_endpoint = Some(self.draft.endpoint);
                self.error = None;
            }
            Ok(DiagnosticResult::Answer(answer)) => { self.answer = Some(answer); self.error = None; }
            Err(error) => self.fail(error.to_string()),
        }
    }
}

/// Called before drawing any Settings page. Leaving the page or changing its
/// profile cancels requests and discards the draft/response. A blocked discovery
/// lookup keeps its process-wide permit until it actually returns.
pub fn reconcile(slot: &mut Option<SettingsDraft>, lib: &Library, space: &str, visible: bool) {
    let active = crate::downloads::active_profile(lib, space);
    if !visible || slot.as_ref().is_some_and(|state| Some(state.profile.as_str()) != active) { *slot = None; }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossbow_core::store::Store;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::time::{Duration, Instant};

    pub(crate) struct Server {
        pub(crate) endpoint: LocalEndpoint,
        request: Receiver<String>,
        pub(crate) release: mpsc::Sender<()>,
        handle: std::thread::JoinHandle<()>,
    }

    impl Server {
        fn start(status: &str, body: &str) -> Self { Self::sequence(&[(status, body)]) }

        pub(crate) fn sequence(replies: &[(&str, &str)]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = LocalEndpoint { port: listener.local_addr().unwrap().port(), ..Default::default() };
            let responses: Vec<_> = replies.iter().map(|(status, body)| format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())).collect();
            let (sender, request) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let handle = std::thread::spawn(move || {
                for response in responses {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    let mut socket = loop {
                        match listener.accept() {
                            Ok((socket, _)) => break socket,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                            Err(error) => panic!("fixture accept deadline: {error}"),
                        }
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    socket.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                    let mut bytes = Vec::new();
                    let mut buf = [0; 4096];
                    loop {
                        let count = socket.read(&mut buf).unwrap();
                        assert!(count > 0 && bytes.len() < 16_384);
                        bytes.extend_from_slice(&buf[..count]);
                        let text = String::from_utf8_lossy(&bytes);
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let length = head.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|n| n.parse::<usize>().ok())).unwrap_or(0);
                            if body.len() >= length { break; }
                        }
                    }
                    sender.send(String::from_utf8(bytes).unwrap()).unwrap();
                    wait.recv_timeout(Duration::from_secs(3)).unwrap();
                    let _ = socket.write_all(response.as_bytes());
                }
            });
            Self { endpoint, request, release, handle }
        }

        pub(crate) fn received(&self) -> String { self.request.recv_timeout(Duration::from_secs(3)).unwrap() }
        pub(crate) fn finish(self) { self.release.send(()).unwrap(); self.join(); }
        pub(crate) fn join(self) { self.handle.join().unwrap(); }
    }

    fn settle(state: &mut SettingsDraft) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.busy() {
            state.poll();
            assert!(Instant::now() < deadline, "diagnostic did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn library(path: &Path) -> Library {
        let mut lib = Library::open(Store::open(path).unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        lib
    }

    pub(crate) struct BlockedScan {
        gate: Arc<AtomicBool>,
        control: Arc<DiscoveryControl>,
        release: Option<mpsc::Sender<()>>,
    }

    impl BlockedScan {
        pub(crate) fn start(state: &mut SettingsDraft, ctx: &egui::Context) -> Self {
            let gate = Arc::new(AtomicBool::new(false));
            let (started, wait_started) = mpsc::channel();
            let (release, blocked) = mpsc::channel();
            state.scan_with(&gate, move |_| {
                started.send(()).unwrap();
                let _ = blocked.recv_timeout(Duration::from_secs(3));
                // Deliberately ignores cancellation, like a blocked OS lookup.
                Some(Discovery { path_limit_reached: true, ..Default::default() })
            }, ctx);
            wait_started.recv_timeout(Duration::from_secs(3)).unwrap();
            let control = Arc::clone(&state.discovery_task.as_ref().unwrap().control);
            Self { gate, control, release: Some(release) }
        }

        pub(crate) fn retry(&self, state: &mut SettingsDraft, ctx: &egui::Context) {
            state.scan_with(&self.gate, |_| Some(Discovery::default()), ctx);
        }

        pub(crate) fn expire(&self, state: &mut SettingsDraft) { state.poll_discovery(self.control.deadline); }

        pub(crate) fn release(&mut self) {
            if let Some(release) = self.release.take() { let _ = release.send(()); }
            let deadline = Instant::now() + Duration::from_secs(3);
            while self.gate.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "discovery worker did not release its permit");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for BlockedScan {
        fn drop(&mut self) { if let Some(release) = self.release.take() { let _ = release.send(()); } }
    }

    pub(crate) fn settle_scan(state: &mut SettingsDraft) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.discovering() {
            state.poll();
            assert!(Instant::now() < deadline, "discovery did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn discovery_workers_remain_bounded_across_draft_replacement() {
        for change in ["cancel", "reload", "page", "profile"] {
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let profile = lib.default_profile_id();
            let space = lib.spaces.iter().find(|(_, space)| space.profile_id == profile).unwrap().0.clone();
            let ctx = egui::Context::default();
            let mut slot = Some(SettingsDraft::load(&lib, &profile));
            let mut blocked = BlockedScan::start(slot.as_mut().unwrap(), &ctx);
            match change {
                "cancel" => slot.as_mut().unwrap().cancel_scan(),
                "reload" => slot.as_mut().unwrap().reload(&lib),
                "page" => reconcile(&mut slot, &lib, &space, false),
                "profile" => {
                    let other = lib.create_profile("B").unwrap();
                    lib.spaces.get_mut(&space).unwrap().profile_id = other;
                    reconcile(&mut slot, &lib, &space, true);
                }
                _ => unreachable!(),
            }
            assert!(blocked.control.cancelled.load(Ordering::Acquire), "{change}");
            for _ in 0..8 {
                let active = crate::downloads::active_profile(&lib, &space).unwrap();
                let mut replacement = SettingsDraft::load(&lib, active);
                blocked.retry(&mut replacement, &ctx);
                assert!(!replacement.discovering(), "{change}: a replacement worker started behind the blocked lookup");
                assert!(replacement.discovery_error.as_deref().unwrap().contains("still finishing"));
                assert!(replacement.discovery.is_none());
                slot = Some(replacement);
            }
            blocked.release();
            let state = slot.as_mut().unwrap();
            state.poll();
            assert!(state.discovery.is_none(), "late result entered the replacement draft");
            blocked.retry(state, &ctx);
            settle_scan(state);
            assert_eq!(state.discovery, Some(Discovery::default()));
            assert!(state.discovery_error.is_none());
        }
    }

    #[test]
    fn discovery_timeout_does_not_free_the_worker_or_accept_its_late_result() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut state = SettingsDraft::load(&lib, &lib.default_profile_id());
        let ctx = egui::Context::default();
        let mut blocked = BlockedScan::start(&mut state, &ctx);
        blocked.expire(&mut state);
        assert!(!state.discovering());
        assert!(state.discovery_error.as_deref().unwrap().contains("timed out"));
        assert!(state.reveal_discovery_error && blocked.gate.load(Ordering::Acquire));
        blocked.retry(&mut state, &ctx);
        assert!(!state.discovering());
        assert!(state.discovery_error.as_deref().unwrap().contains("still finishing"));
        blocked.release();
        state.poll();
        assert!(state.discovery.is_none());
        blocked.retry(&mut state, &ctx);
        settle_scan(&mut state);
        assert_eq!(state.discovery, Some(Discovery::default()));
        assert!(state.discovery_error.is_none());
    }

    #[test]
    fn discovery_deadline_wake_survives_an_earlier_repaint() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut state = SettingsDraft::load(&lib, &lib.default_profile_id());
        let ctx = egui::Context::default();
        let mut blocked = BlockedScan::start(&mut state, &ctx);
        let deadline = blocked.control.deadline;
        let start = deadline - DISCOVERY_TIMEOUT;
        let mut now = start + Duration::from_secs(1);
        let mut delayed_wakes = 0;
        // Consume the original wake with an earlier input/animation repaint.
        // Thereafter this scheduler advances only to egui's requested next frame.
        ctx.request_repaint();
        for _ in 0..16 {
            let mut output = ctx.run_ui(egui::RawInput { time: Some(now.duration_since(start).as_secs_f64()), ..Default::default() }, |_| {
                state.poll_discovery(now);
            });
            let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
            if !state.discovering() { break; }
            assert!(now < deadline);
            assert!(delay <= deadline.duration_since(now), "pending scan lost its deadline wake: {delay:?}");
            if delay.is_zero() { now += Duration::from_millis(16); }
            else { delayed_wakes += 1; now += delay; }
        }
        assert!(delayed_wakes > 0, "timeout must not depend on continuous input/repainting");
        assert!(!state.discovering());
        assert!(state.discovery_error.as_deref().unwrap().contains("timed out"));
        assert!(now <= deadline + Duration::from_millis(16));
        assert!(blocked.gate.load(Ordering::Acquire), "UI timeout released the blocked worker");
        blocked.release();
        state.poll();
        assert!(state.discovery.is_none(), "late result entered the timed-out draft");
        let mut final_delay = Duration::ZERO;
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |_| state.poll_discovery(now));
            final_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
        }
        assert_eq!(final_delay, Duration::MAX, "finished scan kept scheduling wakes");
    }

    #[test]
    fn discovery_completion_and_stopped_workers_release_their_permit() {
        let root = tempfile::tempdir().unwrap();
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let mut state = SettingsDraft::load(&lib, &lib.default_profile_id());
        let ctx = egui::Context::default();
        let gate = Arc::new(AtomicBool::new(false));
        let roots = DiscoveryRoots { binaries: vec![root.path().into()], applications: vec![] };
        state.scan_with(&gate, move |control| providers::discover_cancellable(&roots, || control.stopped()), &ctx);
        settle_scan(&mut state);
        assert_eq!(state.discovery, Some(Discovery::default()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while gate.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        state.scan_with(&gate, |_| None, &ctx);
        settle_scan(&mut state);
        assert!(state.discovery.is_none());
        assert!(state.discovery_error.as_deref().unwrap().contains("scan stopped"));
        while gate.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn selection_is_device_local_profile_bound_and_persists_on_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut lib = library(&path);
        let a = lib.default_profile_id();
        let b = lib.create_profile("B").unwrap();
        let before = lib.store.all_entities().unwrap();
        let mut draft = SettingsDraft::load(&lib, &a);
        draft.set_provider(LocalProvider::LmStudio);
        draft.set_model(Some("fixture-model".into()));
        draft.save(&mut lib, Some(&a));
        assert!(draft.error.is_none());
        assert!(!draft.changed());
        assert_eq!(SettingsDraft::load(&lib, &b).draft, Preferences::default());
        assert_eq!(lib.store.all_entities().unwrap(), before);
        drop(lib);
        let lib = library(&path);
        let reopened = SettingsDraft::load(&lib, &a);
        assert_eq!(reopened.draft, draft.draft);
        assert!(reopened.models.is_empty());
        assert!(!reopened.can_test());
        assert!(!reopened.busy());
    }

    #[test]
    fn save_conflict_requires_reload_instead_of_overwriting_another_writer() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut lib = library(&path);
        let profile = lib.default_profile_id();
        let mut first = SettingsDraft::load(&lib, &profile);
        let mut second = SettingsDraft::load(&lib, &profile);
        first.set_provider(LocalProvider::LmStudio);
        first.save(&mut lib, Some(&profile));
        second.set_provider(LocalProvider::LlamaCpp);
        second.save(&mut lib, Some(&profile));
        assert!(second.error.as_deref().unwrap().contains("changed elsewhere"));
        assert_eq!(SettingsDraft::load(&lib, &profile).draft, first.draft);
        second.reload(&lib);
        assert_eq!(second.draft, first.draft);
        // Existing-row compare/write remains atomic across SQLite connections.
        let other = Store::open(&path).unwrap();
        other.kv_set(&key(&profile), &serde_json::to_string(&Preferences::default()).unwrap()).unwrap();
        second.set_provider(LocalProvider::LlamaCpp);
        second.save(&mut lib, Some(&profile));
        assert!(second.error.as_deref().unwrap().contains("changed elsewhere"));
        assert_eq!(SettingsDraft::load(&lib, &profile).draft, Preferences::default());
    }

    #[test]
    fn write_failure_retains_draft_and_retry_commits_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut lib = library(&path);
        let profile = lib.default_profile_id();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_ai BEFORE INSERT ON kv WHEN NEW.k LIKE 'ai.providers.%' BEGIN SELECT RAISE(ABORT, 'fixture'); END;").unwrap();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.set_provider(LocalProvider::LmStudio);
        let draft = state.draft.clone();
        state.save(&mut lib, Some(&profile));
        assert!(state.error.is_some());
        assert_eq!(state.draft, draft);
        assert!(lib.store.kv_get(&key(&profile)).unwrap().is_none());
        conn.execute_batch("DROP TRIGGER reject_ai").unwrap();
        state.save(&mut lib, Some(&profile));
        assert!(state.error.is_none());
        assert_eq!(state.saved, Some(draft));
        assert!(!state.changed());
    }

    #[test]
    fn wrong_or_removed_profile_cannot_save_or_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut lib = library(&path);
        let profile = lib.default_profile_id();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.set_provider(LocalProvider::LmStudio);
        state.save(&mut lib, Some("other"));
        state.start(&mut lib, Some("other"), Diagnostic::Models, &egui::Context::default());
        assert!(!state.busy());
        assert!(lib.store.kv_get(&key(&profile)).unwrap().is_none());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("UPDATE entities SET deleted = 1 WHERE kind = 'profile' AND id = ?1", [&profile]).unwrap();
        state.save(&mut lib, Some(&profile));
        state.start(&mut lib, Some(&profile), Diagnostic::Models, &egui::Context::default());
        assert!(!state.busy());
        assert!(lib.store.kv_get(&key(&profile)).unwrap().is_none());
    }

    #[test]
    fn invalid_saved_data_is_visible_and_can_be_explicitly_replaced() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        lib.store.kv_set(&key(&profile), r#"{"endpoint":{"provider":"ollama","port":0},"model":null}"#).unwrap();
        let mut state = SettingsDraft::load(&lib, &profile);
        assert!(state.error.as_deref().unwrap().contains("invalid"));
        assert!(state.changed());
        state.save(&mut lib, Some(&profile));
        assert!(state.error.is_none());
        assert_eq!(SettingsDraft::load(&lib, &profile).saved, Some(Preferences::default()));
    }

    #[test]
    fn changing_endpoint_clears_models_answers_and_diagnostic_eligibility() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.models = vec!["fixture-model".into()];
        state.catalogue_endpoint = Some(state.draft.endpoint);
        state.set_model(Some("fixture-model".into()));
        state.answer = Some("old answer".into());
        assert!(state.can_test());
        state.set_endpoint(LocalEndpoint { port: 8081, ..state.draft.endpoint });
        assert!(!state.can_test());
        assert!(state.models.is_empty());
        assert!(state.answer.is_none());
        assert!(state.draft.model.is_none());
        state.start(&mut lib, Some(&profile), Diagnostic::ConnectionTest { model: "fixture-model".into() }, &egui::Context::default());
        assert!(!state.busy());
        assert!(state.error.as_deref().unwrap().contains("Refresh models"));
    }

    #[test]
    fn real_diagnostic_dispatch_refresh_retry_test_and_saved_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("library.sqlite");
        let mut lib = library(&path);
        let profile = lib.default_profile_id();
        let server = Server::sequence(&[
            ("200 OK", r#"{"data":[{"id":"fixture-model"}]}"#),
            ("200 OK", r#"{"choices":[{"message":{"content":"Crossbow connection ready."},"finish_reason":"stop"}]}"#),
            ("503 Unavailable", "untrusted server error"),
            ("200 OK", r#"{"data":[{"id":"fixture-model"}]}"#),
        ]);
        let ctx = egui::Context::default();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.set_endpoint(server.endpoint);
        state.start(&mut lib, Some(&profile), Diagnostic::Models, &ctx);
        let request = server.received();
        assert!(request.starts_with("GET /v1/models "));
        assert!(!request.to_lowercase().contains("authorization"));
        server.release.send(()).unwrap();
        settle(&mut state);
        assert!(state.catalogue_loaded());
        assert_eq!(state.models, ["fixture-model"]);
        assert!(state.draft.model.is_none());
        assert!(!state.can_test());
        state.set_model(Some("fixture-model".into()));
        assert!(state.can_test());
        state.start(&mut lib, Some(&profile), Diagnostic::ConnectionTest { model: "fixture-model".into() }, &ctx);
        let request = server.received();
        assert!(request.starts_with("POST /v1/chat/completions "));
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body, serde_json::json!({"model":"fixture-model","messages":[{"role":"user","content":crossbow_ai::local::TEST_PROMPT}],"stream":false,"max_tokens":32}));
        server.release.send(()).unwrap();
        settle(&mut state);
        assert_eq!(state.answer.as_deref(), Some("Crossbow connection ready."));
        state.save(&mut lib, Some(&profile));
        assert!(state.error.is_none());
        assert!(!state.changed());
        state.start(&mut lib, Some(&profile), Diagnostic::Models, &ctx);
        assert!(state.models.is_empty());
        assert!(state.answer.is_none());
        assert!(!state.can_test());
        assert!(server.received().starts_with("GET /v1/models "));
        server.release.send(()).unwrap();
        settle(&mut state);
        assert!(state.error.as_deref().unwrap().contains("503"));
        assert!(!state.error.as_deref().unwrap().contains("untrusted"));
        assert!(!state.catalogue_loaded());
        state.start(&mut lib, Some(&profile), Diagnostic::Models, &ctx);
        assert!(server.received().starts_with("GET /v1/models "));
        server.finish();
        settle(&mut state);
        assert!(state.can_test());
        assert!(state.error.is_none());
        drop(lib);
        let lib = library(&path);
        let reopened = SettingsDraft::load(&lib, &profile);
        assert_eq!(reopened.draft, state.draft);
        assert!(reopened.models.is_empty() && reopened.answer.is_none());
        assert!(!reopened.can_test() && !reopened.busy());
    }

    #[test]
    fn cancellation_endpoint_model_reload_and_page_changes_discard_inflight_results() {
        for change in ["cancel", "endpoint", "model", "reload", "page", "profile"] {
            let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
            lib.ensure_defaults().unwrap();
            let profile = lib.default_profile_id();
            let space = lib.spaces.iter().find(|(_, space)| space.profile_id == profile).unwrap().0.clone();
            let server = Server::start("200 OK", r#"{"data":[{"id":"stale-model"}]}"#);
            let mut slot = Some(SettingsDraft::load(&lib, &profile));
            let state = slot.as_mut().unwrap();
            state.set_endpoint(server.endpoint);
            state.start(&mut lib, Some(&profile), Diagnostic::Models, &egui::Context::default());
            server.received();
            assert!(state.busy());
            match change {
                "cancel" => state.cancel_request(),
                "endpoint" => state.set_endpoint(LocalEndpoint { port: 1, ..server.endpoint }),
                "model" => state.set_model(Some("new-model".into())),
                "reload" => state.reload(&lib),
                "page" => reconcile(&mut slot, &lib, &space, false),
                "profile" => {
                    lib.spaces.get_mut(&space).unwrap().profile_id = "different".into();
                    reconcile(&mut slot, &lib, &space, true);
                }
                _ => unreachable!(),
            }
            server.finish();
            if let Some(state) = &mut slot {
                state.poll();
                assert!(!state.busy() && !state.catalogue_loaded(), "{change}");
                assert!(state.models.is_empty() && state.answer.is_none(), "{change}");
            } else { assert!(matches!(change, "page" | "profile")); }
        }
    }

    #[test]
    fn save_load_and_unapproved_test_do_not_contact_the_endpoint() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut state = SettingsDraft::load(&lib, &profile);
        state.set_endpoint(LocalEndpoint { port: listener.local_addr().unwrap().port(), ..Default::default() });
        state.set_model(Some("not-refreshed".into()));
        state.save(&mut lib, Some(&profile));
        assert!(state.error.is_none());
        state.reload(&lib);
        state.start(&mut lib, Some(&profile), Diagnostic::ConnectionTest { model: "not-refreshed".into() }, &egui::Context::default());
        assert!(!state.busy());
        assert!(state.error.as_deref().unwrap().contains("Refresh models"));
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn leaving_page_or_switching_profile_discards_ephemeral_state() {
        let mut lib = Library::open(Store::open_in_memory().unwrap()).unwrap();
        lib.ensure_defaults().unwrap();
        let profile = lib.default_profile_id();
        let space = lib.spaces.iter().find(|(_, space)| space.profile_id == profile).unwrap().0.clone();
        let mut slot = Some(SettingsDraft::load(&lib, &profile));
        reconcile(&mut slot, &lib, &space, true);
        assert!(slot.is_some());
        reconcile(&mut slot, &lib, &space, false);
        assert!(slot.is_none());
        slot = Some(SettingsDraft::load(&lib, &profile));
        lib.spaces.get_mut(&space).unwrap().profile_id = "missing".into();
        reconcile(&mut slot, &lib, &space, true);
        assert!(slot.is_none());
    }
}
