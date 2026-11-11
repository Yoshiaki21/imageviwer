//! Zoom and pan (spec §5): how far the image is magnified beyond fitting the
//! window, where it sits, and the rectangle it is drawn into.
//!
//! Plain arithmetic in logical pixels, with no GPUI types, so it can be tested
//! on its own. Sizes are `(width, height)`, points and offsets `(x, y)`.

/// The most a zoom may magnify: one image pixel drawn this many physical
/// pixels wide.
const MAX_PIXEL_SCALE: f32 = 32.0;

/// The zoom of the image on screen. Starts fitted on every image change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zoom {
    /// Magnification relative to fitting the window; 1.0 is fitted and the
    /// smallest it gets (zooming out stops at the fit).
    scale: f32,
    /// How far the image's centre sits from the window's centre.
    offset: (f32, f32),
}

impl Default for Zoom {
    fn default() -> Self {
        Self::fitted()
    }
}

impl Zoom {
    pub fn fitted() -> Self {
        Self {
            scale: 1.0,
            offset: (0., 0.),
        }
    }

    pub fn is_fitted(&self) -> bool {
        self.scale <= 1.0
    }

    /// Multiplies the magnification by `factor`, keeping the image point under
    /// `cursor` where it is. The result stays between the fit and `max_scale`.
    pub fn zoom_at(
        &mut self,
        factor: f32,
        cursor: (f32, f32),
        viewport: (f32, f32),
        image: (f32, f32),
        max_scale: f32,
    ) {
        if !factor.is_finite() || factor <= 0. {
            return;
        }
        // The stored offset may be out of date after a window resize.
        self.clamp(viewport, image);
        let new_scale = (self.scale * factor).clamp(1.0, max_scale.max(1.0));
        if new_scale <= 1.0 {
            *self = Self::fitted();
            return;
        }

        let ratio = new_scale / self.scale;
        let centre = |axis: usize| -> f32 {
            let (view, offset, cursor) = if axis == 0 {
                (viewport.0, self.offset.0, cursor.0)
            } else {
                (viewport.1, self.offset.1, cursor.1)
            };
            let image_centre = view / 2. + offset;
            // The cursor's distance from the image centre grows with the
            // image, so the centre moves away from the cursor by as much.
            let moved_centre = cursor - (cursor - image_centre) * ratio;
            moved_centre - view / 2.
        };
        self.offset = (centre(0), centre(1));
        self.scale = new_scale;
        self.clamp(viewport, image);
    }

    /// Moves the image by `delta`, as far as its edges allow.
    pub fn pan(&mut self, delta: (f32, f32), viewport: (f32, f32), image: (f32, f32)) {
        self.offset.0 += delta.0;
        self.offset.1 += delta.1;
        self.clamp(viewport, image);
    }

    /// The rectangle `(x, y, width, height)` to draw the image into. Along an
    /// axis where the image is smaller than the window it is centred; where it
    /// is larger its edges never come inside the window, so no black gap
    /// opens up beside a magnified image.
    pub fn layout(&self, viewport: (f32, f32), image: (f32, f32)) -> (f32, f32, f32, f32) {
        let mut zoom = *self;
        zoom.clamp(viewport, image);
        let (fit_w, fit_h) = fit(viewport, image);
        let (w, h) = (fit_w * zoom.scale, fit_h * zoom.scale);
        let x = viewport.0 / 2. + zoom.offset.0 - w / 2.;
        let y = viewport.1 / 2. + zoom.offset.1 - h / 2.;
        (x, y, w, h)
    }

    fn clamp(&mut self, viewport: (f32, f32), image: (f32, f32)) {
        let (fit_w, fit_h) = fit(viewport, image);
        self.offset.0 = clamp_axis(self.offset.0, fit_w * self.scale, viewport.0);
        self.offset.1 = clamp_axis(self.offset.1, fit_h * self.scale, viewport.1);
    }
}

/// The largest magnification relative to the fit: until one image pixel is
/// [`MAX_PIXEL_SCALE`] physical pixels wide, but never below the fit itself.
pub fn max_scale(viewport: (f32, f32), image: (f32, f32), scale_factor: f32) -> f32 {
    let (fit_w, _) = fit(viewport, image);
    if fit_w <= 0. || image.0 <= 0. || scale_factor <= 0. {
        return 1.0;
    }
    let physical_per_image_pixel = fit_w / image.0 * scale_factor;
    (MAX_PIXEL_SCALE / physical_per_image_pixel).max(1.0)
}

/// The size of `image` scaled to fit inside `viewport`, keeping its aspect.
fn fit(viewport: (f32, f32), image: (f32, f32)) -> (f32, f32) {
    if image.0 <= 0. || image.1 <= 0. {
        return (0., 0.);
    }
    let scale = (viewport.0 / image.0).min(viewport.1 / image.1).max(0.);
    (image.0 * scale, image.1 * scale)
}

