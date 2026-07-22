//! Visual primitives → timeline geometry. The two shapes a vision model grounds
//! with — a **point** (one location) and a **bounding box** (a rectangle) — turned
//! into the numbers [`engine::Transform`](crate::engine::Transform) already speaks:
//! a fractional centre-offset (`x`/`y`), per-axis scale, and keyframe curves.
//!
//! Coordinate spaces, kept honest in one place so nothing downstream guesses:
//!
//! - **Model space** (input): normalized `0.0..=1.0`, origin **top-left**, `y` down
//!   — the vision convention. Divide Gemini-style `0..1000` integers by 1000 at
//!   the boundary.
//! - **Transform space** (output): fractional offset from the frame **centre**,
//!   `+x` right, `+y` down, in units of one frame (`0.5` = half a frame). This is
//!   exactly what `Transform.x`/`.y` mean, so a point at the top-left of the frame
//!   (`0,0`) becomes offset `(-0.5,-0.5)`, and the centre (`0.5,0.5`) becomes `(0,0)`.
//!
//! ponytail: no `engine` dep — this returns bare numbers and `Animated` curves, so
//! it tests without a filter graph. The action layer assembles them into a
//! `Transform`. Standalone like [`keyframe`](crate::keyframe), and for the same reason.

use crate::keyframe::{Animated, Interp, Key};

/// Clamp a raw model coordinate into `0.0..=1.0`. A model occasionally emits a
/// hair past the edge; clamping is friendlier than rejecting a whole detection.
fn unit(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// A single location on the frame, model space (`0..1`, top-left origin).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x: unit(x), y: unit(y) }
    }

    /// The literal offset that places something's centre **at** this point — for
    /// dropping a title or a PiP where the model pointed.
    pub fn offset(&self) -> (f64, f64) {
        (self.x - 0.5, self.y - 0.5)
    }

    /// The offset that brings this point **to** the frame centre — for tracking,
    /// where the subject the model found should end up centred. The mirror of
    /// [`offset`](Point::offset).
    pub fn centering_offset(&self) -> (f64, f64) {
        (0.5 - self.x, 0.5 - self.y)
    }
}

/// A rectangle on the frame, model space (`0..1`, top-left origin). Normalized on
/// construction so `min` is always top-left regardless of the order the model
/// reported the corners in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl BBox {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let (x0, x1) = (unit(x0.min(x1)), unit(x0.max(x1)));
        let (y0, y1) = (unit(y0.min(y1)), unit(y0.max(y1)));
        Self { x0, y0, x1, y1 }
    }

    /// Centre of the box, model space.
    pub fn center(&self) -> Point {
        Point::new((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    /// Width and height as frame fractions.
    pub fn size(&self) -> (f64, f64) {
        (self.x1 - self.x0, self.y1 - self.y0)
    }

    /// The static-transform numbers that box a layer into this rectangle:
    /// `(offset_x, offset_y, scale_x, scale_y)`. The layer's centre lands on the
    /// box centre and its size shrinks to the box's — `scale` (master) stays `1.0`,
    /// so the caller keeps that knob free. A zero-area box is floored to a sliver so
    /// the layer never collapses to nothing.
    pub fn to_transform(&self) -> (f64, f64, f64, f64) {
        let (ox, oy) = self.center().offset();
        let (mut sx, mut sy) = self.size();
        sx = sx.max(0.01);
        sy = sy.max(0.01);
        (ox, oy, sx, sy)
    }
}

/// A time-stamped detection: where the subject was at clip-local time `t` seconds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TrackSample {
    pub t: f64,
    pub point: Point,
}

/// Turn a sequence of detections into the `x`/`y` position curves that pan the
/// frame to follow the subject. `center = true` keeps the subject centred (the
/// "follow this face" case); `center = false` moves the layer so its centre traces
/// the reported path. Keys ease with [`Interp::Smooth`] so the pan glides rather
/// than snapping between detections.
///
/// Returns `None` for fewer than two samples — one detection is a position, not a
/// path, so the caller should use [`Point::offset`] instead. A curve is emitted
/// even if every sample is identical; [`Animated::curve`] collapses a lone key but
/// not a flat multi-key path, and the caller may want the explicit hold.
///
/// Note: a pan only has room to move once the frame is zoomed in past `1.0` — at
/// `scale == 1` the crop already fills the source and there is nowhere to slide.
/// Giving the subject headroom (a zoom) is the action layer's job, not this one's.
pub fn track_curves(samples: &[TrackSample], center: bool) -> Option<(Animated<f64>, Animated<f64>)> {
    if samples.len() < 2 {
        return None;
    }
    let mut xs = Vec::with_capacity(samples.len());
    let mut ys = Vec::with_capacity(samples.len());
    for s in samples {
        let (ox, oy) = if center { s.point.centering_offset() } else { s.point.offset() };
        xs.push(Key { t: s.t, v: ox, interp: Interp::Smooth });
        ys.push(Key { t: s.t, v: oy, interp: Interp::Smooth });
    }
    Some((Animated::curve(xs), Animated::curve(ys)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_offset_and_centering_are_mirror_images() {
        let p = Point::new(0.5, 0.5); // dead centre
        assert_eq!(p.offset(), (0.0, 0.0));
        assert_eq!(p.centering_offset(), (0.0, 0.0));

        let tl = Point::new(0.0, 0.0); // top-left corner
        assert_eq!(tl.offset(), (-0.5, -0.5)); // place there → shift up-left
        assert_eq!(tl.centering_offset(), (0.5, 0.5)); // centre it → shift down-right
    }

    #[test]
    fn out_of_range_coords_clamp_not_panic() {
        let p = Point::new(1.4, -0.2);
        assert_eq!((p.x, p.y), (1.0, 0.0));
    }

    #[test]
    fn box_normalizes_corner_order() {
        // Reported bottom-right first — still the same rectangle.
        let b = BBox::new(0.8, 0.9, 0.2, 0.1);
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (0.2, 0.1, 0.8, 0.9));
    }

    #[test]
    fn box_to_transform_centres_and_shrinks() {
        // A box over the top-left quarter of the frame.
        let b = BBox::new(0.0, 0.0, 0.5, 0.5);
        let (ox, oy, sx, sy) = b.to_transform();
        assert!((ox - -0.25).abs() < 1e-9); // centre of that quarter is a quarter up-left
        assert!((oy - -0.25).abs() < 1e-9);
        assert!((sx - 0.5).abs() < 1e-9); // half the frame each way
        assert!((sy - 0.5).abs() < 1e-9);
    }

    #[test]
    fn degenerate_box_does_not_collapse_to_zero() {
        let (_, _, sx, sy) = BBox::new(0.5, 0.5, 0.5, 0.5).to_transform();
        assert!(sx > 0.0 && sy > 0.0);
    }

    #[test]
    fn track_needs_two_samples_and_eases() {
        let one = [TrackSample { t: 0.0, point: Point::new(0.2, 0.2) }];
        assert!(track_curves(&one, true).is_none());

        let path = [
            TrackSample { t: 0.0, point: Point::new(0.2, 0.5) }, // subject left
            TrackSample { t: 1.0, point: Point::new(0.8, 0.5) }, // subject right
        ];
        let (x, _y) = track_curves(&path, true).unwrap();
        // Centering: subject on the left → pan right (positive), and vice-versa.
        assert!(x.is_animated());
        assert!((x.sample(0.0) - 0.3).abs() < 1e-9); // 0.5 - 0.2
        assert!((x.sample(1.0) - -0.3).abs() < 1e-9); // 0.5 - 0.8
    }
}
