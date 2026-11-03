//! CEF work belongs to eframe's root lifecycle, including ticks with no UI pass.

use std::time::Instant;

pub(super) struct EnginePump<P = fn()> {
    pump: P,
    timing: Option<(Instant, f64)>,
}

impl Default for EnginePump {
    fn default() -> Self {
        Self { pump: crossbow_engine::pump, timing: None }
    }
}

impl<P: FnMut()> EnginePump<P> {
    pub(super) fn on_logic(&mut self, trace: bool) {
        // eframe calls this on the UI thread before each UI pass, or alone while
        // hidden. Pump once here; do not require a new pass number or self-wake.
        let started = trace.then(Instant::now);
        (self.pump)();
        self.timing = started.map(|start| (start, start.elapsed().as_secs_f64() * 1000.0));
    }

    pub(super) fn on_ui(&mut self) -> Option<(Instant, f64)> {
        self.timing.take()
    }
}

// Use the engine's real coalescer, not a second model of its acknowledgement rules.
#[cfg(test)]
#[path = "../../crates/crossbow-engine/src/repaint.rs"]
mod repaint;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use egui::{Context, RawInput, ViewportId};

    const WAIT: Duration = Duration::from_secs(2);

    fn input(minimized: bool, occluded: bool) -> RawInput {
        let mut input = RawInput::default();
        let viewport = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
        viewport.minimized = Some(minimized);
        viewport.occluded = Some(occluded);
        input
    }

    fn with_pump<P: FnMut()>(pump: P) -> EnginePump<P> {
        EnginePump { pump, timing: None }
    }

    #[test]
    fn visible_logic_pumps_before_ui_once_and_retains_trace_timing() {
        let ctx = Context::default();
        let caller = std::thread::current().id();
        let calls = Cell::new(0);
        let mut pump = with_pump(|| {
            assert_eq!(std::thread::current().id(), caller);
            calls.set(calls.get() + 1);
        });
        let mut passes = 0;
        let mut output = ctx.run_ui(input(false, false), |ui| {
            passes += 1;
            pump.on_logic(true);
            assert_eq!(calls.get(), passes, "logic must service CEF before drawing");
            let (started, elapsed) = pump.on_ui().expect("trace for this logic/UI pair");
            assert!(started <= Instant::now());
            assert!(elapsed >= 0.0);
            assert_eq!(calls.get(), passes, "UI must not pump a second time");
            assert!(pump.on_ui().is_none(), "timing is consumed once");
            assert_eq!(calls.get(), passes);
            if passes == 1 { ui.ctx().request_discard("pump multipass regression"); }
        });
        output.textures_delta.clear();
        assert_eq!(passes, 2);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn logic_only_pumps_with_minimized_occluded_or_visible_viewport() {
        for raw in [input(true, false), input(false, true), input(false, false), RawInput::default()] {
            let ctx = Context::default();
            let caller = std::thread::current().id();
            let calls = Cell::new(0);
            let mut pump = with_pump(|| {
                assert_eq!(std::thread::current().id(), caller);
                calls.set(calls.get() + 1);
            });
            let pass = ctx.cumulative_pass_nr();
            for expected in 1..=3 {
                let _ = ctx.run_logic(&raw, |_| pump.on_logic(false));
                assert_eq!(calls.get(), expected, "no UI or new pass is needed to pump");
                assert_eq!(ctx.cumulative_pass_nr(), pass);
                assert!(pump.timing.is_none());
            }
        }
    }

    #[test]
    fn hidden_to_visible_transition_replaces_timing_without_skipping_or_double_pumping() {
        let ctx = Context::default();
        let calls = Cell::new(0);
        let mut pump = with_pump(|| calls.set(calls.get() + 1));
        let _ = ctx.run_logic(&input(false, true), |_| pump.on_logic(true));
        let hidden_start = pump.timing.expect("hidden logic still measures its own pump").0;
        assert_eq!(calls.get(), 1);
        let mut output = ctx.run_ui(input(false, false), |_| {
            pump.on_logic(true);
            let visible_start = pump.on_ui().unwrap().0;
            assert!(visible_start >= hidden_start);
            assert_eq!(calls.get(), 2);
        });
        output.textures_delta.clear();
        let _ = ctx.run_logic(&input(true, false), |_| pump.on_logic(false));
        assert_eq!(calls.get(), 3);
        assert!(pump.timing.is_none(), "an untraced tick must not retain stale timing");
    }

    #[test]
    fn logic_only_preserves_pending_post_paint_texture_retirement() {
        let ctx = Context::default();
        let retirement = crate::texture_retirement::TextureRetirement::new(&ctx);
        let id = egui::TextureId::User(901);
        let calls = Cell::new(0);
        let mut pump = with_pump(|| calls.set(calls.get() + 1));
        retirement.retire(id);
        let _ = ctx.run_logic(&input(true, false), |_| pump.on_logic(false));
        let mut output = ctx.run_ui(input(false, false), |ui| {
            pump.on_logic(false);
            assert!(pump.on_ui().is_none());
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0));
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            ui.painter().image(id, rect, uv, egui::Color32::WHITE);
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(output.textures_delta.free, [id].into_iter().collect(), "hidden logic must not drain the post-paint queue");
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
        let mut next = ctx.run_ui(input(false, false), |_| {
            pump.on_logic(false);
            assert!(pump.on_ui().is_none());
        });
        assert!(next.textures_delta.free.is_empty());
        next.textures_delta.clear();
    }

    #[test]
    fn hidden_logic_acknowledges_coalesced_wakes_and_services_next_cef_timer() {
        let ctx = Context::default();
        let (tx, rx) = mpsc::channel();
        let wake_ctx = ctx.clone();
        let waker = repaint::RepaintWaker::new(move || {
            wake_ctx.request_repaint_of(ViewportId::ROOT);
            let _ = tx.send(std::thread::current().id());
        });
        let caller = std::thread::current().id();
        let calls = Cell::new(0);
        let mut pump = with_pump(|| {
            assert_eq!(std::thread::current().id(), caller);
            // This is the production engine::pump order: ack, then CEF work,
            // which may schedule its next timer from inside the callback.
            waker.begin_pump();
            calls.set(calls.get() + 1);
            if calls.get() < 3 { waker.schedule_pump(Duration::ZERO); }
        });
        waker.schedule_pump(Duration::ZERO);
        for expected in 1..=3 {
            assert_ne!(rx.recv_timeout(WAIT).expect("due CEF work wakes root logic"), caller);
            for _ in 0..100 { waker.wake(Duration::ZERO); }
            assert!(rx.try_recv().is_err(), "one dispatch must await acknowledgement");
            let _ = ctx.run_logic(&input(false, true), |_| pump.on_logic(false));
            assert_eq!(calls.get(), expected);
        }
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err(), "no periodic self-wake after CEF work is drained");
    }

    #[test]
    fn hidden_logic_acknowledges_while_repaint_sink_is_still_in_flight() {
        let ctx = Context::default();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = Cell::new(true);
        let waker = Arc::new(repaint::RepaintWaker::new(move || {
            let _ = started_tx.send(());
            if first.replace(false) { let _ = release_rx.recv_timeout(WAIT); }
        }));
        let calls = Cell::new(0);
        let mut pump = with_pump(|| {
            waker.begin_pump();
            calls.set(calls.get() + 1);
            waker.schedule_pump(Duration::ZERO);
        });
        waker.wake(Duration::ZERO);
        started_rx.recv_timeout(WAIT).unwrap();
        let _ = ctx.run_logic(&input(true, false), |_| pump.on_logic(false));
        release_tx.send(()).unwrap();
        assert_eq!(calls.get(), 1, "logic must acknowledge without waiting for the repaint sink");
        started_rx.recv_timeout(WAIT).expect("the timer queued during CEF work must survive the in-flight sink");
        waker.stop();
        assert!(started_rx.try_recv().is_err());
    }
}
