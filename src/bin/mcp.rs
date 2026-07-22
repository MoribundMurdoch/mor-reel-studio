//! MorReel MCP server — the bridge that lets a vision model ground coordinates
//! straight into a `.morreel` project. Speaks MCP over stdio (newline-delimited
//! JSON-RPC 2.0: `initialize`, `tools/list`, `tools/call`), and each tool loads
//! the named project, applies the model's boxes/points, and saves it back.
//!
//! It shares the app's own [`coords`] math and [`keyframe`] types by including
//! those two standalone modules by path — so the geometry a model gets here is
//! identical to what the in-app plugin produces, and the keyframe curves it writes
//! serialize in exactly the format the editor loads (a `Const` as a bare number, a
//! `Curve` as an array of keys). ponytail: `#[path]` include, not a lib split —
//! both modules are dependency-light and this keeps the working app untouched.
//!
//! Offline only: it edits the project file, which the editor picks up on load. The
//! live "drive the running app" path is a separate control endpoint, not wired yet.
//!
//! Hand-rolled rather than pulling in an MCP SDK: four methods over line-delimited
//! JSON is less code than a framework's setup, and adds no dependency.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

#[path = "../keyframe.rs"]
mod keyframe;
#[path = "../coords.rs"]
mod coords;

use coords::{track_curves, BBox, Point, TrackSample};
use keyframe::{Animated, Interp, Key};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let resp = match method {
            "initialize" => Some(ok(id, init_result())),
            "tools/list" => Some(ok(id, json!({ "tools": tool_specs() }))),
            "tools/call" => Some(handle_call(id, req.get("params"))),
            "ping" => Some(ok(id, json!({}))),
            // Anything else with an id is an unknown method; without an id it's a
            // notification (e.g. notifications/initialized) — nothing to answer.
            _ if id.is_some() => Some(err(id, -32601, "method not found")),
            _ => None,
        };
        if let Some(r) = resp {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
}

// --- JSON-RPC envelopes --------------------------------------------------------

fn ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Option<Value>, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

fn init_result() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "morreel-coords", "version": env!("CARGO_PKG_VERSION") },
    })
}

// --- tools ---------------------------------------------------------------------

