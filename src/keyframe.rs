//! Keyframable parameters — the spine of principle 5: everything worth animating
//! is a *curve*, not a constant. A time-varying value is an [`Animated<T>`],
//! either a plain constant or a sorted list of keyframes. It serialises **as a
//! bare scalar while constant**, so every project saved before keyframes existed
//! still loads — a stored `1.0` reads back as `Animated::Const(1.0)`, and only a
//! parameter the user actually animated grows into an array on disk.
//!
//! This module is deliberately standalone: no engine or UI deps, so it can land
//! and be tested before anything threads it into `Transform` / `TitleItem`.
//!
//! ponytail: three interpolations (Hold / Linear / Smooth) cover Ken Burns moves
//! and caption pops. Bezier handles are the upgrade path if a curve ever needs a
//! bespoke ease — nothing in a vertical reel does yet.

use serde::{Deserialize, Serialize};

/// How a value approaches a keyframe from the one before it.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Interp {
    /// No motion: hold the previous value, then jump at the key. Cuts, caption pops.
    Hold,
    /// Constant-rate straight line.
    Linear,
    /// Ease in and out (smoothstep) — the Ken Burns default.
    Smooth,
}

/// A value pinned to a clip-local time.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Key<T> {
    /// Seconds from the clip's **own** start, never the timeline. A trim or a
    /// move then carries the animation with the clip for free — keys never need
    /// re-timing when the clip slides.
    pub t: f64,
    pub v: T,
    /// How `v` is reached from the previous key. Ignored on the first key.
    pub interp: Interp,
}

/// A parameter that may vary over time.
///
/// `Const` is the common, always-valid degenerate case, and its existence is the
/// point: an `Animated<T>` can **always** be sampled — there is no such thing as
/// a parameter with no value. That totality is the one invariant the rest of the
/// editor leans on, so [`sample`](Animated::sample) returns `T`, never `Option`.
///
/// `Curve` is kept sorted by time with ≥2 keys; build it through [`curve`] rather
/// than the variant so that stays true.
///
/// [`curve`]: Animated::curve
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Animated<T> {
    /// One value, no motion. Serialises as the bare scalar (back-compat).
    Const(T),
    /// Two or more keyframes, sorted by `t`.
    Curve(Vec<Key<T>>),
}

impl<T> Animated<T> {
    /// True when this parameter actually moves. Lets the engine skip emitting a
    /// time expression for the (overwhelmingly common) static case.
    pub fn is_animated(&self) -> bool {
        matches!(self, Animated::Curve(_))
    }
}

impl<T: Copy> Animated<T> {
    /// Build a curve from keyframes: sorts by time and collapses a lone key back
    /// to a constant, so a `Curve` genuinely animates (≥2 keys, ascending time).
    ///
    /// Panics on an empty list — a parameter must have at least one value.
    pub fn curve(mut keys: Vec<Key<T>>) -> Self {
        keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        match keys.len() {
            0 => panic!("Animated::curve needs at least one key"),
            1 => Animated::Const(keys[0].v),
            _ => Animated::Curve(keys),
        }
    }
}

/// Two keys closer than this in time are "the same" keyframe — one frame at
/// 60fps. The diamond toggles the key at the playhead, and a click lands on
/// whatever key is within a frame of it rather than stacking a second one.
pub const KEY_EPS: f64 = 0.008;

impl<T: Copy> Animated<T> {
    /// Current keys as an owned vec; a `Const` yields one implicit key at t=0.
    /// The authoring path below edits this and rebuilds, so a static parameter
    /// grows its first key from wherever it already sits.
    fn keyframes(&self) -> Vec<Key<T>> {
        match self {
            Animated::Const(v) => vec![Key { t: 0.0, v: *v, interp: Interp::Smooth }],
            Animated::Curve(k) => k.clone(),
        }
    }

    /// Set (or replace) the key at time `t`. The first key on a `Const` arms the
    /// parameter — it becomes a `Curve`, and a second key at another time makes
    /// it actually move. Smooth easing, the Ken Burns default.
    pub fn set_key(&mut self, t: f64, v: T, interp: Interp) {
        let mut keys = self.keyframes();
        match keys.iter_mut().find(|k| (k.t - t).abs() < KEY_EPS) {
            Some(k) => {
                k.v = v;
                k.interp = interp;
            }
            None => keys.push(Key { t, v, interp }),
        }
        keys.sort_by(|a, b| a.t.total_cmp(&b.t));
        *self = Animated::Curve(keys);
    }

    /// Remove the key within a frame of `t`. Collapsing to one key drops back to
    /// a `Const` — un-keying the last diamond makes the parameter static again.
    /// A `Const` (or a `t` that hits no key) is left untouched.
    pub fn remove_key(&mut self, t: f64) {
        let Animated::Curve(keys) = self else { return };
        keys.retain(|k| (k.t - t).abs() >= KEY_EPS);
        if keys.len() == 1 {
            *self = Animated::Const(keys[0].v);
        }
    }

    /// Is there a key within a frame of `t`? Fills the diamond at that playhead.
    pub fn has_key(&self, t: f64) -> bool {
        matches!(self, Animated::Curve(k) if k.iter().any(|k| (k.t - t).abs() < KEY_EPS))
    }
}

impl<T> Default for Animated<T>
where
    T: Default,
{
    fn default() -> Self {
        Animated::Const(T::default())
    }
}

/// Values that can be interpolated between two keyframes. A 2D point (for a
/// combined x/y Ken Burns move) implements this the same way once it's needed.
pub trait Lerp {
    fn lerp(self, to: Self, f: f64) -> Self;
}

