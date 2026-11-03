//! Translate native monitor and pane positions without conflating their sizes.

use crate::native_window::{DipRect, DisplaySnapshot};
use crossbow_engine::{ScreenGeometry, ScreenRect, ViewPosition};

fn rect(value: DipRect, ratio: f64) -> Option<ScreenRect> {
    if value.width <= 0.0 || value.height <= 0.0 { return None; }
    // Round shared edges once. Rounding an origin and extent independently can
    // put the work area's bottom one pixel outside the monitor at UI zoom.
    let edges = [value.x * ratio, value.y * ratio, (value.x + value.width) * ratio, (value.y + value.height) * ratio];
    if edges.into_iter().any(|v| !v.is_finite() || v.round() < f64::from(i32::MIN) || v.round() > f64::from(i32::MAX)) {
        return None;
    }
    let [left, top, right, bottom] = edges.map(|v| v.round() as i32);
    let rect = ScreenRect { x: left, y: top, width: right.checked_sub(left)?, height: bottom.checked_sub(top)? };
    rect.is_valid().then_some(rect)
}

fn dip_ratio(display: DisplaySnapshot, pixels_per_point: f32) -> Option<f64> {
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || !display.backing_scale.is_finite() || display.backing_scale <= 0.0 { return None; }
    let ratio = display.backing_scale / f64::from(pixels_per_point);
    (ratio.is_finite() && ratio > 0.0).then_some(ratio)
}

pub fn screen_geometry(display: DisplaySnapshot, pixels_per_point: f32) -> Option<ScreenGeometry> {
    // CEF's scale is egui's pixels-per-point, which may include UI zoom. Native
    // rectangles use AppKit points; backing scale alone must not double them.
    let ratio = dip_ratio(display, pixels_per_point)?;
    let screen = ScreenGeometry { bounds: rect(display.screen, ratio)?, work_area: rect(display.work_area, ratio)? };
    screen.is_valid().then_some(screen)
}

/// Root window bounds, not the individual CEF pane or full rendering NSView.
pub fn root_geometry(display: DisplaySnapshot, pixels_per_point: f32) -> Option<ScreenRect> {
    rect(display.window, dip_ratio(display, pixels_per_point)?)
}

