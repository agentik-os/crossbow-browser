//! Monitor geometry is independent of any tab's layout viewport.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl ScreenRect {
    pub fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }

    pub(crate) fn cef_rect(self) -> cef::Rect {
        cef::Rect { x: self.x, y: self.y, width: self.width, height: self.height }
    }
}

/// Top-left-origin desktop coordinates in logical pixels, not backing pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenGeometry {
    pub bounds: ScreenRect,
    pub work_area: ScreenRect,
}

impl ScreenGeometry {
    pub fn is_valid(self) -> bool {
        let bounds = self.bounds;
        let work = self.work_area;
        bounds.is_valid() && work.is_valid()
            && work.x >= bounds.x && work.y >= bounds.y
            && i64::from(work.x) + i64::from(work.width) <= i64::from(bounds.x) + i64::from(bounds.width)
            && i64::from(work.y) + i64::from(work.height) <= i64::from(bounds.y) + i64::from(bounds.height)
    }
}

/// A pane's top-left in CEF screen DIPs, separate from monitor bounds and size.
/// `screen_scale` converts those DIPs to GetScreenPoint's platform units.
/// On macOS, `macos_primary_top` flips top-left DIPs to Cocoa bottom-left DIPs;
/// Windows/Linux use device pixels and leave that field unset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPosition {
    pub x: f64,
    pub y: f64,
    pub screen_scale: f64,
    pub macos_primary_top: Option<f64>,
}

impl ViewPosition {
    pub fn is_valid(self) -> bool {
        self.screen_scale.is_finite() && self.screen_scale > 0.0
            && self.macos_primary_top.is_none_or(f64::is_finite)
            && self.cef_origin().is_some() && self.screen_point(0, 0).is_some()
    }

    pub(crate) fn cef_origin(self) -> Option<(i32, i32)> {
        Some((coordinate(self.x)?, coordinate(self.y)?))
    }

    pub(crate) fn screen_point(self, x: i32, y: i32) -> Option<(i32, i32)> {
        let x = (self.x + f64::from(x)) * self.screen_scale;
        let y = (self.y + f64::from(y)) * self.screen_scale;
        let y = self.macos_primary_top.map_or(y, |top| top - y);
        Some((coordinate(x)?, coordinate(y)?))
    }
}

fn coordinate(value: f64) -> Option<i32> {
    let value = value.round();
    (value.is_finite() && value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX)).then_some(value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_negative_monitor_origins_without_scaling_bounds() {
        let screen = ScreenGeometry {
            bounds: ScreenRect { x: -1920, y: -200, width: 1920, height: 1080 },
            work_area: ScreenRect { x: -1920, y: -176, width: 1920, height: 1056 },
        };
        assert!(screen.is_valid());
        assert_eq!(screen.bounds.cef_rect().width, 1920);
    }

    #[test]
    fn view_position_preserves_fractional_origin_until_platform_conversion() {
        let position = ViewPosition { x: -1160.25, y: 112.25, screen_scale: 1.25, macos_primary_top: Some(982.0) };
        assert!(position.is_valid());
        assert_eq!(position.cef_origin(), Some((-1160, 112)));
        assert_eq!(position.screen_point(10, 20), Some((-1438, 817)));
        let pixels = ViewPosition { screen_scale: 2.5, macos_primary_top: None, ..position };
        assert_eq!(pixels.screen_point(10, 20), Some((-2876, 331)));
        assert_eq!(position.screen_point(i32::MAX, 0), None);
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!ViewPosition { screen_scale: scale, ..position }.is_valid());
        }
        assert!(!ViewPosition { x: f64::INFINITY, ..position }.is_valid());
        assert!(!ViewPosition { macos_primary_top: Some(f64::NAN), ..position }.is_valid());
        assert!(!ViewPosition { x: f64::from(i32::MAX), ..position }.is_valid());
    }

    #[test]
    fn rejects_empty_or_outside_work_areas_without_integer_overflow() {
        let bounds = ScreenRect { x: i32::MAX - 100, y: 0, width: 100, height: 100 };
        let mut screen = ScreenGeometry { bounds, work_area: bounds };
        assert!(screen.is_valid());
        screen.work_area.width = 101;
        assert!(!screen.is_valid());
        screen.work_area.width = 0;
        assert!(!screen.is_valid());
    }
}
