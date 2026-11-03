//! Small animation toolkit: eased tweens and critically damped springs whose
//! state lives in egui's per-id memory.

use egui::{Context, Id};
use std::sync::atomic::{AtomicBool, Ordering};

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Settings → "Smooth animations" off: every tween and spring jumps to its end.
pub fn set_reduced_motion(on: bool) {
    REDUCED_MOTION.store(on, Ordering::Relaxed);
}

fn reduced() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Overshooting ease for "pop" entrances.
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

#[derive(Clone, Copy)]
struct Spring {
    value: f32,
    velocity: f32,
    last: f64,
}

/// Critically damped spring towards `target` (stiffness in 1/s²).
/// Returns the current value and keeps repainting until it settles.
pub fn spring(ctx: &Context, id: Id, target: f32, stiffness: f32) -> f32 {
    if reduced() {
        return target;
    }
    let now = ctx.input(|i| i.time);
    let mut s = ctx.data(|d| d.get_temp::<SpringState>(id)).map(|s| s.0).unwrap_or(Spring {
        value: target,
        velocity: 0.0,
        last: now,
    });
    let dt = ((now - s.last) as f32).clamp(0.0, 1.0 / 20.0);
    s.last = now;
    let omega = stiffness.sqrt();
    // Exact solution of the critically damped spring. Unlike Euler integration,
    // this stays stable when a busy frame takes longer than the spring period.
    let displacement = s.value - target;
    let b = s.velocity + omega * displacement;
    let decay = (-omega * dt).exp();
    s.value = target + (displacement + b * dt) * decay;
    s.velocity = (s.velocity - omega * b * dt) * decay;
    if (s.value - target).abs() < 0.001 && s.velocity.abs() < 0.01 {
        s.value = target;
        s.velocity = 0.0;
    } else {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, SpringState(s)));
    s.value
}

#[derive(Clone, Copy)]
struct SpringState(Spring);

/// Jump a spring to a value without animating.
pub fn spring_set(ctx: &Context, id: Id, value: f32) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(id, SpringState(Spring { value, velocity: 0.0, last: now })));
}

#[derive(Clone, Copy)]
struct Transition {
    from: f32,
    target: f32,
    started: f64,
    duration: f32,
}

impl Transition {
    fn value(self, now: f64) -> f32 {
        let t = ((now - self.started) as f32 / self.duration.max(0.001)).clamp(0.0, 1.0);
        if t >= 1.0 { self.target } else { lerp(self.from, self.target, ease_out_strong(t)) }
    }

    fn update(&mut self, target: f32, now: f64, duration: f32, reduced: bool) -> f32 {
        if reduced || duration <= 0.0 {
            *self = Self { from: target, target, started: now, duration };
        } else if target != self.target {
            *self = Self { from: self.value(now), target, started: now, duration };
        }
        self.value(now)
    }
}

/// A bounded, interruptible transition; delayed frames do not extend its deadline.
/// Reduced motion also updates the stored endpoint, so re-enabling cannot replay it.
pub fn bounded_transition(ctx: &Context, id: Id, target: f32, duration: f32) -> f32 {
    let now = ctx.input(|i| i.time);
    let mut state = ctx.data(|d| d.get_temp::<Transition>(id))
        .unwrap_or(Transition { from: target, target, started: now, duration });
    let value = state.update(target, now, duration, reduced());
    ctx.data_mut(|d| d.insert_temp(id, state));
    if value != target { ctx.request_repaint(); }
    value
}

