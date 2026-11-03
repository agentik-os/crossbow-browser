//! Opt-in, bounded local timing. No URLs, input text, account IDs or credentials.
//! Samples are buffered until exit so disk writes cannot stall measured frames.

use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CAPACITY: usize = 10_000;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Browser,
    CommandBar,
    Settings,
    Onboarding,
}

#[derive(Serialize)]
struct Sample {
    frame: u64,
    pass: u64,
    since_start_ms: f64,
    ui_ms: f64,
    cef_pump_ms: f64,
    settings_ms: f64,
    previous_frame_cpu_ms: Option<f64>,
    surface: Surface,
    remaining_input_events: usize,
    focused: bool,
    width_points: f32,
    height_points: f32,
    pixels_per_point: f32,
    repaint_requested: bool,
}

pub struct FrameTrace {
    path: PathBuf,
    start: Instant,
    samples: Vec<Sample>,
    dropped: usize,
}

impl FrameTrace {
    pub fn from_env(data_dir: &Path) -> Option<Self> {
        if std::env::var("CROSSBOW_FRAME_TRACE").as_deref() != Ok("1") {
            return None;
        }
        Some(Self::new(Self::session_path(data_dir)))
    }

    fn session_path(data_dir: &Path) -> PathBuf {
        data_dir.join(format!("frame-trace-{}-{}.json", std::process::id(), crossbow_core::new_id()))
    }

    fn new(path: PathBuf) -> Self {
        Self { path, start: Instant::now(), samples: Vec::with_capacity(CAPACITY), dropped: 0 }
    }

    pub fn record(&mut self, ctx: &egui::Context, started: Instant, cef_pump_ms: f64, settings_ms: f64, previous_cpu: Option<f32>, surface: Surface) {
        if self.samples.len() == CAPACITY {
            self.dropped += 1;
            return;
        }
        let ui_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (remaining_input_events, focused) = ctx.input(|i| (i.events.len(), i.focused));
        self.samples.push(Sample {
            frame: ctx.cumulative_frame_nr(),
            pass: ctx.cumulative_pass_nr(),
            since_start_ms: started.duration_since(self.start).as_secs_f64() * 1000.0,
            ui_ms,
            cef_pump_ms,
            settings_ms,
            previous_frame_cpu_ms: previous_cpu.map(|s| s as f64 * 1000.0),
            surface,
            remaining_input_events,
            focused,
            width_points: ctx.content_rect().width(),
            height_points: ctx.content_rect().height(),
            pixels_per_point: ctx.pixels_per_point(),
            repaint_requested: ctx.has_requested_repaint(),
        });
    }

    pub fn save(&self) -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = std::io::BufWriter::new(options.open(&self.path)?);
        serde_json::to_writer_pretty(&mut output, &serde_json::json!({
            "schema": 1,
            "kind": "local_ui_cpu_diagnostic_not_presentation_latency",
            "version": env!("CARGO_PKG_VERSION"),
            "debug_assertions": cfg!(debug_assertions),
            "capacity": CAPACITY,
            "dropped_samples": self.dropped,
            "samples": self.samples,
        }))?;
        output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_process_id_still_gets_a_new_session_path() {
        let dir = Path::new("isolated-traces");
        let first = FrameTrace::session_path(dir);
        let second = FrameTrace::session_path(dir);
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir));
        assert!(first.file_name().unwrap().to_str().unwrap().starts_with(&format!("frame-trace-{}-", std::process::id())));
    }

    #[test]
    fn timing_buffer_is_bounded_and_reports_dropped_samples() {
        let ctx = egui::Context::default();
        let mut trace = FrameTrace::new(PathBuf::new());
        for _ in 0..CAPACITY + 3 {
            trace.record(&ctx, Instant::now(), 0.0, 0.0, None, Surface::Browser);
        }
        assert_eq!(trace.samples.len(), CAPACITY);
        assert_eq!(trace.dropped, 3);
    }

    #[test]
    fn timing_output_is_private_content_free_and_never_overwrites() {
        let path = std::env::temp_dir().join(format!("crossbow-frame-trace-test-{}.json", crossbow_core::new_id()));
        let mut trace = FrameTrace::new(path.clone());
        trace.record(&egui::Context::default(), Instant::now(), 1.0, 2.0, Some(0.003), Surface::Settings);
        trace.save().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["samples"][0]["surface"], "settings");
        assert_eq!(value["samples"][0]["settings_ms"], 2.0);
        assert!(!String::from_utf8_lossy(&bytes).contains("https://"));
        assert_eq!(trace.save().unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_file(path).unwrap();
    }
}
