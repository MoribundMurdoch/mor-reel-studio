//! In-app plugin spine. [`dispatch`] routes a `(plugin, tool, json-params)` call
//! against a timeline [`Snapshot`](crate::Snapshot) to the right handler. The
//! boundary is JSON on purpose — it is the same shape the MCP server speaks over
//! the wire, so a live coordinate command from a model and an in-process call are
//! one code path, and a second plugin is one more match arm.
//!
//! The first (and, today, only) plugin is `coords`: the vision-model "visual
//! primitives" — a point and a bounding box — turned into transforms and pan
//! curves via [`crate::coords`].

use crate::coords::{track_curves, BBox, Point, TrackSample};
use crate::engine::AnimatedTransform;
use crate::keyframe::{Animated, Interp, Key};
use crate::{Clip, OverlayItem, Snapshot};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

/// Which lane an action targets. Only the two lanes that carry an
/// [`AnimatedTransform`] — V1 clips and V2 overlays. Titles have their own
/// alignment geometry and aren't addressed here yet.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub enum Lane {
    V1,
    V2,
}

/// A lane + index into it: which item a coordinate lands on.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ItemRef {
    pub lane: Lane,
    pub index: usize,
}

/// The mutable transform of the addressed item, or a message naming what's missing.
fn resolve<'a>(snap: &'a mut Snapshot, r: ItemRef) -> Result<&'a mut AnimatedTransform, String> {
    let t = match r.lane {
        Lane::V1 => snap.clips.get_mut(r.index).map(|c: &mut Clip| &mut c.transform),
        Lane::V2 => snap.overlays.get_mut(r.index).map(|o: &mut OverlayItem| &mut o.transform),
    };
    t.ok_or_else(|| format!("no item at {:?}[{}]", r.lane, r.index))
}

// --- tool params (JSON-facing) -------------------------------------------------
// Raw f64s, not `coords::Point`/`BBox`: the coords constructors are the single
// clamp/normalize gate, so params stay dumb and always route through them.

fn default_true() -> bool {
    true
}
fn default_zoom() -> f64 {
    1.3
}

/// Box a layer into a rectangle — the bounding-box primitive.
#[derive(Deserialize)]
struct PlaceBoxParams {
    target: ItemRef,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    /// Keep the picture's aspect and crop to fill the box, rather than stretch it.
    #[serde(default = "default_true")]
    cover: bool,
}

