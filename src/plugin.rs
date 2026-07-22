//! In-app plugin spine. A [`Plugin`] contributes named **tools** that mutate a
//! timeline [`Snapshot`](crate::Snapshot); the [`Registry`] holds them and
//! dispatches a `(plugin, tool, json-params)` call to the right one. The boundary
//! is JSON on purpose — it is the same shape the MCP server speaks over the wire,
//! so a live coordinate command from a model and an in-process call are one code
//! path, and a second plugin is a `impl Plugin` + one `push`, not an enum edit.
//!
//! The first (and, today, only) plugin is [`CoordsPlugin`]: the vision-model
//! "visual primitives" — a point and a bounding box — turned into transforms and
//! pan curves via [`crate::coords`].

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

// --- plugin trait + registry ---------------------------------------------------

/// One tool a plugin exposes — its name and a one-line description, enough to fill
/// an MCP `tools/list` or a UI menu.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
}

/// A unit of timeline capability. Implement this and register it to add tools a
/// model (or the UI) can call by name.
pub trait Plugin {
    fn name(&self) -> &'static str;
    fn tools(&self) -> Vec<ToolSpec>;
    /// Run `tool` with JSON `params` against the timeline, returning a short
    /// human-readable summary of what changed (or an error message).
    fn call(&self, snap: &mut Snapshot, tool: &str, params: &Value) -> Result<String, String>;
}

fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, String> {
    serde_json::from_value(params.clone()).map_err(|e| format!("bad params: {e}"))
}

/// Visual primitives → geometry. See [`crate::coords`].
pub struct CoordsPlugin;

impl Plugin for CoordsPlugin {
    fn name(&self) -> &'static str {
        "coords"
    }

    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "place_box",
                description: "Box a V1/V2 layer into a bounding box (x0,y0,x1,y1 in 0..1, top-left origin).",
            },
            ToolSpec {
                name: "place_point",
                description: "Put a V1/V2 layer's centre at a point (x,y in 0..1, top-left origin).",
            },
            ToolSpec {
                name: "track_point",
                description: "Pan a layer to follow a moving point: samples=[{t,x,y}], optional zoom, center.",
            },
        ]
    }

    fn call(&self, snap: &mut Snapshot, tool: &str, params: &Value) -> Result<String, String> {
        match tool {
            "place_box" => place_box(snap, parse(params)?),
            "place_point" => place_point(snap, parse(params)?),
            "track_point" => track_point(snap, parse(params)?),
            other => Err(format!("coords: unknown tool '{other}'")),
        }
    }
}

/// Holds the registered plugins and routes calls to them.
pub struct Registry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self { plugins: vec![Box::new(CoordsPlugin)] }
    }
}

impl Registry {
    /// Run `plugin.tool(params)` against the timeline.
    pub fn dispatch(
        &self,
        snap: &mut Snapshot,
        plugin: &str,
        tool: &str,
        params: &Value,
    ) -> Result<String, String> {
        let p = self.plugins.iter().find(|p| p.name() == plugin).ok_or_else(|| format!("no plugin '{plugin}'"))?;
        p.call(snap, tool, params)
    }

    /// Every registered plugin and its tools — the shape an MCP `tools/list`
    /// answer or a UI menu is built from.
    pub fn manifest(&self) -> Value {
        Value::Array(
            self.plugins
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "plugin": p.name(),
                        "tools": p.tools().iter().map(|t| serde_json::json!({
                            "name": t.name, "description": t.description
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }
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
        }
    }

    fn one_clip_snapshot() -> Snapshot {
        Snapshot {
            clips: vec![clip()],
            overlays: vec![],
            audios: vec![],
            titles: vec![],
            markers: vec![],
            mixer: Mixer::default(),
        }
    }

    #[test]
    fn place_box_sets_the_static_pose() {
        let reg = Registry::default();
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({
            "target": {"lane": "V1", "index": 0},
            "x0": 0.0, "y0": 0.0, "x1": 0.5, "y1": 0.5
        });
        reg.dispatch(&mut snap, "coords", "place_box", &params).unwrap();
        let pose = snap.clips[0].transform.pose();
        assert!((pose.scale_x - 0.5).abs() < 1e-9);
        assert!((pose.x - -0.25).abs() < 1e-9);
        assert!(pose.cover); // defaulted on
        assert!(!snap.clips[0].transform.x.is_animated()); // static, not a curve
    }

    #[test]
    fn track_point_writes_pan_curves_and_zoom() {
        let reg = Registry::default();
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({
            "target": {"lane": "V1", "index": 0},
            "samples": [{"t": 0.0, "x": 0.2, "y": 0.5}, {"t": 2.0, "x": 0.8, "y": 0.5}],
            "zoom": 1.5
        });
        reg.dispatch(&mut snap, "coords", "track_point", &params).unwrap();
        let xf = &snap.clips[0].transform;
        assert!(xf.x.is_animated()); // pan curve laid down
        assert!(xf.scale.is_animated()); // zoom headroom engages the animated branch
        assert!((xf.scale.sample(0.0) - 1.5).abs() < 1e-9);
        // Centering a subject that moves left→right pans right→left.
        assert!(xf.x.sample(0.0) > xf.x.sample(2.0));
    }

    #[test]
    fn a_missing_target_is_an_error_not_a_panic() {
        let reg = Registry::default();
        let mut snap = one_clip_snapshot();
        let params = serde_json::json!({"target": {"lane": "V2", "index": 0}, "x": 0.5, "y": 0.5});
        assert!(reg.dispatch(&mut snap, "coords", "place_point", &params).is_err());
    }

    #[test]
    fn manifest_lists_the_coords_tools() {
        let m = Registry::default().manifest();
        let s = m.to_string();
        assert!(s.contains("place_box") && s.contains("place_point") && s.contains("track_point"));
    }
}