/// Every field is `0..1`, top-left origin — the vision-model convention. `project`
/// is the path to a `.morreel` file. `lane` is "V1" (main clips) or "V2" (overlays).
fn tool_specs() -> Value {
    let target = json!({
        "project": { "type": "string", "description": "Path to the .morreel project file." },
        "lane": { "type": "string", "enum": ["V1", "V2"], "description": "V1 = main clip, V2 = overlay." },
        "index": { "type": "integer", "description": "0-based item index within the lane." },
    });
    json!([
        {
            "name": "list_items",
            "description": "List the addressable items (V1 clips, V2 overlays) in a project, with their indices and names.",
            "inputSchema": { "type": "object",
                "properties": { "project": { "type": "string" } }, "required": ["project"] },
        },
        {
            "name": "place_box",
            "description": "Box a layer into a bounding box. x0,y0,x1,y1 are the rectangle corners in 0..1 (top-left origin).",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "x0": {"type":"number"}, "y0": {"type":"number"}, "x1": {"type":"number"}, "y1": {"type":"number"},
                "cover": {"type":"boolean","description":"Keep aspect and crop to fill (default true) vs. stretch."}
            })), "required": ["project","lane","index","x0","y0","x1","y1"] },
        },
        {
            "name": "place_point",
            "description": "Put a layer's centre at a point (x,y in 0..1, top-left origin).",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "x": {"type":"number"}, "y": {"type":"number"}
            })), "required": ["project","lane","index","x","y"] },
        },
        {
            "name": "track_point",
            "description": "Pan a layer to follow a moving point. samples is a list of {t,x,y} (t = clip-local seconds, x/y in 0..1). Optional zoom (headroom the pan slides in, default 1.3) and center (keep the subject centred, default true).",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "samples": {"type":"array","items":{"type":"object","properties":{
                    "t":{"type":"number"},"x":{"type":"number"},"y":{"type":"number"}}}},
                "zoom": {"type":"number"}, "center": {"type":"boolean"}
            })), "required": ["project","lane","index","samples"] },
        },
        // The `edit` plugin — plain timeline verbs. These drive the *running*
        // editor (start MorReel with MORREEL_LIVE=1); they need app state, not a
        // file, so offline they return a message saying so.
        {
            "name": "set_effect",
            "description": "Set a V1/V2 item's effect/look preset. effect is the preset name; amount (0..1, default 1) is its strength. Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "effect": {"type":"string"}, "amount": {"type":"number"}
            })), "required": ["lane","index","effect"] },
        },
        {
            "name": "set_speed",
            "description": "Retime a V1/V2 item: speed (0.1..10, 1=normal) and/or reverse (bool). At least one required. Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "speed": {"type":"number"}, "reverse": {"type":"boolean"}
            })), "required": ["lane","index"] },
        },
        {
            "name": "trim",
            "description": "Set a V1/V2 item's source trim: in and/or out, in seconds. Kept in order within the source duration. Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "in": {"type":"number"}, "out": {"type":"number"}
            })), "required": ["lane","index"] },
        },
        {
            "name": "enable",
            "description": "Enable/disable a V1/V2 item (invisible+silent, still on the timeline). on=<bool>. Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "on": {"type":"boolean"}
            })), "required": ["lane","index","on"] },
        },
        {
            "name": "set_volume",
            "description": "Set a V1 clip's audio gain: volume (0..2) and/or mute (bool). V1 only. Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "volume": {"type":"number"}, "mute": {"type":"boolean"}
            })), "required": ["lane","index"] },
        },
        {
            "name": "remove",
            "description": "Delete a V1/V2 item from its lane (later indices shift down). Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({})),
                "required": ["lane","index"] },
        },
        {
            "name": "get_frame",
            "description": "Render a V1/V2 clip's source frame and return the PNG file path — so you can SEE the shot (read the image) and then reframe it with place_box / place_point / track_point. This is how you 'Smart Conform' a horizontal clip into 9:16: look at the whole uncropped frame, find the subject, then place/track it. `at` = seconds into the source (default: clip midpoint). Live editor only.",
            "inputSchema": { "type": "object", "properties": merge(&target, &json!({
                "at": {"type":"number"}
            })), "required": ["lane","index"] },
        },
    ])
}

/// Which in-app plugin owns a tool name — so the live path forwards the call to
/// the right handler. The `edit` verbs go to the edit plugin; everything else is
/// coords. Kept in sync with [`crate::plugin`] by hand (two small lists).
const EDIT_TOOLS: &[&str] = &["set_effect", "set_speed", "trim", "enable", "set_volume", "remove"];

fn plugin_for(tool: &str) -> &'static str {
    if EDIT_TOOLS.contains(&tool) {
        "edit"
    } else {
        "coords"
    }
}