impl Lerp for f64 {
    fn lerp(self, to: f64, f: f64) -> f64 {
        self + (to - self) * f
    }
}

impl<T: Lerp + Copy> Animated<T> {
    /// The value at clip-local time `t`. Total by construction: a constant
    /// returns itself; a curve clamps to its endpoints outside its range (the
    /// standard hold) and interpolates the bracketing pair inside it.
    pub fn sample(&self, t: f64) -> T {
        let keys = match self {
            Animated::Const(v) => return *v,
            Animated::Curve(k) => k,
        };
        if t <= keys[0].t {
            return keys[0].v;
        }
        let last = keys.len() - 1;
        if t >= keys[last].t {
            return keys[last].v;
        }
        // Bracketing segment [a, b): a.t <= t < b.t. Guaranteed to exist because
        // t is strictly inside (keys[0].t, keys[last].t) after the clamps above.
        let j = keys.iter().position(|k| k.t > t).unwrap();
        let (a, b) = (keys[j - 1], keys[j]);
        let span = b.t - a.t;
        let f = if span <= f64::EPSILON { 0.0 } else { (t - a.t) / span };
        a.v.lerp(b.v, ease(b.interp, f))
    }
}

/// Map linear 0..1 progress through a segment onto its interpolation curve.
fn ease(interp: Interp, f: f64) -> f64 {
    match interp {
        Interp::Hold => 0.0, // stay on the previous value until the next key lands
        Interp::Linear => f,
        Interp::Smooth => f * f * (3.0 - 2.0 * f), // smoothstep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f64, interp: Interp) -> Key<f64> {
        Key { t, v, interp }
    }

    #[test]
    fn a_constant_samples_flat_and_serialises_as_a_bare_scalar() {
        let p = Animated::Const(1.5_f64);
        assert_eq!(p.sample(0.0), 1.5);
        assert_eq!(p.sample(99.0), 1.5);
        // The whole back-compat trick: a constant is just the number on disk.
        assert_eq!(serde_json::to_string(&p).unwrap(), "1.5");
        // And an old project's bare `1.5` reads straight back into a param.
        let back: Animated<f64> = serde_json::from_str("1.5").unwrap();
        assert_eq!(back, Animated::Const(1.5));
    }

    #[test]
    fn interpolations_behave() {
        let lin = Animated::curve(vec![key(0.0, 0.0, Interp::Linear), key(2.0, 10.0, Interp::Linear)]);
        assert_eq!(lin.sample(1.0), 5.0); // halfway → half the value

        let smooth = Animated::curve(vec![key(0.0, 0.0, Interp::Smooth), key(1.0, 1.0, Interp::Smooth)]);
        assert_eq!(smooth.sample(0.0), 0.0);
        assert_eq!(smooth.sample(1.0), 1.0);
        assert!((smooth.sample(0.5) - 0.5).abs() < 1e-9); // symmetric ease

        // Hold keeps the previous value across the whole segment, then jumps.
        let hold = Animated::curve(vec![key(0.0, 3.0, Interp::Linear), key(1.0, 9.0, Interp::Hold)]);
        assert_eq!(hold.sample(0.5), 3.0);
        assert_eq!(hold.sample(1.0), 9.0);
    }

    #[test]
    fn clamps_outside_the_curve_and_survives_a_round_trip() {
        let p = Animated::curve(vec![key(1.0, 4.0, Interp::Linear), key(3.0, 8.0, Interp::Linear)]);
        assert_eq!(p.sample(0.0), 4.0); // before first key
        assert_eq!(p.sample(5.0), 8.0); // after last key
        let back: Animated<f64> = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn diamonds_arm_move_and_disarm_a_parameter() {
        // A static value: no key at the playhead, nothing animates.
        let mut p = Animated::Const(100.0_f64);
        assert!(!p.has_key(0.0));
        assert!(!p.is_animated());

        // First diamond at the clip start arms it (still visually static).
        p.set_key(0.0, 100.0, Interp::Smooth);
        assert!(p.has_key(0.0));
        assert!(p.is_animated());
        assert_eq!(p.sample(5.0), 100.0);

        // A second diamond at t=2 with a new value makes it move.
        p.set_key(2.0, 140.0, Interp::Linear);
        assert!(p.has_key(2.0));
        assert_eq!(p.sample(1.0), 120.0); // halfway up the linear ramp

        // Clicking the same playhead re-keys in place, never stacks a duplicate.
        p.set_key(2.004, 160.0, Interp::Linear); // within KEY_EPS of t=2
        assert_eq!(p.sample(2.0), 160.0);
        let Animated::Curve(k) = &p else { panic!("still a curve") };
        assert_eq!(k.len(), 2);

        // Un-keying down to one key collapses straight back to a constant,
        // holding whichever value survived.
        p.remove_key(2.0);
        assert_eq!(p, Animated::Const(100.0));
        p.remove_key(0.0); // a Const has no keys to pull; left as-is
        assert_eq!(p, Animated::Const(100.0));
    }

    #[test]
    fn curve_sorts_and_collapses_a_lone_key() {
        // Given out of order, sampling still tracks ascending time.
        let p = Animated::curve(vec![key(2.0, 20.0, Interp::Linear), key(0.0, 0.0, Interp::Linear)]);
        assert_eq!(p.sample(1.0), 10.0);
        // A single key is not an animation — it degrades to a constant.
        assert_eq!(Animated::curve(vec![key(0.0, 7.0, Interp::Linear)]), Animated::Const(7.0));
    }
}
