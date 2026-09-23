//! Mouse input (spec §4): which quarter of the window a click landed in, how
//! long to wait before a click is known not to be a double click, and turning
//! wheel movement into whole notches.
//!
//! The OS-specific values are read here so the viewer never needs a `cfg`.

use std::time::Duration;

use gpui_kit::{Pixels, Point, ScrollDelta, Size};

use crate::library::Step;

/// A quarter of the window, numbered 1–4 anticlockwise from the top left as
/// in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickArea {
    /// 1: previous sibling folder (↑).
    TopLeft,
    /// 2: previous image (←).
    BottomLeft,
    /// 3: next image (→).
    BottomRight,
    /// 4: next sibling folder (↓).
    TopRight,
}

impl ClickArea {
    /// The quarter of a window of `size` that contains `position`. The whole
    /// window counts, letterbox bars included, so the areas do not move with
    /// the image's aspect ratio.
    pub fn at(position: Point<Pixels>, size: Size<Pixels>) -> Self {
        let left = f32::from(position.x) < f32::from(size.width) / 2.;
        let top = f32::from(position.y) < f32::from(size.height) / 2.;
        match (left, top) {
            (true, true) => Self::TopLeft,
            (true, false) => Self::BottomLeft,
            (false, false) => Self::BottomRight,
            (false, true) => Self::TopRight,
        }
    }
}

/// How long a click waits for a second one before it counts as a single
/// click. Matches the interval GPUI itself uses to report `click_count == 2`.
pub fn double_click_interval() -> Duration {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
        // SAFETY: takes no arguments and only reads a system setting.
        Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }))
    }
    #[cfg(not(windows))]
    {
        // GPUI's Linux backends hard-code this value.
        Duration::from_millis(400)
    }
}

/// How many "lines" GPUI reports for one wheel notch.
fn lines_per_notch() -> f32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETWHEELSCROLLLINES,
        };
        let mut lines: u32 = 3;
        // SAFETY: SPI_GETWHEELSCROLLLINES writes one u32 through the pointer,
        // which points at a live local.
        unsafe {
            SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                (&mut lines as *mut u32).cast(),
                0,
            );
        }
        // GPUI multiplies notches by this setting, "one screen at a time"
        // included, so the same value converts back to notches.
        lines as f32
    }
    #[cfg(not(windows))]
    {
        // GPUI's Linux backends scale every notch by this constant.
        3.0
    }
}

/// Collects wheel movement until it adds up to one notch, so a high-resolution
/// wheel that reports fractions of a notch still moves one image per notch.
#[derive(Debug, Default)]
pub struct WheelNotches {
    accumulated: f32,
}

impl WheelNotches {
    /// Feeds one wheel event; returns the image step once a whole notch has
    /// been turned. Down is forward (→), up is backward (←).
    ///
    /// Pixel deltas come from touchpads, which are deliberately ignored:
    /// kinetic scrolling would flick through many images.
    pub fn feed(&mut self, delta: &ScrollDelta) -> Option<Step> {
        match delta {
            ScrollDelta::Lines(lines) => self.feed_lines(lines.y, lines_per_notch()),
            ScrollDelta::Pixels(_) => None,
        }
    }

    fn feed_lines(&mut self, lines: f32, lines_per_notch: f32) -> Option<Step> {
        if lines == 0. || lines_per_notch <= 0. {
            return None;
        }
        // A change of direction starts counting afresh.
        if self.accumulated * lines < 0. {
            self.accumulated = 0.;
        }
        self.accumulated += lines;

        // Allow for rounding when fractions of a notch are summed.
        if self.accumulated.abs() < lines_per_notch - 0.01 {
            return None;
        }
        // At most one image per event, however large the setting makes it.
        self.accumulated = 0.;
        // GPUI reports turning the wheel down as a negative delta.
        Some(if lines < 0. {
            Step::Forward
        } else {
            Step::Backward
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ClickArea, WheelNotches};
    use crate::library::Step;
    use gpui_kit::{point, px, size};

    #[test]
    fn each_quarter_maps_to_its_area() {
        let window = size(px(800.), px(600.));
        assert_eq!(
            ClickArea::at(point(px(10.), px(10.)), window),
            ClickArea::TopLeft
        );
        assert_eq!(
            ClickArea::at(point(px(10.), px(590.)), window),
            ClickArea::BottomLeft
        );
        assert_eq!(
            ClickArea::at(point(px(790.), px(590.)), window),
            ClickArea::BottomRight
        );
        assert_eq!(
            ClickArea::at(point(px(790.), px(10.)), window),
            ClickArea::TopRight
        );
    }

    #[test]
    fn the_exact_centre_belongs_to_the_bottom_right() {
        let window = size(px(800.), px(600.));
        assert_eq!(
            ClickArea::at(point(px(400.), px(300.)), window),
            ClickArea::BottomRight
        );
    }

    #[test]
    fn one_notch_down_is_one_step_forward() {
        let mut wheel = WheelNotches::default();
        assert_eq!(wheel.feed_lines(-3., 3.), Some(Step::Forward));
    }

    #[test]
    fn one_notch_up_is_one_step_backward() {
        let mut wheel = WheelNotches::default();
        assert_eq!(wheel.feed_lines(3., 3.), Some(Step::Backward));
    }

    #[test]
    fn fractions_of_a_notch_add_up_to_one_step() {
        let mut wheel = WheelNotches::default();
        assert_eq!(wheel.feed_lines(-0.75, 3.), None);
        assert_eq!(wheel.feed_lines(-0.75, 3.), None);
        assert_eq!(wheel.feed_lines(-0.75, 3.), None);
        assert_eq!(wheel.feed_lines(-0.75, 3.), Some(Step::Forward));
        // The count restarts after a step.
        assert_eq!(wheel.feed_lines(-0.75, 3.), None);
    }

    #[test]
    fn reversing_direction_discards_the_partial_notch() {
        let mut wheel = WheelNotches::default();
        assert_eq!(wheel.feed_lines(-2., 3.), None);
        assert_eq!(wheel.feed_lines(2., 3.), None);
        assert_eq!(wheel.feed_lines(1., 3.), Some(Step::Backward));
    }

    #[test]
    fn a_huge_event_still_moves_only_one_image() {
        let mut wheel = WheelNotches::default();
        assert_eq!(wheel.feed_lines(-30., 3.), Some(Step::Forward));
        assert_eq!(wheel.feed_lines(-1., 3.), None);
    }
}