/// Progress 0→1 of an animation started at `start` (seconds, egui time).
pub fn progress(ctx: &Context, start: f64, duration: f32) -> f32 {
    if reduced() {
        return 1.0;
    }
    let now = ctx.input(|i| i.time);
    let t = ((now - start) as f32 / duration.max(0.001)).clamp(0.0, 1.0);
    if t < 1.0 {
        ctx.request_repaint();
    }
    t
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// CSS-style `cubic-bezier(x1, y1, x2, y2)` evaluated at `t`.
pub fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let (cx, cy) = (3.0 * x1, 3.0 * y1);
    let (bx, by) = (3.0 * (x2 - x1) - cx, 3.0 * (y2 - y1) - cy);
    let (ax, ay) = (1.0 - cx - bx, 1.0 - cy - by);
    let x = |s: f32| ((ax * s + bx) * s + cx) * s;
    let dx = |s: f32| (3.0 * ax * s + 2.0 * bx) * s + cx;
    let mut s = t;
    for _ in 0..8 {
        let err = x(s) - t;
        let d = dx(s);
        if err.abs() < 1e-4 || d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    ((ay * s + by) * s + cy) * s
}

/// The house curve for entrances and UI responses: fast start, long settle.
pub fn ease_out_strong(t: f32) -> f32 {
    bezier(0.23, 1.0, 0.32, 1.0, t)
}

/// For things already on screen that move from A to B.
pub fn ease_in_out_strong(t: f32) -> f32 {
    bezier(0.77, 0.0, 0.175, 1.0, t)
}

pub fn reduced_motion() -> bool {
    reduced()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_stiffness_springs_stay_bounded_on_slow_frames() {
        for stiffness in [500.0, 600.0, 700.0, 800.0, 900.0] {
            let ctx = Context::default();
            let id = Id::unique("slow-frame-spring");
            let mut previous = 0.0;
            ctx.run_ui(egui::RawInput { time: Some(0.0), ..Default::default() }, |ui| {
                spring_set(ui.ctx(), id, 0.0);
            }).textures_delta.clear();
            for frame in 1..=40 {
                ctx.run_ui(egui::RawInput { time: Some(frame as f64 * 0.05), ..Default::default() }, |ui| {
                    let value = spring(ui.ctx(), id, 1.0, stiffness);
                    assert!(value.is_finite() && (previous..=1.0).contains(&value), "{stiffness}: {previous} → {value}");
                    previous = value;
                }).textures_delta.clear();
            }
            assert_eq!(previous, 1.0);
        }
    }

    #[test]
    fn bounded_transition_reverses_without_jumping_or_restarting_on_another_pass() {
        let mut state = Transition { from: 0.0, target: 0.0, started: 0.0, duration: 0.22 };
        assert_eq!(state.update(1.0, 1.0, 0.22, false), 0.0);
        let before = state.update(1.0, 1.08, 0.22, false);
        assert!(before > 0.0 && before < 1.0);
        assert_eq!(state.update(0.0, 1.08, 0.15, false), before);
        let falling = state.update(0.0, 1.12, 0.15, false);
        assert!(falling < before);
        assert_eq!(state.update(1.0, 1.12, 0.22, false), falling);
        assert_eq!(state.update(1.0, 1.12, 0.22, false), falling);
        assert_eq!(state.update(1.0, 1.34, 0.22, false), 1.0);
        assert_eq!(state.update(1.0, 40.0, 0.22, false), 1.0);
    }

    #[test]
    fn reduced_motion_synchronizes_the_transition_before_reenabling_it() {
        for duration in [0.0, 0.15, 0.22] {
            let mut state = Transition { from: 0.0, target: 1.0, started: 0.0, duration: 0.22 };
            assert_eq!(state.update(0.0, 0.08, duration, true), 0.0);
            assert_eq!(state.update(0.0, 0.1, 0.22, false), 0.0);
            assert_eq!(state.update(1.0, 0.12, 0.22, false), 0.0);
            assert_eq!(state.update(1.0, 0.34, 0.22, false), 1.0);
        }
    }

    #[test]
    fn house_curves_have_correct_endpoints_and_are_monotonic() {
        for curve in [ease_out_strong, ease_in_out_strong] {
            assert_eq!(curve(0.0), 0.0);
            assert_eq!(curve(1.0), 1.0);
            let mut previous = 0.0;
            for frame in 1..=100 {
                let value = curve(frame as f32 / 100.0);
                assert!(value >= previous && value <= 1.0);
                previous = value;
            }
        }
    }
}