fn merge(a: &Value, b: &Value) -> Value {
    let mut m = a.as_object().cloned().unwrap_or_default();
    for (k, v) in b.as_object().unwrap() {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

fn handle_call(id: Option<Value>, params: Option<&Value>) -> Value {
    let params = params.cloned().unwrap_or(json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match apply(name, &args) {
        Ok(text) => ok(id, json!({ "content": [{ "type": "text", "text": text }] })),
        // Tool-level failures are returned as an isError result, per MCP — the model
        // reads the message and retries, rather than the whole call erroring out.
        Err(text) => ok(id, json!({ "content": [{ "type": "text", "text": text }], "isError": true })),
    }
}

// --- tool bodies (load → mutate → save) ----------------------------------------

fn apply(name: &str, args: &Value) -> Result<String, String> {
    // Live mode: drive the running editor over its localhost control port instead
    // of editing a file. `project` isn't needed — the app already has one loaded.
    if let Some(port) = live_port() {
        return call_live(port, name, args);
    }
    // The edit verbs (and get_frame) act on the live app, not a file — offline
    // there's nothing to mutate or render, so say how to reach them.
    if plugin_for(name) == "edit" || name == "get_frame" {
        return Err(format!(
            "'{name}' drives the running editor — start MorReel with MORREEL_LIVE=1 (or set MORREEL_LIVE_PORT). Offline, only the coords tools edit a .morreel file."
        ));
    }
    let project = args.get("project").and_then(Value::as_str).ok_or("missing 'project' path")?.to_string();
    if name == "list_items" {
        return list_items(&project);
    }
    let mut doc = load(&project)?;
    let lane = args.get("lane").and_then(Value::as_str).ok_or("missing 'lane'")?;
    let index = args.get("index").and_then(Value::as_u64).ok_or("missing 'index'")? as usize;
    let summary = match name {
        "place_box" => {
            let (x0, y0, x1, y1) = (num(args, "x0")?, num(args, "y0")?, num(args, "x1")?, num(args, "y1")?);
            let cover = args.get("cover").and_then(Value::as_bool).unwrap_or(true);
            let (ox, oy, sx, sy) = BBox::new(x0, y0, x1, y1).to_transform();
            let t = transform_of(&mut doc, lane, index)?;
            set(t, "x", json!(ox));
            set(t, "y", json!(oy));
            set(t, "scale", json!(1.0));
            set(t, "scale_x", json!(sx));
            set(t, "scale_y", json!(sy));
            set(t, "cover", json!(cover));
            format!("boxed {lane}[{index}] → {sx:.2}×{sy:.2} at ({ox:+.2},{oy:+.2})")
        }
        "place_point" => {
            let (ox, oy) = Point::new(num(args, "x")?, num(args, "y")?).offset();
            let t = transform_of(&mut doc, lane, index)?;
            set(t, "x", json!(ox));
            set(t, "y", json!(oy));
            format!("placed {lane}[{index}] at ({ox:+.2},{oy:+.2})")
        }
        "track_point" => {
            let samples = parse_samples(args)?;
            let center = args.get("center").and_then(Value::as_bool).unwrap_or(true);
            let zoom = args.get("zoom").and_then(Value::as_f64).unwrap_or(1.3).max(1.0);
            let (xc, yc) = track_curves(&samples, center).ok_or("track_point needs at least 2 samples")?;
            let span = samples.last().map(|s| s.t).unwrap_or(0.0).max(1e-3);
            let scale = Animated::curve(vec![
                Key { t: 0.0, v: zoom, interp: Interp::Smooth },
                Key { t: span, v: zoom, interp: Interp::Smooth },
            ]);
            let n = samples.len();
            let t = transform_of(&mut doc, lane, index)?;
            // to_value on the shared Animated types → exactly the on-disk format.
            set(t, "x", serde_json::to_value(&xc).unwrap());
            set(t, "y", serde_json::to_value(&yc).unwrap());
            set(t, "scale", serde_json::to_value(&scale).unwrap());
            format!("tracked {lane}[{index}] over {n} points, zoom {zoom:.2}")
        }
        other => return Err(format!("unknown tool '{other}'")),
    };
    save(&project, &doc)?;
    Ok(summary)
}

fn list_items(project: &str) -> Result<String, String> {
    let doc = load(project)?;
    let names = |key: &str| -> Vec<String> {
        doc.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, it)| {
                        let name = it.get("name").and_then(Value::as_str).unwrap_or("");
                        format!("  [{i}] {name}")
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let v1 = names("clips");
    let v2 = names("overlays");
    Ok(format!("V1 clips ({}):\n{}\nV2 overlays ({}):\n{}", v1.len(), v1.join("\n"), v2.len(), v2.join("\n")))
}

/// The mutable `transform` object of the addressed item, creating it if the item
/// was saved without one (a default transform serializes to nothing).
fn transform_of<'a>(doc: &'a mut Value, lane: &str, index: usize) -> Result<&'a mut Value, String> {
    let key = match lane {
        "V1" => "clips",
        "V2" => "overlays",
        other => return Err(format!("lane must be V1 or V2, got '{other}'")),
    };
    let arr = doc.get_mut(key).and_then(Value::as_array_mut).ok_or_else(|| format!("no {key} in project"))?;
    let item = arr.get_mut(index).ok_or_else(|| format!("no item at {lane}[{index}]"))?;
    let obj = item.as_object_mut().ok_or("item is not an object")?;
    obj.entry("transform").or_insert(json!({}));
    Ok(obj.get_mut("transform").unwrap())
}

fn set(transform: &mut Value, field: &str, v: Value) {
    if let Some(o) = transform.as_object_mut() {
        o.insert(field.to_string(), v);
    }
}

fn parse_samples(args: &Value) -> Result<Vec<TrackSample>, String> {
    let arr = args.get("samples").and_then(Value::as_array).ok_or("missing 'samples' array")?;
    arr.iter()
        .map(|s| {
            Ok(TrackSample {
                t: s.get("t").and_then(Value::as_f64).ok_or("sample missing 't'")?,
                point: Point::new(
                    s.get("x").and_then(Value::as_f64).ok_or("sample missing 'x'")?,
                    s.get("y").and_then(Value::as_f64).ok_or("sample missing 'y'")?,
                ),
            })
        })
        .collect()
}

fn num(args: &Value, key: &str) -> Result<f64, String> {
    args.get(key).and_then(Value::as_f64).ok_or_else(|| format!("missing number '{key}'"))
}

/// The editor's live-control port, if live mode is on. `MORREEL_LIVE_PORT` sets it
/// explicitly; `MORREEL_LIVE` alone opts in at the default 8177. Unset → offline
/// (edit `.morreel` files).
fn live_port() -> Option<u16> {
    if let Ok(p) = std::env::var("MORREEL_LIVE_PORT") {
        return p.parse().ok();
    }
    std::env::var("MORREEL_LIVE").is_ok().then_some(8177)
}

/// Forward a tool call to the running editor and return its one-line reply. The
/// flat tool args (lane, index, x0…) are reshaped into the plugin's params, whose
/// item reference is nested under `target`.
fn call_live(port: u16, tool: &str, args: &Value) -> Result<String, String> {
    use std::io::{BufRead, Write};
    let mut params = args.as_object().cloned().unwrap_or_default();
    params.remove("project");
    let lane = params.remove("lane");
    let index = params.remove("index");
    if tool != "list_items" {
        params.insert(
            "target".into(),
            json!({ "lane": lane.ok_or("missing 'lane'")?, "index": index.ok_or("missing 'index'")? }),
        );
    }
    let req = json!({ "plugin": plugin_for(tool), "tool": tool, "params": Value::Object(params) });

    let stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("MorReel live app not reachable on 127.0.0.1:{port} ({e}); start the editor or unset MORREEL_LIVE"))?;
    let mut wr = &stream;
    writeln!(wr, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply from editor: {e}"))?;
    if let Some(msg) = resp.get("ok").and_then(Value::as_str) {
        Ok(msg.to_string())
    } else {
        Err(resp.get("error").and_then(Value::as_str).unwrap_or("unknown editor error").to_string())
    }
}

fn load(path: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not valid JSON: {e}"))
}

fn save(path: &str, doc: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("can't write {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_one_clip() -> Value {
        json!({ "clips": [{ "name": "shot.mp4" }], "overlays": [], "audios": [], "titles": [] })
    }

    #[test]
    fn place_box_writes_static_transform_fields() {
        let mut doc = project_with_one_clip();
        let (ox, oy, sx, sy) = BBox::new(0.0, 0.0, 0.5, 0.5).to_transform();
        let t = transform_of(&mut doc, "V1", 0).unwrap();
        set(t, "x", json!(ox));
        set(t, "scale_x", json!(sx));
        let _ = (oy, sy);
        let xf = &doc["clips"][0]["transform"];
        assert_eq!(xf["x"], json!(-0.25));
        assert_eq!(xf["scale_x"], json!(0.5));
    }

    #[test]
    fn track_point_serializes_a_curve_the_editor_can_load() {
        let samples = vec![
            TrackSample { t: 0.0, point: Point::new(0.2, 0.5) },
            TrackSample { t: 2.0, point: Point::new(0.8, 0.5) },
        ];
        let (xc, _yc) = track_curves(&samples, true).unwrap();
        let v = serde_json::to_value(&xc).unwrap();
        // A Curve is an array of keys, each {t, v, interp} — the on-disk shape.
        assert!(v.is_array(), "curve must serialize as an array: {v}");
        assert_eq!(v[0]["interp"], json!("Smooth"));
        // And it round-trips straight back into the shared Animated type.
        let back: Animated<f64> = serde_json::from_value(v).unwrap();
        assert!(back.is_animated());
    }

    #[test]
    fn a_bad_lane_is_an_error_not_a_panic() {
        let mut doc = project_with_one_clip();
        assert!(transform_of(&mut doc, "A1", 0).is_err());
        assert!(transform_of(&mut doc, "V1", 9).is_err());
    }
}