/// Put a layer's centre at a point — the point primitive, placed literally.
#[derive(Deserialize)]
struct PlacePointParams {
    target: ItemRef,
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct SampleParam {
    t: f64,
    x: f64,
    y: f64,
}

/// Follow a moving point across the clip — a sequence of the point primitive,
/// turned into a pan. `zoom` is the headroom the pan slides within: at `1.0` the
/// crop already fills the frame and nothing can move, so it defaults to a gentle
/// punch-in.
#[derive(Deserialize)]
struct TrackPointParams {
    target: ItemRef,
    samples: Vec<SampleParam>,
    #[serde(default = "default_zoom")]
    zoom: f64,
    /// Keep the tracked subject centred (true) vs. move the layer along the path.
    #[serde(default = "default_true")]
    center: bool,
}

// --- tool bodies ---------------------------------------------------------------

fn place_box(snap: &mut Snapshot, p: PlaceBoxParams) -> Result<String, String> {
    let (ox, oy, sx, sy) = BBox::new(p.x0, p.y0, p.x1, p.y1).to_transform();
    let t = resolve(snap, p.target)?;
    let mut pose = t.pose();
    pose.x = ox;
    pose.y = oy;
    pose.scale = 1.0;
    pose.scale_x = sx;
    pose.scale_y = sy;
    pose.cover = p.cover;
    t.set_pose(pose);
    Ok(format!("boxed {:?}[{}] → {sx:.2}×{sy:.2} at ({ox:+.2},{oy:+.2})", p.target.lane, p.target.index))
}

fn place_point(snap: &mut Snapshot, p: PlacePointParams) -> Result<String, String> {
    let (ox, oy) = Point::new(p.x, p.y).offset();
    let t = resolve(snap, p.target)?;
    let mut pose = t.pose();
    pose.x = ox;
    pose.y = oy;
    t.set_pose(pose);
    Ok(format!("placed {:?}[{}] at ({ox:+.2},{oy:+.2})", p.target.lane, p.target.index))
}

fn track_point(snap: &mut Snapshot, p: TrackPointParams) -> Result<String, String> {
    let samples: Vec<TrackSample> =
        p.samples.iter().map(|s| TrackSample { t: s.t, point: Point::new(s.x, s.y) }).collect();
    let (xc, yc) = track_curves(&samples, p.center).ok_or("track_point needs at least 2 samples")?;
    // Zoom headroom the pan lives inside — a flat curve so the animated branch of
    // AnimatedTransform::chain() engages (it keys on `scale` being animated) and
    // there is room for the crop to slide. ponytail: pan only *renders* once
    // chain() reads these x/y curves into the zoompan crop offset (engine layer).
    let z = p.zoom.max(1.0);
    let span = samples.last().map(|s| s.t).unwrap_or(0.0).max(1e-3);
    let t = resolve(snap, p.target)?;
    t.x = xc;
    t.y = yc;
    t.scale = Animated::curve(vec![
        Key { t: 0.0, v: z, interp: Interp::Smooth },
        Key { t: span, v: z, interp: Interp::Smooth },
    ]);
    Ok(format!("tracked {:?}[{}] over {} points, zoom {z:.2}", p.target.lane, p.target.index, samples.len()))
}

// --- edit plugin: plain timeline verbs -----------------------------------------
// Not geometry — the everyday edits (retime, trim, effect, mute, delete) a model
// or a CLI wants to drive without touching the GUI. Every one mutates the same
// Snapshot dispatch owns, so it lands on the undo stack and refreshes preview like
// any UI edit, and shows up over MCP, the live port and the `morreel` CLI at once.

fn miss(r: ItemRef) -> String {
    format!("no item at {:?}[{}]", r.lane, r.index)
}

/// Edit a field that lives on *both* lane structs. The body is expanded once per
/// lane, so it type-checks against `Clip` in the V1 arm and `OverlayItem` in the
/// V2 arm — every field it names exists on both. ponytail: a macro, not a trait —
/// the two structs share field *names*, not an interface, and one trait with two
/// impls would be more code than this. `volume`/`transition` are V1-only and so
/// don't go through here.
macro_rules! on_item {
    ($snap:expr, $r:expr, |$it:ident| $body:expr) => {
        match $r.lane {
            Lane::V1 => {
                let $it = $snap.clips.get_mut($r.index).ok_or_else(|| miss($r))?;
                $body
            }
            Lane::V2 => {
                let $it = $snap.overlays.get_mut($r.index).ok_or_else(|| miss($r))?;
                $body
            }
        }
    };
}

#[derive(Deserialize)]
struct SetEffectParams {
    target: ItemRef,
    effect: String,
    amount: Option<f64>,
}

#[derive(Deserialize)]
struct SetSpeedParams {
    target: ItemRef,
    speed: Option<f64>,
    reverse: Option<bool>,
}

#[derive(Deserialize)]
struct TrimParams {
    target: ItemRef,
    #[serde(rename = "in")]
    in_s: Option<f64>,
    out: Option<f64>,
}

#[derive(Deserialize)]
struct EnableParams {
    target: ItemRef,
    on: bool,
}

#[derive(Deserialize)]
struct SetVolumeParams {
    target: ItemRef,
    volume: Option<f64>,
    mute: Option<bool>,
}

#[derive(Deserialize)]
struct RemoveParams {
    target: ItemRef,
}

/// Set the effect/look preset, validated against the live effect list (built-ins
/// plus any hub bundle) so a typo is a clear error the model can retry, not a
/// silent no-op at render time.
fn set_effect(snap: &mut Snapshot, p: SetEffectParams) -> Result<String, String> {
    if !crate::all_effects().iter().any(|(_, n, _)| *n == p.effect) {
        let names: Vec<String> = crate::all_effects().into_iter().map(|(_, n, _)| n).collect();
        return Err(format!("unknown effect '{}'. valid: {}", p.effect, names.join(", ")));
    }
    let amt = p.amount.unwrap_or(1.0).clamp(0.0, 1.0);
    let r = p.target;
    on_item!(snap, r, |it| {
        it.effect = p.effect.clone();
        it.effect_amount = amt;
    });
    Ok(format!("{:?}[{}] effect → {} ({amt:.2})", r.lane, r.index, p.effect))
}

/// Retime: `speed` (0.1..10, 1.0 = normal) and/or `reverse`. Clamp matches the
/// GUI's own scale limits so a driven edit can't leave a state the UI can't.
fn set_speed(snap: &mut Snapshot, p: SetSpeedParams) -> Result<String, String> {
    if p.speed.is_none() && p.reverse.is_none() {
        return Err("set_speed needs 'speed' and/or 'reverse'".into());
    }
    let r = p.target;
    let (sp, rev) = on_item!(snap, r, |it| {
        if let Some(s) = p.speed {
            it.speed = s.clamp(0.1, 10.0);
        }
        if let Some(v) = p.reverse {
            it.reverse = v;
        }
        (it.speed, it.reverse)
    });
    Ok(format!("{:?}[{}] speed {sp:.2}× reverse={rev}", r.lane, r.index))
}

/// Set the source trim in/out (seconds into the source file). Either bound is
/// optional; the pair is clamped into `0..duration` and kept in order with a small
/// minimum span, so no combination collapses the clip.
fn trim(snap: &mut Snapshot, p: TrimParams) -> Result<String, String> {
    if p.in_s.is_none() && p.out.is_none() {
        return Err("trim needs 'in' and/or 'out' (seconds)".into());
    }
    let r = p.target;
    let msg = on_item!(snap, r, |it| {
        const MIN_SRC: f64 = 0.05;
        let dur = it.duration.max(MIN_SRC);
        let nin = p.in_s.unwrap_or(it.in_s).clamp(0.0, dur - MIN_SRC);
        let nout = p.out.unwrap_or(it.out_s).clamp(nin + MIN_SRC, dur);
        it.in_s = nin;
        it.out_s = nout;
        format!("{:?}[{}] trim {nin:.2}..{nout:.2}s", r.lane, r.index)
    });
    Ok(msg)
}

/// Enable/disable an item — the driven form of Clip › Disable (invisible + silent,
/// still on the timeline).
fn enable(snap: &mut Snapshot, p: EnableParams) -> Result<String, String> {
    let r = p.target;
    on_item!(snap, r, |it| {
        it.enabled = p.on;
    });
    Ok(format!("{:?}[{}] {}", r.lane, r.index, if p.on { "enabled" } else { "disabled" }))
}

/// Set a V1 clip's audio gain (0 = silent, 1 = unity, 2 = +6dB). `mute:true` is a
/// shortcut for volume 0; an explicit `volume` wins if both are given. V2 overlays
/// carry no audio, so this rejects them rather than silently doing nothing.
fn set_volume(snap: &mut Snapshot, p: SetVolumeParams) -> Result<String, String> {
    let r = p.target;
    if r.lane != Lane::V1 {
        return Err("set_volume applies to V1 clips (V2 overlays have no audio)".into());
    }
    if p.volume.is_none() && p.mute.is_none() {
        return Err("set_volume needs 'volume' and/or 'mute'".into());
    }
    let c = snap.clips.get_mut(r.index).ok_or_else(|| miss(r))?;
    if p.mute == Some(true) {
        c.volume = 0.0;
    }
    if let Some(v) = p.volume {
        c.volume = v.clamp(0.0, 2.0);
    }
    Ok(format!("V1[{}] volume {:.2}", r.index, c.volume))
}

/// Delete an item from its lane. Indices after it shift down by one, same as a
/// GUI delete — a follow-up call should re-`list_items` rather than assume old
/// indices.
fn remove(snap: &mut Snapshot, p: RemoveParams) -> Result<String, String> {
    let r = p.target;
    let name = match r.lane {
        Lane::V1 => {
            if r.index >= snap.clips.len() {
                return Err(miss(r));
            }
            snap.clips.remove(r.index).name
        }
        Lane::V2 => {
            if r.index >= snap.overlays.len() {
                return Err(miss(r));
            }
            snap.overlays.remove(r.index).name
        }
    };
    Ok(format!("removed {:?}[{}] {name}", r.lane, r.index))
}

// --- tool dispatch -------------------------------------------------------------

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, String> {
    serde_json::from_value(params.clone()).map_err(|e| format!("bad params: {e}"))
}

/// Run `plugin.tool(params)` against the timeline, returning a short summary of
/// what changed (or an error message). One plugin today — `coords` (visual
/// primitives → geometry, see [`crate::coords`]); a second is another arm.
pub fn dispatch(snap: &mut Snapshot, plugin: &str, tool: &str, params: &Value) -> Result<String, String> {
    match plugin {
        "coords" => match tool {
            "place_box" => place_box(snap, parse(params)?),
            "place_point" => place_point(snap, parse(params)?),
            "track_point" => track_point(snap, parse(params)?),
            other => Err(format!("coords: unknown tool '{other}'")),
        },
        "edit" => match tool {
            "set_effect" => set_effect(snap, parse(params)?),
            "set_speed" => set_speed(snap, parse(params)?),
            "trim" => trim(snap, parse(params)?),
            "enable" => enable(snap, parse(params)?),
            "set_volume" => set_volume(snap, parse(params)?),
            "remove" => remove(snap, parse(params)?),
            other => Err(format!("edit: unknown tool '{other}'")),
        },
        other => Err(format!("no plugin '{other}'")),
    }
}

/// Every plugin and its tools — the shape an MCP `tools/list` answer or a UI
/// menu is built from.
pub fn manifest() -> Value {
    serde_json::json!([
        {
            "plugin": "coords",
            "tools": [
                {"name": "place_box", "description": "Box a V1/V2 layer into a bounding box (x0,y0,x1,y1 in 0..1, top-left origin)."},
                {"name": "place_point", "description": "Put a V1/V2 layer's centre at a point (x,y in 0..1, top-left origin)."},
                {"name": "track_point", "description": "Pan a layer to follow a moving point: samples=[{t,x,y}], optional zoom, center."},
            ],
        },
        {
            "plugin": "edit",
            "tools": [
                {"name": "set_effect", "description": "Set a V1/V2 item's effect/look preset: effect=<name>, optional amount 0..1."},
                {"name": "set_speed", "description": "Retime a V1/V2 item: speed (0.1..10) and/or reverse (bool)."},
                {"name": "trim", "description": "Set a V1/V2 item's source trim: in and/or out, in seconds."},
                {"name": "enable", "description": "Enable/disable a V1/V2 item (invisible+silent, still on the timeline): on=<bool>."},
                {"name": "set_volume", "description": "Set a V1 clip's audio gain: volume (0..2) and/or mute (bool). V1 only."},
                {"name": "remove", "description": "Delete a V1/V2 item from its lane (later indices shift down)."},
            ],
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mixer;

    fn clip() -> Clip {
        Clip {
            path: String::new(),
            name: String::new(),
            duration: 5.0,
            in_s: 0.0,
            out_s: 5.0,
            has_audio: true,
            effect: "None".to_string(),
            effect_amount: 1.0,
            framing: "Crop".to_string(),
            transform: AnimatedTransform::default(),
            grade: crate::engine::Grade::default(),
            speed: 1.0,
            reverse: false,
            volume: 1.0,
            denoise: 0.0,
            treat: "None".to_string(),
            transition: "None".to_string(),
            trans_dur: 0.5,
            thumb: String::new(),
            wave: String::new(),
            proxy: String::new(),
            group: 0,
            enabled: true,
            solo: false,
        }
    }

    fn one_clip_snapshot() -> Snapshot {
        Snapshot {
            clips: vec![clip()],
            ..Default::default()
        }
    }

    #[test]
    fn place_box_sets_the_static_pose() {
        
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({
            "target": {"lane": "V1", "index": 0},
            "x0": 0.0, "y0": 0.0, "x1": 0.5, "y1": 0.5
        });
        dispatch(&mut snap, "coords", "place_box", &params).unwrap();
        let pose = snap.clips[0].transform.pose();
        assert!((pose.scale_x - 0.5).abs() < 1e-9);
        assert!((pose.x - -0.25).abs() < 1e-9);
        assert!(pose.cover); // defaulted on
        assert!(!snap.clips[0].transform.x.is_animated()); // static, not a curve
    }

    #[test]
    fn track_point_writes_pan_curves_and_zoom() {
        
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({
            "target": {"lane": "V1", "index": 0},
            "samples": [{"t": 0.0, "x": 0.2, "y": 0.5}, {"t": 2.0, "x": 0.8, "y": 0.5}],
            "zoom": 1.5
        });
        dispatch(&mut snap, "coords", "track_point", &params).unwrap();
        let xf = &snap.clips[0].transform;
        assert!(xf.x.is_animated()); // pan curve laid down
        assert!(xf.scale.is_animated()); // zoom headroom engages the animated branch
        assert!((xf.scale.sample(0.0) - 1.5).abs() < 1e-9);
        // Centering a subject that moves left→right pans right→left.
        assert!(xf.x.sample(0.0) > xf.x.sample(2.0));
    }

    #[test]
    fn a_missing_target_is_an_error_not_a_panic() {
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({"target": {"lane": "V2", "index": 0}, "x": 0.5, "y": 0.5});
        assert!(dispatch(&mut snap, "coords", "place_point", &params).is_err());
    }

    #[test]
    fn manifest_lists_the_coords_tools() {
        let m = manifest();
        let s = m.to_string();
        assert!(s.contains("place_box") && s.contains("place_point") && s.contains("track_point"));
        // and the edit plugin's verbs
        assert!(s.contains("set_speed") && s.contains("trim") && s.contains("remove"));
    }

    #[test]
    fn set_speed_clamps_and_can_reverse() {
        let mut snap = one_clip_snapshot();
        let p = serde_json::json!({"target": {"lane": "V1", "index": 0}, "speed": 99.0, "reverse": true});
        dispatch(&mut snap, "edit", "set_speed", &p).unwrap();
        assert_eq!(snap.clips[0].speed, 10.0); // clamped to the GUI's ceiling
        assert!(snap.clips[0].reverse);
    }

    #[test]
    fn trim_keeps_in_before_out() {
        let mut snap = one_clip_snapshot(); // clip duration 5.0
        // Ask for out before in — must not collapse or invert.
        let p = serde_json::json!({"target": {"lane": "V1", "index": 0}, "in": 3.0, "out": 1.0});
        dispatch(&mut snap, "edit", "trim", &p).unwrap();
        let c = &snap.clips[0];
        assert!(c.in_s < c.out_s && c.in_s >= 0.0 && c.out_s <= 5.0);
    }

    #[test]
    fn set_effect_rejects_an_unknown_name() {
        let mut snap = one_clip_snapshot();
        let bad = serde_json::json!({"target": {"lane": "V1", "index": 0}, "effect": "Nope"});
        assert!(dispatch(&mut snap, "edit", "set_effect", &bad).is_err());
        let ok = serde_json::json!({"target": {"lane": "V1", "index": 0}, "effect": "None", "amount": 2.0});
        dispatch(&mut snap, "edit", "set_effect", &ok).unwrap();
        assert_eq!(snap.clips[0].effect, "None");
        assert_eq!(snap.clips[0].effect_amount, 1.0); // amount clamped into 0..1
    }

    #[test]
    fn set_volume_is_v1_only_and_mutes() {
        let mut snap = one_clip_snapshot();
        dispatch(&mut snap, "edit", "set_volume", &serde_json::json!({"target": {"lane": "V1", "index": 0}, "mute": true})).unwrap();
        assert_eq!(snap.clips[0].volume, 0.0);
        // V2 has no audio → error, not a silent no-op.
        let v2 = serde_json::json!({"target": {"lane": "V2", "index": 0}, "volume": 1.0});
        assert!(dispatch(&mut snap, "edit", "set_volume", &v2).is_err());
    }

    #[test]
    fn remove_shrinks_the_lane() {
        let mut snap = one_clip_snapshot();
        dispatch(&mut snap, "edit", "remove", &serde_json::json!({"target": {"lane": "V1", "index": 0}})).unwrap();
        assert!(snap.clips.is_empty());
        // removing again is an error, not a panic
        assert!(dispatch(&mut snap, "edit", "remove", &serde_json::json!({"target": {"lane": "V1", "index": 0}})).is_err());
    }
}