/// Limits the centre's offset along one axis so that an image of `size`
/// covers the window of `view` edge to edge, or is centred if it cannot.
fn clamp_axis(offset: f32, size: f32, view: f32) -> f32 {
    if size <= view {
        return 0.;
    }
    let limit = (size - view) / 2.;
    offset.clamp(-limit, limit)
}

#[cfg(test)]
mod tests {
    use super::{max_scale, Zoom};

    const VIEW: (f32, f32) = (800., 600.);
    /// Wider than the window: fits as 800×400, letterboxed top and bottom.
    const WIDE: (f32, f32) = (1600., 800.);

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// Where image point `cursor` (in window coordinates now) sits as a
    /// fraction of the drawn rectangle.
    fn fraction_under(zoom: &Zoom, cursor: (f32, f32)) -> (f32, f32) {
        let (x, y, w, h) = zoom.layout(VIEW, WIDE);
        ((cursor.0 - x) / w, (cursor.1 - y) / h)
    }

    #[test]
    fn fitted_layout_is_centred_and_letterboxed() {
        assert_eq!(Zoom::fitted().layout(VIEW, WIDE), (0., 100., 800., 400.));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let mut zoom = Zoom::fitted();
        let cursor = (600., 250.);
        let before = fraction_under(&zoom, cursor);
        zoom.zoom_at(2., cursor, VIEW, WIDE, 10.);
        let after = fraction_under(&zoom, cursor);
        assert!(close(before.0, after.0), "{before:?} vs {after:?}");
        assert!(close(before.1, after.1), "{before:?} vs {after:?}");
    }

    #[test]
    fn zooming_out_stops_at_the_fit_and_recentres() {
        let mut zoom = Zoom::fitted();
        zoom.zoom_at(3., (700., 500.), VIEW, WIDE, 10.);
        assert!(!zoom.is_fitted());
        zoom.zoom_at(0.1, (100., 100.), VIEW, WIDE, 10.);
        assert!(zoom.is_fitted());
        assert_eq!(zoom.layout(VIEW, WIDE), (0., 100., 800., 400.));
    }

    #[test]
    fn zooming_in_stops_at_the_maximum() {
        let mut zoom = Zoom::fitted();
        zoom.zoom_at(100., (400., 300.), VIEW, WIDE, 4.);
        let (_, _, w, h) = zoom.layout(VIEW, WIDE);
        assert_eq!((w, h), (3200., 1600.));
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
        let mut zoom = Zoom::fitted();
        zoom.zoom_at(2., (400., 300.), VIEW, WIDE, 10.);
        // 1600×800 in an 800×600 window.
        zoom.pan((10_000., 10_000.), VIEW, WIDE);
        assert_eq!(zoom.layout(VIEW, WIDE), (0., 0., 1600., 800.));
        zoom.pan((-10_000., -10_000.), VIEW, WIDE);
        assert_eq!(zoom.layout(VIEW, WIDE), (-800., -200., 1600., 800.));
    }

    #[test]
    fn an_axis_still_smaller_than_the_window_stays_centred() {
        let mut zoom = Zoom::fitted();
        // The wide image at 1.25× is 1000×500, still shorter than the window.
        zoom.zoom_at(1.25, (100., 120.), VIEW, WIDE, 10.);
        zoom.pan((0., 50.), VIEW, WIDE);
        let (_, y, _, h) = zoom.layout(VIEW, WIDE);
        assert_eq!((y, h), (50., 500.));
    }

    #[test]
    fn a_shrunk_window_pulls_the_image_back_over_the_gap() {
        let mut zoom = Zoom::fitted();
        zoom.zoom_at(2., (400., 300.), VIEW, WIDE, 10.);
        zoom.pan((10_000., 0.), VIEW, WIDE);
        // Half the window: the drawn size halves to 800×400, so the offset
        // that was at its limit would now leave a gap on the left.
        let (x, _, w, _) = zoom.layout((400., 300.), WIDE);
        assert_eq!((x, w), (0., 800.));
    }

    #[test]
    fn the_maximum_is_thirty_two_physical_pixels_per_image_pixel() {
        // Fitted, one image pixel is 0.5 logical pixels, so 1.0 physical at 2×.
        assert_eq!(max_scale(VIEW, WIDE, 2.), 32.);
    }

    #[test]
    fn the_maximum_is_never_below_the_fit() {
        // A tiny image already blown up past the limit by fitting.
        assert_eq!(max_scale(VIEW, (10., 10.), 1.), 1.);
        assert_eq!(max_scale(VIEW, (0., 0.), 1.), 1.);
    }
}
