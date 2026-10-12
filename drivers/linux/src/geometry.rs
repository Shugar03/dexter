//! Platform-free window selection for Linux screenshot capture.

use dexter_core::Window;

/// Largest on-screen layer-0 window with area > 1; if none is on screen,
/// use the largest layer-0 window with area > 1.
pub fn pick_capture_window(windows: &[Window]) -> Option<&Window> {
    let eligible = |window: &&Window| window.layer == 0 && window.bounds.w * window.bounds.h > 1.0;
    let largest = |left: &&Window, right: &&Window| {
        (left.bounds.w * left.bounds.h).total_cmp(&(right.bounds.w * right.bounds.h))
    };
    windows
        .iter()
        .filter(|window| eligible(window) && window.on_screen)
        .max_by(|left, right| largest(left, right))
        .or_else(|| windows.iter().filter(eligible).max_by(largest))
}

#[cfg(test)]
mod tests {
    use super::pick_capture_window;
    use dexter_core::{Rect, Window};

    fn window(id: u32, area: f64, on_screen: bool, layer: i32) -> Window {
        Window {
            id,
            pid: 1,
            app: "fixture".into(),
            bundle_id: None,
            title: None,
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                w: area,
                h: 1.0,
            },
            on_screen,
            layer,
        }
    }

    #[test]
    fn largest_on_screen_layer_zero_window_wins() {
        let windows = [
            window(1, 200.0, false, 0),
            window(2, 100.0, true, 0),
            window(3, 300.0, true, 1),
        ];
        assert_eq!(pick_capture_window(&windows).unwrap().id, 2);
    }

    #[test]
    fn falls_back_to_largest_layer_zero_window() {
        let windows = [window(1, 200.0, false, 0), window(2, 100.0, false, 0)];
        assert_eq!(pick_capture_window(&windows).unwrap().id, 1);
    }

    #[test]
    fn excludes_nonzero_layers_and_degenerate_windows() {
        let windows = [window(1, 1.0, true, 0), window(2, 20.0, true, 1)];
        assert!(pick_capture_window(&windows).is_none());
    }
}