/// `local_origin` is the individual pane's top-left in egui points relative to
/// the rendering NSView, not the monitor or full NSView's origin.
pub fn view_position(display: DisplaySnapshot, local_origin: (f32, f32), pixels_per_point: f32) -> Option<ViewPosition> {
    let ratio = dip_ratio(display, pixels_per_point)?;
    rect(display.view, ratio)?;
    let position = ViewPosition {
        x: display.view.x * ratio + f64::from(local_origin.0),
        y: display.view.y * ratio + f64::from(local_origin.1),
        // GetScreenPoint requires native DIPs on macOS, device pixels elsewhere.
        screen_scale: if cfg!(target_os = "macos") { 1.0 / ratio } else { f64::from(pixels_per_point) },
        macos_primary_top: cfg!(target_os = "macos").then_some(display.primary_top),
    };
    position.is_valid().then_some(position)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_is_not_rescaled_or_replaced_by_content_bounds() {
        let mut display = DisplaySnapshot {
            screen: DipRect { x: -1800.0, y: 0.0, width: 1800.0, height: 1169.0 },
            work_area: DipRect { x: -1800.0, y: 39.0, width: 1800.0, height: 1100.0 },
            view: DipRect { x: 0.0, y: 0.0, width: 640.0, height: 420.0 },
            window: DipRect { x: 0.0, y: 0.0, width: 640.0, height: 448.0 },
            primary_top: 982.0,
            backing_scale: 2.0,
        };
        let original = screen_geometry(display, 2.0).unwrap();
        assert_eq!(original.bounds.width, 1800);
        assert_eq!(original.bounds.x, -1800);
        for width in [420.0, 900.0, 1360.0] {
            for backing_scale in [1.0, 2.0] {
                display.view.width = width;
                display.backing_scale = backing_scale;
                assert_eq!(screen_geometry(display, backing_scale as f32), Some(original));
            }
        }
        display.backing_scale = 2.0;
        assert_eq!(screen_geometry(display, 2.5).unwrap().bounds.width, 1440);
        assert!(screen_geometry(display, 0.0).is_none());
        assert!(screen_geometry(display, f32::NAN).is_none());
    }

    #[test]
    fn rounded_shared_monitor_edges_preserve_work_area_containment() {
        let display = DisplaySnapshot {
            screen: DipRect { x: 0.0, y: 0.0, width: 1512.0, height: 982.0 },
            work_area: DipRect { x: 0.0, y: 39.0, width: 1512.0, height: 943.0 },
            view: DipRect { x: 100.0, y: 80.0, width: 1000.0, height: 800.0 },
            window: DipRect { x: 100.0, y: 52.0, width: 1000.0, height: 828.0 },
            primary_top: 982.0,
            backing_scale: 2.0,
        };
        let geometry = screen_geometry(display, 4.0).expect("round shared edges, not independent extents");
        assert_eq!(geometry.bounds, ScreenRect { x: 0, y: 0, width: 756, height: 491 });
        assert_eq!(geometry.work_area, ScreenRect { x: 0, y: 20, width: 756, height: 471 });
    }

    #[test]
    fn rounding_keeps_shared_negative_edges_and_rejects_overflow() {
        let display = DisplaySnapshot {
            screen: DipRect { x: -1512.0, y: -982.0, width: 1512.0, height: 982.0 },
            work_area: DipRect { x: -1473.0, y: -943.0, width: 1473.0, height: 943.0 },
            view: DipRect { x: -1400.0, y: -900.0, width: 1000.0, height: 800.0 },
            window: DipRect { x: -1400.0, y: -928.0, width: 1000.0, height: 828.0 },
            primary_top: 982.0,
            backing_scale: 2.0,
        };
        for ppp in [1.0, 2.0, 2.5, 3.0, 4.0] {
            let geometry = screen_geometry(display, ppp).unwrap();
            assert_eq!(geometry.bounds.x + geometry.bounds.width, 0);
            assert_eq!(geometry.bounds.y + geometry.bounds.height, 0);
            assert_eq!(geometry.work_area.x + geometry.work_area.width, 0);
            assert_eq!(geometry.work_area.y + geometry.work_area.height, 0);
        }
        assert!(rect(DipRect { x: f64::from(i32::MIN), y: 0.0, width: f64::from(u32::MAX), height: 100.0 }, 1.0).is_none());
        assert!(rect(DipRect { x: f64::from(i32::MAX), y: 0.0, width: 100.0, height: 100.0 }, 1.0).is_none());
    }

    #[test]
    fn pane_positions_add_native_view_origin_and_egui_offset_once() {
        let mut display = DisplaySnapshot {
            screen: DipRect { x: -1800.0, y: 0.0, width: 1800.0, height: 1169.0 },
            work_area: DipRect { x: -1800.0, y: 39.0, width: 1800.0, height: 1100.0 },
            view: DipRect { x: -1700.0, y: 80.0, width: 1400.0, height: 900.0 },
            window: DipRect { x: -1700.0, y: 50.0, width: 1400.0, height: 950.0 },
            primary_top: 982.0,
            backing_scale: 2.0,
        };
        assert_eq!(root_geometry(display, 2.5), Some(ScreenRect { x: -1360, y: 40, width: 1120, height: 760 }));
        for (ppp, ratio) in [(2.0, 1.0), (2.5, 0.8), (4.0, 0.5)] {
            let screen = screen_geometry(display, ppp);
            let root = root_geometry(display, ppp);
            for (x, y) in [(200.0, 48.0), (720.0, 48.0), (125.0, 80.0)] {
                let position = view_position(display, (x, y), ppp).unwrap();
                assert_eq!((position.x, position.y), (-1700.0 * ratio + f64::from(x), 80.0 * ratio + f64::from(y)));
                let native_scale = if cfg!(target_os = "macos") { 1.0 / ratio } else { f64::from(ppp) };
                assert_eq!(position.screen_scale, native_scale);
                assert_eq!(position.macos_primary_top, cfg!(target_os = "macos").then_some(982.0));
                assert_eq!(screen_geometry(display, ppp), screen);
                assert_eq!(root_geometry(display, ppp), root);
            }
        }
        display.view.x += 100.0;
        assert_eq!(view_position(display, (200.0, 48.0), 2.5).unwrap().x, -1080.0);
        for origin in [(f32::NAN, 0.0), (0.0, f32::INFINITY)] {
            assert!(view_position(display, origin, 2.0).is_none());
        }
        assert!(view_position(display, (0.0, 0.0), 0.0).is_none());
        display.view.width = 0.0;
        assert!(view_position(display, (0.0, 0.0), 2.0).is_none());
    }

    #[test]
    fn invalid_native_rectangles_fail_closed() {
        for x in [f64::NAN, f64::INFINITY, f64::from(i32::MAX) + 1.0] {
            assert!(rect(DipRect { x, y: 0.0, width: 640.0, height: 420.0 }, 1.0).is_none());
        }
        assert!(rect(DipRect { x: 0.0, y: 0.0, width: 0.1, height: 420.0 }, 1.0).is_none());
    }
}
