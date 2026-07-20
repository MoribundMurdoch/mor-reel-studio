// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — portrait-only (9:16) video editor.
// V1: main clip track (trim/reorder/split, ripple by construction).
// V2: full-frame cutaway overlays. A1: audio mixed under. Effects per video item.

mod bevel;
mod engine;

use dioxus::html::HasFileData;
use dioxus::prelude::*;
use engine::{AudioSpec, ClipSpec, OverlaySpec, TitleSpec};
use mor_rust_dioxus_ui_kit::{
    use_shortcut, MenuItem, MenuSeparator, Modal, MorAppFrame, MorMenuDropdown, MorSelect,
    MorShortcutRoot, MorStyleProvider, Slider, UiMode,
};

/// MorReel look: deep-night surround (still near-neutral so color judgment
/// holds), with a light retro MMO HUD smidge — brass-gold for video, gem-teal
/// for audio, title gold, record-red for the playhead. Labels stay plain English;
/// the fantasy is only in the chrome.
const MORREEL_TOML: &str = r##"
bg            = "#101018"
panel         = "#181822"
header        = "#0b0b11"
text          = "#ebe6dc"
text_muted    = "#8e8a96"
border        = "#2a2836"
border_light  = "#413e4f"
accent        = "#d9a441"
accent_hover  = "#efbc58"
btn           = "#252530"
btn_hover     = "#323240"
font_family   = "Cantarell, 'Segoe UI', system-ui, sans-serif"
font_size_base= "13px"
font_size_h1  = "20px"
padding_base  = "8px"
border_radius = "7px"
destructive   = "#e5484d"
success       = "#3dd6c8"
warning       = "#e8c060"
"##;

fn main() {
    let cfg = UiMode::launch_config("MorReel Studio");
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

/// Named looks — (category, name, ffmpeg filter snippet), applied identically
/// to preview frames and export so preview = export. "Motion" ports moranima's
/// BgMotion camera moves (Zoom/Drift/Sway) with the same phase math
/// (ph = 0.1π·t, so 2ph ≈ 0.628t) translated to ffmpeg time-expressions.
// ponytail: moranima's particle overlays (fireflies/snow/embers) need a second
// video input; the per-clip effect slot is one linear chain, so they wait
// until effects can be filter_complex branches. Tilt (animated perspective)
// has no per-frame ffmpeg equivalent — Sway covers the feel.
const EFFECTS: &[(&str, &str, &str)] = &[
    ("Color", "None", ""),
    ("Color", "B&W", "hue=s=0"),
    ("Color", "Sepia", "colorchannelmixer=.393:.769:.189:0:.349:.686:.168:0:.272:.534:.131"),
    ("Color", "Warm", "colortemperature=4500"),
    ("Color", "Cool", "colortemperature=8500"),
    ("Color", "Punch", "eq=contrast=1.18:saturation=1.45"),
    ("Look", "Dreamy", "gblur=sigma=2,eq=brightness=0.04:saturation=1.15"),
    ("Look", "Vignette", "vignette"),
    // Keyed on input time, not on zoompan's own `zoom` accumulator. With d=1
    // (one output frame per input frame) that accumulator resets every frame
    // instead of compounding, so `min(zoom+0.0006,1.25)` pinned this at a flat
    // 1.005x — it never actually zoomed, in the preview or the export. Against
    // input time it ramps as advertised: 0.0006 per frame at 30 fps is 0.018
    // per second, reaching the 1.25 cap in about 14 s.
    ("Motion", "Slow zoom", "zoompan=z='min(1+0.018*it,1.25)':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1080x1920:fps=30,setsar=1"),
    // moranima Zoom: z = 1 + 0.12·(0.5+0.5·sin(2ph)), and 2ph = 0.628t
    ("Motion", "Pulse zoom", "zoompan=z='1.06+0.06*sin(0.628*it)':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1080x1920:fps=30,setsar=1"),
    // moranima Drift: 1.12× overscan, window slides ±0.05w / ±0.03h on offset sines
    ("Motion", "Drift", "scale=1210:2150,crop=1080:1920:x='65+54*sin(0.628*t)':y='115+58*cos(0.408*t)',setsar=1"),
    // moranima Sway: 1.1× overscan hides the corners of a ±0.035 rad rock
    ("Motion", "Sway", "scale=1188:2112,rotate=0.035*sin(0.628*t):ow=1080:oh=1920,setsar=1"),
];

fn effect_filter(name: &str) -> &'static str {
    EFFECTS.iter().find(|(_, n, _)| *n == name).map_or("", |(_, _, f)| f)
}

/// Effect at strength `a` (0..=1): parameters interpolate from identity to the
/// full look, so a=1 is exactly the EFFECTS table and a=0 is no filter. Motion
/// amounts scale amplitude, matching moranima's `amount` semantics.
fn effect_filter_amt(name: &str, a: f64) -> String {
    let a = a.clamp(0.0, 1.0);
    let full = effect_filter(name);
    if full.is_empty() || a <= 0.001 {
        return String::new();
    }
    if a >= 0.999 {
        return full.to_string(); // byte-identical to the table at full strength
    }
    match name {
        "B&W" => format!("hue=s={:.3}", 1.0 - a),
        // sepia matrix lerped toward identity
        "Sepia" => format!(
            "colorchannelmixer={:.3}:{:.3}:{:.3}:0:{:.3}:{:.3}:{:.3}:0:{:.3}:{:.3}:{:.3}",
            1.0 - 0.607 * a, 0.769 * a, 0.189 * a,
            0.349 * a, 1.0 - 0.314 * a, 0.168 * a,
            0.272 * a, 0.534 * a, 1.0 - 0.869 * a
        ),
        "Warm" => format!("colortemperature={:.0}", 6500.0 - 2000.0 * a),
        "Cool" => format!("colortemperature={:.0}", 6500.0 + 2000.0 * a),
        "Punch" => format!("eq=contrast={:.3}:saturation={:.3}", 1.0 + 0.18 * a, 1.0 + 0.45 * a),
        "Dreamy" => format!(
            "gblur=sigma={:.2},eq=brightness={:.3}:saturation={:.3}",
            2.0 * a, 0.04 * a, 1.0 + 0.15 * a
        ),
        "Vignette" => format!("vignette=angle={:.4}", 0.6283 * a), // PI/5 at full
        "Slow zoom" => format!(
            "zoompan=z='min(1+{:.4}*it,{:.3})':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1080x1920:fps=30,setsar=1",
            0.018 * a, 1.0 + 0.25 * a
        ),
        "Pulse zoom" => format!(
            "zoompan=z='{:.3}+{:.3}*sin(0.628*it)':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1080x1920:fps=30,setsar=1",
            1.0 + 0.06 * a, 0.06 * a
        ),
        "Drift" => format!(
            "scale=1210:2150,crop=1080:1920:x='65+{:.1}*sin(0.628*t)':y='115+{:.1}*cos(0.408*t)',setsar=1",
            54.0 * a, 58.0 * a
        ),
        "Sway" => format!(
            "scale=1188:2112,rotate={:.4}*sin(0.628*t):ow=1080:oh=1920,setsar=1",
            0.035 * a
        ),
        _ => full.to_string(),
    }
}

const TITLE_COLORS: &[(&str, &str)] = &[
    ("White", "white"),
    ("Black", "black"),
    ("Gold", "#E8C060"),
    ("Red", "#E5484D"),
    ("Cyan", "#3DD6D0"),
];

const TITLE_POS: &[(&str, f64)] = &[("Top", 0.10), ("Middle", 0.45), ("Lower third", 0.72)];

/// Bevel styles from the mor_cameo_emboss plugin. The stored value keeps the
/// cameo/intaglio lineage; the label says what it actually looks like, the way
/// the designer app words it — "raised" and "sunken" mean something to someone
/// who has never cut a seal.
const BEVELS: &[(&str, &str)] = &[
    ("Off", "Off"),
    ("Cameo", "Raised — stands off the video"),
    ("Intaglio", "Sunken — carved into it"),
];

fn bevel_label(value: &str) -> String {
    BEVELS.iter().find(|(v, _)| *v == value).map_or("Off", |(_, l)| l).to_string()
}

fn bevel_value(label: &str) -> String {
    BEVELS.iter().find(|(_, l)| *l == label).map_or("Off", |(v, _)| v).to_string()
}

/// How a source fills 9:16 — mostly for landscape imports. Crop covers and
/// center-crops, Fit letterboxes on black, Zoom punches in 1.5× then crops.
const FRAMINGS: &[&str] = &["Crop", "Fit", "Zoom"];

/// What V1 and V2 accept: video and photos on the same lanes, since ffmpeg
/// loops a still and a Motion effect turns it into a camera move over it.
fn media_ext() -> Vec<&'static str> {
    engine::VIDEO_EXT.iter().chain(engine::IMAGE_EXT).copied().collect()
}

/// Timeline span a freshly imported source takes: a video keeps its whole
/// length, a still gets a sensible default the Out point can stretch.
fn initial_out(path: &str, duration: f64) -> f64 {
    if engine::is_still(path) { engine::STILL_DEFAULT } else { duration }
}

/// Upload ceilings for a portrait video, shortest first. Going over doesn't
/// break the export — it just means that platform will reject or truncate it,
/// which is worth knowing before you render rather than after.
// ponytail: static table, not a fetched policy — platforms change these rarely
// and a stale number here is a nudge, not a hard block.
const LIMITS: &[(&str, f64)] = &[("Shorts", 60.0), ("Reels", 90.0), ("TikTok", 600.0)];

/// Which platforms the reel has outgrown, e.g. "over Shorts 1:00.0".
/// None while it still fits everywhere.
fn over_limits(total: f64) -> Option<String> {
    let over: Vec<String> = LIMITS
        .iter()
        .filter(|(_, cap)| total > *cap)
        .map(|(name, cap)| format!("{name} {}", fmt_t(*cap)))
        .collect();
    (!over.is_empty()).then(|| format!("over {}", over.join(", ")))
}

fn title_color(name: &str) -> &'static str {
    TITLE_COLORS.iter().find(|(n, _)| *n == name).map_or("white", |(_, c)| c)
}

fn title_y(name: &str) -> f64 {
    TITLE_POS.iter().find(|(n, _)| *n == name).map_or(0.45, |(_, y)| *y)
}

/// Greedy word-wrap for caption cards — drawtext has no auto-wrap, so long
/// transcript segments would run off the 1080px frame.
fn wrap_caption(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for w in text.split_whitespace() {
        let wlen = w.chars().count();
        if line > 0 && line + 1 + wlen > max {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(w);
        line += wlen;
    }
    out
}

/// Rasterize every card a title is made of — one normally, one per word when
/// the words come in individually. Content-addressed, so re-rendering after an
/// edit only pays for the steps that actually changed.
async fn render_one(t: &TitleItem) -> Result<Vec<String>, String> {
    let mut cards = Vec::new();
    for (text, _, _) in t.segments() {
        cards.push(engine::render_title(&t.style_of(&text)).await?);
    }
    Ok(cards)
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct TitleItem {
    text: String,
    at: f64,
    dur: f64,
    font_size: f64,
    color: String,
    pos: String,
    bevel: String,
    bevel_size: f64,
    /// Any installed fontconfig family.
    font: String,
    /// How multiple lines line up: "Centre" | "Left" | "Right".
    #[serde(default = "centre")]
    align: String,
    /// How the card arrives and leaves; see engine::TITLE_ANIMS.
    #[serde(default = "none_label")]
    anim: String,
    /// Bring the words in one at a time instead of all at once — the caption
    /// style every phone editor has. Costs one rendered card per word.
    #[serde(default)]
    reveal: bool,
    /// "Text" or one of the shapes. A shape is a T-lane card like any other —
    /// it just draws a box instead of words.
    #[serde(default = "text_kind")]
    kind: String,
    #[serde(default = "shape_w_default")]
    shape_w: f64,
    #[serde(default = "shape_h_default")]
    shape_h: f64,
    #[serde(default)]
    shape_x: f64,
    /// Semi-opaque backdrop box behind the text (caption legibility).
    boxed: bool,
    /// Outline width in px, 0 = none — legibility without an opaque plate.
    #[serde(default)]
    outline: f64,
    #[serde(default = "black")]
    outline_color: String,
    /// The rest of the bevel's own controls. Defaults match the designer app
    /// this bevel came from, so an older project loads looking as it did.
    #[serde(default = "bevel_soften")]
    soften: f64,
    #[serde(default = "bevel_depth")]
    depth: f64,
    #[serde(default = "bevel_angle")]
    angle: f64,
    #[serde(default = "bevel_altitude")]
    altitude: f64,
    #[serde(default = "bevel_opacity")]
    hi_opacity: f64,
    #[serde(default = "bevel_opacity")]
    sh_opacity: f64,
    /// Made by Auto captions — lets "Remove captions" clear them in bulk.
    caption: bool,
    /// Rendered cards, one per revealed step (just one unless the words
    /// come in one at a time). Empty while a render is in flight.
    #[serde(skip)]
    pngs: Vec<String>,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
}

/// The palette name, not the CSS colour — `title_color` looks these up by
/// display name and falls back to white on a miss.
fn black() -> String {
    "Black".to_string()
}
fn bevel_soften() -> f64 {
    4.0
}
fn bevel_depth() -> f64 {
    100.0
}
fn bevel_angle() -> f64 {
    120.0
}
fn bevel_altitude() -> f64 {
    30.0
}
fn bevel_opacity() -> f64 {
    0.75
}

/// Which part of the on-screen transform box is being dragged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum XfGrab {
    Move,
    Scale,
    Rotate,
}

/// The four corners of the transform box, as fractions across the monitor
/// (0,0 = top left, 1,1 = bottom right).
///
/// The rotation has to be applied in *pixel* space and converted back, or on a
/// 9:16 frame a rotated box comes out sheared: one fraction of width is not the
/// same distance as one fraction of height.
fn xf_corners(t: &engine::Transform) -> [(f64, f64); 4] {
    let half = t.scale / 2.0;
    let (sin, cos) = t.rotation.to_radians().sin_cos();
    let ar = engine::W as f64 / engine::H as f64;
    // The box turns about its own centre, and that centre is where the
    // position offset put it.
    let (cx, cy) = (0.5 + t.x, 0.5 + t.y);
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(sx, sy)| {
        let (fx, fy) = (sx * half, sy * half);
        (cx + (fx * cos - fy * sin / ar), cy + (fx * sin * ar + fy * cos))
    })
}

/// A dragged handle updates the transform from where the pointer is now,
/// relative to where it went down and to the centre of the frame on screen.
/// `rect` is the monitor's on-screen box: (left, top, width, height).
fn xf_apply(
    grab: XfGrab,
    start: engine::Transform,
    from: (f64, f64),
    to: (f64, f64),
    rect: (f64, f64, f64, f64),
) -> engine::Transform {
    let (rl, rt, rw, rh) = rect;
    if rw < 1.0 || rh < 1.0 {
        return start;
    }
    let (cx, cy) = (rl + rw / 2.0, rt + rh / 2.0);
    let mut t = start;
    match grab {
        XfGrab::Move => {
            t.x = start.x + (to.0 - from.0) / rw;
            t.y = start.y + (to.1 - from.1) / rh;
        }
        XfGrab::Scale => {
            // Ratio of distances from the centre, so grabbing any corner (or a
            // corner clamped back into view) scales the same way.
            let d0 = ((from.0 - cx).powi(2) + (from.1 - cy).powi(2)).sqrt();
            let d1 = ((to.0 - cx).powi(2) + (to.1 - cy).powi(2)).sqrt();
            if d0 > 2.0 {
                t.scale = (start.scale * d1 / d0).clamp(0.1, 4.0);
            }
        }
        XfGrab::Rotate => {
            let a0 = (from.1 - cy).atan2(from.0 - cx);
            let a1 = (to.1 - cy).atan2(to.0 - cx);
            // Keep it in the -180..180 the slider shows.
            let deg = start.rotation + (a1 - a0).to_degrees();
            t.rotation = (deg + 180.0).rem_euclid(360.0) - 180.0;
        }
    }
    t
}

/// Shape size and offset, as fractions of the frame. Vertical placement reuses
/// the Position control a title already has.
type ShapeKnob = (&'static str, f64, fn(&mut TitleItem, f64));

fn shape_knobs(t: &TitleItem) -> Vec<ShapeKnob> {
    let set_w: fn(&mut TitleItem, f64) = |i, v| i.shape_w = v;
    vec![
        ("Width", t.shape_w, set_w),
        ("Height", t.shape_h, |i, v| i.shape_h = v),
        ("Across", t.shape_x, |i, v| i.shape_x = v),
    ]
}

/// One row of the Transform panel: label, value, min, max, step, and how to
/// write it back. Both lanes carry the same struct, so one table serves both.
type XformKnob = (&'static str, f64, f64, f64, f64, fn(&mut engine::Transform, f64));

/// Opacity is only offered where it composites over something — on V1 there is
/// nothing underneath it but black.
fn transform_knobs(t: &engine::Transform, with_opacity: bool) -> Vec<XformKnob> {
    let set_scale: fn(&mut engine::Transform, f64) = |x, v| x.scale = v;
    let mut knobs: Vec<XformKnob> = vec![
        ("Scale", t.scale, 0.1, 4.0, 0.01, set_scale),
        ("Position X", t.x, -1.0, 1.0, 0.005, |x, v| x.x = v),
        ("Position Y", t.y, -1.0, 1.0, 0.005, |x, v| x.y = v),
        ("Rotation", t.rotation, -180.0, 180.0, 1.0, |x, v| x.rotation = v),
    ];
    if with_opacity {
        knobs.push(("Opacity", t.opacity, 0.0, 1.0, 0.01, |x, v| x.opacity = v));
    }
    knobs
}

/// One row of the bevel panel: label, current value, max, step, and how to
/// write it back. A table beats seven near-identical slider blocks, and the
/// ranges are the designer app's.
type BevelKnob = (&'static str, f64, f64, f64, fn(&mut TitleItem, f64));

fn bevel_knobs(t: &TitleItem) -> Vec<BevelKnob> {
    let set_size: fn(&mut TitleItem, f64) = |i, v| i.bevel_size = v;
    vec![
        ("Size", t.bevel_size, 100.0, 1.0, set_size),
        ("Softness", t.soften, 100.0, 1.0, |i, v| i.soften = v),
        ("Depth", t.depth, 100.0, 1.0, |i, v| i.depth = v),
        ("Light angle", t.angle, 360.0, 5.0, |i, v| i.angle = v),
        ("Light height", t.altitude, 90.0, 5.0, |i, v| i.altitude = v),
        ("Highlight strength", t.hi_opacity, 1.0, 0.05, |i, v| i.hi_opacity = v),
        ("Shadow strength", t.sh_opacity, 1.0, 0.05, |i, v| i.sh_opacity = v),
    ]
}

/// A word-by-word reveal finishes this far into the card's life, so the
/// finished line still holds long enough to read.
const REVEAL_SPAN: f64 = 0.6;

/// Prefixes of `text` ending at each word, cut out of the original string so
/// the line breaks it already has survive exactly. Rejoining split words with
/// spaces would unwrap a caption and make it jump between lines mid-reveal.
fn word_prefixes(text: &str) -> Vec<String> {
    let mut ends = Vec::new();
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if in_word {
                ends.push(i);
            }
            in_word = false;
        } else {
            in_word = true;
        }
    }
    if in_word {
        ends.push(text.len());
    }
    ends.into_iter().map(|e| text[..e].to_string()).collect()
}

impl TitleItem {
    /// The cards this title is actually made of: (text, start, length). One
    /// card normally; one per word when the words come in individually.
    fn segments(&self) -> Vec<(String, f64, f64)> {
        let steps = word_prefixes(&self.text);
        if !self.reveal || self.kind != "Text" || steps.len() < 2 {
            return vec![(self.text.clone(), self.at, self.dur)];
        }
        let n = steps.len();
        let step = self.dur * REVEAL_SPAN / n as f64;
        steps
            .into_iter()
            .enumerate()
            .map(|(k, text)| {
                let at = self.at + k as f64 * step;
                // The last word holds until the card is done.
                let end =
                    if k + 1 == n { self.at + self.dur } else { self.at + (k + 1) as f64 * step };
                (text, at, (end - at).max(0.01))
            })
            .collect()
    }

    /// Which card is on screen at `t`, if any.
    fn card_at(&self, t: f64) -> Option<usize> {
        self.segments().iter().position(|(_, at, dur)| t >= *at && t < at + dur)
    }

    /// Map the timeline item onto the engine's render parameters. The item
    /// stores friendly choices (a colour name, a position name); the style
    /// stores what ffmpeg and the bevel actually need.
    /// This card's look, carrying whichever words this step shows.
    fn style_of(&self, text: &str) -> engine::TitleStyle {
        engine::TitleStyle {
            text: text.to_string(),
            font_size: self.font_size as u32,
            color: title_color(&self.color).to_string(),
            y_frac: title_y(&self.pos),
            font: self.font.clone(),
            align: self.align.clone(),
            outline: self.outline,
            outline_color: title_color(&self.outline_color).to_string(),
            boxed: self.boxed,
            kind: self.kind.clone(),
            shape_w: self.shape_w,
            shape_h: self.shape_h,
            shape_x: self.shape_x,
            bevel: self.bevel.clone(),
            bevel_size: self.bevel_size,
            soften: self.soften,
            depth: self.depth,
            angle: self.angle,
            altitude: self.altitude,
            hi_opacity: self.hi_opacity,
            sh_opacity: self.sh_opacity,
        }
    }
}

/// The export fade's opacity at global time `t`, for the scrub preview.
fn title_alpha(t: f64, at: f64, dur: f64) -> f64 {
    let f = engine::title_fade(dur).max(0.01);
    ((t - at) / f).min((at + dur - t) / f).clamp(0.0, 1.0)
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Clip {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    has_audio: bool,
    effect: String,
    /// Effect strength 0..=1 (parameter interpolation, not a crossfade).
    effect_amount: f64,
    framing: String,
    /// Where the picture sits in the frame — scale, position, rotation.
    #[serde(default)]
    transform: engine::Transform,
    /// Playback rate: 0.5 is slow motion, 2.0 is double speed.
    #[serde(default = "unity")]
    speed: f64,
    /// Gain on this clip's own audio; 0.0 mutes it.
    #[serde(default = "unity")]
    volume: f64,
    /// Transition *into* this clip. Stored on the incoming clip so it survives
    /// reordering, and ignored on the first clip — nothing precedes it.
    #[serde(default = "none_label")]
    transition: String,
    #[serde(default = "half")]
    trans_dur: f64,
    #[serde(skip)]
    thumb: String,
    /// Full-source waveform data URI for this clip's own audio; empty until the
    /// background render lands, and always empty for a silent source.
    #[serde(skip)]
    wave: String,
    /// 480p scrub proxy path; empty until the background build finishes.
    #[serde(skip)]
    proxy: String,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
}

impl Clip {
    fn spec(&self) -> ClipSpec {
        ClipSpec {
            path: self.path.clone(),
            in_s: self.in_s,
            out_s: self.out_s,
            has_audio: self.has_audio,
            effect: self.look(),
            framing: self.framing.clone(),
            speed: self.speed,
            volume: self.volume,
            transition: self.transition.clone(),
            trans_dur: self.trans_dur,
        }
    }

    /// The whole video chain for this clip: geometry first, then the look.
    /// Every preview, thumbnail and export goes through here, so they cannot
    /// drift apart.
    fn look(&self) -> String {
        join_chain(
            engine::transform_chain(&self.transform, engine::W, engine::H, false),
            effect_filter_amt(&self.effect, self.effect_amount),
        )
    }

    /// Seconds on the timeline — the source span retimed by the speed.
    fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// What preview/scrub extraction should read: the proxy once built.
    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

/// The inspector's one-line summary of a V1 clip. A still has no source length
/// to report and needs no proxy, so it reads differently from a video.
fn clip_note(c: &Clip) -> String {
    if engine::is_still(&c.path) {
        return format!(
            "Photo • holding {} — drag Out to hold it longer, or add a Motion effect",
            fmt_t(c.trimmed())
        );
    }
    format!(
        "{} source • keeping {}{}{}{}",
        fmt_t(c.duration),
        fmt_t(c.trimmed()),
        if (c.speed - 1.0).abs() > 0.01 { format!(" at {:.2}×", c.speed) } else { String::new() },
        if c.has_audio { "" } else { " • no audio" },
        if c.proxy.is_empty() { " • building proxy…" } else { " • proxy" },
    )
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct OverlayItem {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    at: f64,
    effect: String,
    /// Effect strength 0..=1 (parameter interpolation, not a crossfade).
    effect_amount: f64,
    framing: String,
    /// Where the picture sits in the frame. A scaled-down overlay is a
    /// picture-in-picture, since V2 composites over V1.
    #[serde(default)]
    transform: engine::Transform,
    /// Playback rate, same as a V1 clip: 0.5 is slow motion.
    #[serde(default = "unity")]
    speed: f64,
    #[serde(skip)]
    proxy: String,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
}

impl OverlayItem {
    /// Seconds this cutaway covers V1 for — its source span, retimed.
    fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// Same as a clip's, but built for a layer that composites: the area the
    /// picture vacates is transparent, so V1 shows through around it.
    fn look(&self) -> String {
        join_chain(
            engine::transform_chain(&self.transform, engine::W, engine::H, true),
            effect_filter_amt(&self.effect, self.effect_amount),
        )
    }

    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct AudioItem {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    at: f64,
    volume: f64,
    /// How hard this bed ducks under the main track while it is talking.
    /// 0 = never. Music under a voiceover is the reason this exists.
    #[serde(default)]
    duck: f64,
    /// Full-source waveform data URI; empty until the background render lands.
    #[serde(skip)]
    wave: String,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
}

/// Inline CSS windowing a full-source waveform image to the span an item keeps.
/// The image spans the whole source, so it is stretched to `duration` seconds
/// wide and shifted left by the in point; trims and splits are then free, since
/// they only move this window rather than re-rendering anything.
///
/// `speed` compresses both, so a retimed V1 clip's waveform still lines up with
/// its retimed width on the timeline. A1 items are never retimed and pass 1.0.
fn wave_css(wave: &str, duration: f64, in_s: f64, scale: f64, speed: f64) -> String {
    if wave.is_empty() {
        return String::new();
    }
    let px = scale / speed.max(0.01);
    format!(
        "background-image:url({wave});background-size:{:.1}px 100%;\
         background-position:-{:.1}px 0;background-repeat:no-repeat;",
        duration * px,
        in_s * px
    )
}

/// Serde fallback for rate-like fields, so a project written before they
/// existed loads as 1x rather than as a divide-by-zero.
fn unity() -> f64 {
    1.0
}
fn none_label() -> String {
    "None".to_string()
}
fn centre() -> String {
    "Centre".to_string()
}
fn text_kind() -> String {
    "Text".to_string()
}
fn shape_w_default() -> f64 {
    0.6
}
fn shape_h_default() -> f64 {
    0.12
}
/// Default transition length. Short, because a reel cut is quick.
fn half() -> f64 {
    0.5
}

/// A saved title look, kept outside any project so a series of reels can share
/// one. The whole item is stored and only its styling is applied — that way a
/// preset gains any field a title gains, with nothing to keep in step.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct TitlePreset {
    name: String,
    style: TitleItem,
}

fn presets_path() -> std::path::PathBuf {
    engine::config_dir().join("title-presets.json")
}

fn load_presets() -> Vec<TitlePreset> {
    std::fs::read_to_string(presets_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_presets(all: &[TitlePreset]) -> Result<(), String> {
    let dir = engine::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(presets_path(), json).map_err(|e| e.to_string())
}

/// Take `src`'s look but keep `dst`'s own words, timing and lane identity.
/// A style is everything a card looks like; it is never what it says or when.
fn restyle(dst: &TitleItem, src: &TitleItem) -> TitleItem {
    TitleItem {
        text: dst.text.clone(),
        at: dst.at,
        dur: dst.dur,
        group: dst.group,
        caption: dst.caption,
        pngs: Vec::new(),
        ..src.clone()
    }
}

/// A whole-timeline snapshot for undo/redo. Snapshotting every lane is cheaper
/// to get *right* than per-action inverse operations — those silently miss a
/// field the moment someone adds one — and a reel is tens of items, not
/// thousands. Rendered PNGs and proxies ride along; they're content-addressed,
/// so restoring an old one costs a cache hit, not a re-render.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Snapshot {
    clips: Vec<Clip>,
    overlays: Vec<OverlayItem>,
    audios: Vec<AudioItem>,
    titles: Vec<TitleItem>,
    /// Beat markers, in timeline seconds, sorted. Not a lane — they hold no
    /// media, they are just the places you want cuts to land.
    #[serde(default)]
    markers: Vec<f64>,
}

/// What the inspector is editing.
#[derive(Clone, Copy, PartialEq)]
enum Sel {
    Main(usize),
    Over(usize),
    Aud(usize),
    Title(usize),
}

/// What was right-clicked; picks the context menu's contents.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Monitor,
    Timeline,
    Clip(usize),
    Over(usize),
    Aud(usize),
    Title(usize),
}

/// The transition leading into clip `i`, clamped to something both it and the
/// clip before it can accommodate. 0 for a cut, and always 0 for the first clip.
fn fade_in(clips: &[Clip], i: usize) -> f64 {
    if i == 0 || i >= clips.len() {
        return 0.0;
    }
    if engine::xfade_name(&clips[i].transition).is_empty() {
        return 0.0;
    }
    clips[i].trans_dur.clamp(0.0, clips[i].trimmed().min(clips[i - 1].trimmed()) * 0.9)
}

/// How much of the timeline each clip owns. A transition overlaps a clip's tail
/// with the next clip's head, so a clip followed by one owns less than it runs
/// for. These sum to the finished length, and every position on the timeline —
/// the ruler, the playhead, the magnet, clip widths — is derived from them.
fn extents(clips: &[Clip]) -> Vec<f64> {
    (0..clips.len())
        .map(|i| (clips[i].trimmed() - fade_in(clips, i + 1)).max(0.05))
        .collect()
}

/// If `t` falls inside the transition leading into some clip, return that
/// clip's index, how far the blend has run (0..1), and the source time to pull
/// its frame from. The overlap sits at the end of the outgoing clip's extent,
/// which is exactly where the next clip's own footage has already started.
fn transition_at(clips: &[Clip], t: f64) -> Option<(usize, f64, f64)> {
    let ext = extents(clips);
    let mut start = 0.0;
    for i in 0..clips.len() {
        let end = start + ext[i];
        let d = fade_in(clips, i + 1);
        if d > 0.0 && t >= end - d && t < end {
            let progress = ((t - (end - d)) / d).clamp(0.0, 1.0);
            let next = &clips[i + 1];
            // The incoming clip is already running during the overlap.
            let into = (t - (end - d)) * next.speed.max(0.01);
            return Some((i + 1, progress, next.in_s + into));
        }
        start = end;
    }
    None
}

/// Map a global timeline position to (clip index, source-file time) on V1.
/// A retimed clip covers `speed` seconds of source per timeline second.
fn locate(clips: &[Clip], t: f64) -> Option<(usize, f64)> {
    let mut acc = 0.0;
    for (i, d) in extents(clips).into_iter().enumerate() {
        let c = &clips[i];
        if t < acc + d || i + 1 == clips.len() {
            return Some((i, c.in_s + (t - acc).clamp(0.0, d) * c.speed.max(0.01)));
        }
        acc += d;
    }
    None
}

fn fmt_t(s: f64) -> String {
    let s = if s > 0.0 { s } else { 0.0 }; // squash -0.0 → "0:-0.0"
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// Timeline item class string: base + selection + group-mark states.
fn item_class(base: &str, sel: bool, mark: bool) -> String {
    format!("{base}{}{}", if sel { " selected" } else { "" }, if mark { " marked" } else { "" })
}

/// A cut at source-time `local` is valid only if both halves keep at least
/// `min` seconds; returns the cut point when it is.
fn cut_local(in_s: f64, out_s: f64, local: f64, min: f64) -> Option<f64> {
    (local >= in_s + min && local <= out_s - min).then_some(local)
}

/// Magnetic timeline: how far an item anchored at `at` moves when a V1 edit
/// rearranges the clips. `old` is each clip's (start, end) span before the
/// edit; `new_start` maps an old clip index to its start after the edit
/// (None = clip deleted). Unattached or orphaned items hold position.
fn magnet_delta(at: f64, old: &[(f64, f64)], new_start: impl Fn(usize) -> Option<f64>) -> f64 {
    old.iter()
        .position(|&(s, e)| at >= s && at < e)
        .and_then(|k| new_start(k).map(|ns| ns - old[k].0))
        .unwrap_or(0.0)
}

/// A timeline lane, as a drop target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    V1,
    V2,
    A1,
}

/// What a file is, by extension. An unknown extension is treated as video:
/// `probe` decides for real, and its no-duration fallback catches images in
/// containers these tables have never heard of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Video,
    Still,
    Audio,
}

fn kind_of(path: &str) -> Kind {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if engine::AUDIO_EXT.contains(&ext.as_str()) {
        Kind::Audio
    } else if engine::IMAGE_EXT.contains(&ext.as_str()) {
        Kind::Still
    } else {
        Kind::Video
    }
}

/// Comma-join two filter chains, either of which may be empty.
fn join_chain(a: String, b: String) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => format!("{a},{b}"),
    }
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Where a dropped file actually lands. The lane you drop on says what you
/// meant, but the file has the final say: sound can't go on a video track and
/// a photo has nothing to contribute to an audio one. Returns the lane to use,
/// plus a note when that differs from where it was dropped.
fn route_drop(kind: Kind, onto: Lane) -> Result<(Lane, Option<&'static str>), &'static str> {
    match (kind, onto) {
        // Sound is sound wherever it was aimed — quietly send it to A1.
        (Kind::Audio, Lane::A1) => Ok((Lane::A1, None)),
        (Kind::Audio, _) => Ok((Lane::A1, Some("audio goes to A1"))),
        // A video dropped on A1 contributes its soundtrack.
        (Kind::Video, Lane::A1) => Ok((Lane::A1, Some("using its soundtrack"))),
        (Kind::Still, Lane::A1) => Err("a photo has no sound to mix"),
        (_, lane) => Ok((lane, None)),
    }
}

/// Which index a drop at `t` seconds should insert before on V1. The main track
/// is a concat with no gaps, so a drop can only ever mean "between these two
/// clips" — never "at 12.4s". Past the midpoint of a clip means after it.
fn insert_index(clips: &[Clip], t: f64) -> usize {
    let mut acc = 0.0;
    for (i, d) in extents(clips).into_iter().enumerate() {
        if t < acc + d / 2.0 {
            return i;
        }
        acc += d;
    }
    clips.len()
}

/// Snap `at` to the nearest of `targets` within `tol` seconds, else leave it.
/// Targets are the things an editor actually lines up against: clip cuts, the
/// end of the reel, and the playhead.
fn snap_to(at: f64, targets: &[f64], tol: f64) -> f64 {
    targets
        .iter()
        .copied()
        .filter(|t| (t - at).abs() <= tol)
        .min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs()))
        .unwrap_or(at)
}

/// Old→new index for a drag-reorder: clips[lo..lo+len] move so the block
/// starts at `dest`, where `dest` indexes the sequence with the block removed.
fn block_map(k: usize, lo: usize, len: usize, dest: usize) -> usize {
    if k >= lo && k < lo + len {
        dest + (k - lo)
    } else {
        let w = if k < lo { k } else { k - len };
        if w < dest { w } else { w + len }
    }
}

#[component]
fn App() -> Element {
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        MorShortcutRoot { Editor {} }
    }
}

/// Where a phone app's own chrome sits over a 9:16 feed: the top header strip,
/// the right-hand action rail (like / comment / share), and the bottom caption
/// and username block. Anything you keep inside the clear middle reads on
/// TikTok, Reels and Shorts alike.
// ponytail: one worst-case mask across the three, not per-platform presets —
// these are the widest margins of the set, so clearing them clears all of them.
// Percentages are of the 1080x1920 frame and are deliberately generous; nudge
// them in the CSS if a platform moves its furniture.
#[component]
fn SafeArea() -> Element {
    rsx! {
        div { class: "mr-safe",
            div { class: "mr-safe-zone mr-safe-top", span { "header" } }
            div { class: "mr-safe-zone mr-safe-rail", span { "actions" } }
            div { class: "mr-safe-zone mr-safe-bottom", span { "caption / username" } }
        }
    }
}

/// Program monitor window: just the phone, fed by the editor's preview signal.
/// Runs in its own VirtualDom, so it gets its own style provider.
#[component]
fn Monitor(preview: Signal<String>, out: Signal<bool>, safe: Signal<bool>) -> Element {
    // Closing the window drops this VirtualDom — that drop is what docks the
    // monitor back into the editor.
    use_drop(move || out.set(false));
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        div { class: "mr-monitor",
            // The webview's default menu (Reload/Inspect) is noise in a monitor.
            oncontextmenu: move |evt| evt.prevent_default(),
            div { class: "mr-phone",
                if preview().is_empty() {
                    span { "Add clips to preview your reel" }
                } else {
                    img { src: "{preview}" }
                }
                if safe() {
                    SafeArea {}
                }
            }
        }
    }
}

/// One row of the right-click menu: kit menu styling, runs the action; the
/// click bubbles to the backdrop, which closes the menu. Shortcuts are
/// display-only — the live binds stay on the app menu.
#[component]
fn CtxItem(
    label: String,
    #[props(default = None)] shortcut: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] danger: bool,
    on_action: EventHandler<()>,
) -> Element {
    let class = match (disabled, danger) {
        (true, _) => "mor-menu-item mor-menu-action disabled",
        (false, true) => "mor-menu-item mor-menu-action mr-danger",
        (false, false) => "mor-menu-item mor-menu-action",
    };
    rsx! {
        button {
            class,
            disabled,
            onclick: move |_| on_action.call(()),
            span { class: "mor-menu-action-label", "{label}" }
            if let Some(sc) = shortcut {
                span { class: "shortcut", "{sc}" }
            }
        }
    }
}

#[component]
fn Editor() -> Element {
    let mut clips = use_signal(Vec::<Clip>::new);
    let mut overlays = use_signal(Vec::<OverlayItem>::new);
    let mut audios = use_signal(Vec::<AudioItem>::new);
    let mut titles = use_signal(Vec::<TitleItem>::new);
    let mut selected = use_signal(|| Option::<Sel>::None);
    let mut playhead = use_signal(|| 0.0f64); // global timeline seconds
    let mut preview = use_signal(String::new);
    let mut status = use_signal(|| "Ready — add clips to start.".to_string());
    let mut export_progress = use_signal(|| Option::<f64>::None);
    let mut importing = use_signal(|| false);

    let total_of = move || extents(&clips.read()).iter().sum::<f64>();

    // Right-click menu: (viewport x, y, what was clicked). One menu, many targets.
    let mut ctx_menu = use_signal(|| Option::<(f64, f64, Ctx)>::None);
    let mut open_ctx = move |evt: Event<MouseData>, target: Ctx| {
        evt.prevent_default(); // replaces the webview's Reload/Inspect menu
        evt.stop_propagation();
        let p = evt.client_coordinates();
        ctx_menu.set(Some((p.x, p.y, target)));
    };

    // Preview extraction: latest-wins queue so slider drags don't stack ffmpeg runs.
    // Whatever is composited on top — a title's fade, the incoming half of a
    // transition — rides along so the scrub shows what the export will.
    let mut pending = use_signal(|| Option::<(String, f64, String, String, engine::Over)>::None);
    let mut preview_busy = use_signal(|| false);
    let mut request_preview =
        move |path: String, t: f64, framing: String, effect: String, over: engine::Over| {
            pending.set(Some((path, t, framing, effect, over)));
            if preview_busy() {
                return;
            }
            preview_busy.set(true);
            spawn(async move {
                loop {
                    // Take-then-drop: a `while let` scrutinee guard would stay
                    // borrowed across the await, and any scrub event's pending.set
                    // during extraction panics (AlreadyBorrowedMut → abort).
                    let next = pending.write().take();
                    let Some((p, t, fr, e, ov)) = next else { break };
                    if let Ok(uri) = engine::frame_data_uri(&p, t, 540, 960, &fr, &e, ov).await {
                        preview.set(uri);
                    }
                }
                preview_busy.set(false);
            });
        };

    // Proxy builds: one at a time in the background; when a proxy lands, every
    // item using that source switches its scrub path over.
    let mut proxy_queue = use_signal(Vec::<String>::new);
    let mut proxy_busy = use_signal(|| false);
    let mut queue_proxy = move |path: String| {
        proxy_queue.write().push(path);
        if proxy_busy() {
            return;
        }
        proxy_busy.set(true);
        spawn(async move {
            loop {
                let next = {
                    let mut q = proxy_queue.write();
                    if q.is_empty() { None } else { Some(q.remove(0)) }
                };
                let Some(src) = next else { break };
                match engine::ensure_proxy(&src).await {
                    Ok(proxy) => {
                        for c in clips.write().iter_mut().filter(|c| c.path == src) {
                            c.proxy = proxy.clone();
                        }
                        for o in overlays.write().iter_mut().filter(|o| o.path == src) {
                            o.proxy = proxy.clone();
                        }
                    }
                    Err(e) => status.set(format!("Proxy build failed (scrubbing uses the original): {e}")),
                }
            }
            proxy_busy.set(false);
        });
    };

    // Seek: playhead moves, selection follows the V1 clip underneath, preview
    // shows whatever is on top (a V2 overlay covers V1 while it runs).
    let mut seek_to = move |t: f64| {
        playhead.set(t);
        // Topmost title active at t, composited onto the preview frame.
        let title_png = titles
            .read()
            .iter()
            .rev()
            .find(|ti| t >= ti.at && t < ti.at + ti.dur && !ti.pngs.is_empty())
            .and_then(|ti| {
                // A revealed title is a run of cards; show whichever is up now,
                // faded against the whole title rather than its own step.
                let k = ti.card_at(t).unwrap_or(0).min(ti.pngs.len().saturating_sub(1));
                ti.pngs.get(k).map(|p| (p.clone(), title_alpha(t, ti.at, ti.dur)))
            });
        let over = overlays.read().iter().find(|o| t >= o.at && t < o.at + o.trimmed()).map(
            |o| {
                (
                    o.scrub_path(),
                    o.in_s + (t - o.at),
                    o.framing.clone(),
                    o.look(),
                )
            },
        );
        let loc = locate(&clips.read(), t);
        if let Some((i, _)) = loc {
            if selected() != Some(Sel::Main(i)) {
                selected.set(Some(Sel::Main(i)));
            }
        }
        let mut layers = engine::Over { title: title_png, ..Default::default() };
        if let Some((path, local, fr, eff)) = over {
            request_preview(path, local, fr, eff, layers);
        } else if let Some((i, local)) = loc {
            let (path, fr, eff) = {
                let cl = clips.read();
                (cl[i].scrub_path(), cl[i].framing.clone(), cl[i].look())
            };
            // Inside a transition the export is showing both clips at once, so
            // the monitor has to as well: the outgoing clip is the base and the
            // incoming one fades up over it by however far the blend has run.
            // Without this, scrubbing a dissolve would show a hard cut.
            if let Some((next, alpha, ntime)) = transition_at(&clips.read(), t) {
                let cl = clips.read();
                layers.blend = Some((
                    cl[next].scrub_path(),
                    ntime,
                    cl[next].framing.clone(),
                    cl[next].look(),
                    alpha,
                ));
            }
            request_preview(path, local, fr, eff, layers);
        }
    };

    // Re-render a title card after any edit; content-addressed, so unchanged
    // params are a cache hit. Refreshes the preview when the render lands.
    let rerender_title = move |k: usize| {
        let Some(t) = titles.read().get(k).cloned() else { return };
        spawn(async move {
            match render_one(&t).await {
                Ok(png) => {
                    if let Some(item) = titles.write().get_mut(k) {
                        item.pngs = png;
                    }
                    seek_to(playhead());
                }
                Err(e) => status.set(format!("Title render failed: {e}")),
            }
        });
    };

    let start_of = move |i: usize| -> f64 {
        extents(&clips.read()).iter().take(i).sum()
    };

    let mut select_clip = move |i: usize| {
        seek_to(start_of(i));
    };

    // Magnetic timeline: V2/A1/T items anchor to the V1 clip under their start
    // point, so trims, moves and ripple deletes carry them along. ~ toggles it
    // off to edit V1 while attached items hold position (this timeline has no
    // dragging, so "hold ~ while dragging" becomes a toggle).
    // ponytail: anchors are positional, not content ids — an item re-anchors if
    // an edit puts a different clip under it.
    let mut magnet = use_signal(|| true);
    let spans = move || -> Vec<(f64, f64)> {
        let mut acc = 0.0;
        extents(&clips.read()).into_iter().map(|d| { let s = acc; acc += d; (s, acc) }).collect()
    };
    let mut ride = move |old: Vec<(f64, f64)>, new_start: &dyn Fn(usize) -> Option<f64>| {
        if !magnet() {
            return;
        }
        for o in overlays.write().iter_mut() {
            o.at = (o.at + magnet_delta(o.at, &old, new_start)).max(0.0);
        }
        for a in audios.write().iter_mut() {
            a.at = (a.at + magnet_delta(a.at, &old, new_start)).max(0.0);
        }
        for t in titles.write().iter_mut() {
            t.at = (t.at + magnet_delta(t.at, &old, new_start)).max(0.0);
        }
    };
    let mut toggle_magnet = move |_: ()| {
        magnet.toggle();
        status.set(if magnet() {
            "Magnetic timeline on — attached V2/A1/T items ride V1 edits."
        } else {
            "Magnetic timeline off — V1 edits leave attached items in place."
        }
        .to_string());
    };

    // Grouping: Ctrl+click marks items across any lane, Ctrl+G links the marks
    // into a group (0 = ungrouped). Grouped items drag together; grouped V1
    // clips reorder as a contiguous block.
    // ponytail: marks are positional Sels, so structural edits clear them.
    let mut marked = use_signal(Vec::<Sel>::new);
    let mut next_group = use_signal(|| 1usize);
    // Undo/redo. `push_undo` records the state *before* an edit; a non-empty
    // tag collapses a run of edits that share it, so one slider drag is one
    // undo step instead of forty. Discrete actions pass "" and never collapse.
    let mut undo_stack = use_signal(Vec::<Snapshot>::new);
    let mut redo_stack = use_signal(Vec::<Snapshot>::new);
    let mut undo_tag = use_signal(String::new);
    // Beat markers: tap M along to the music while it plays and you get the
    // grid you actually want to cut on. They are snap targets, so a dragged
    // item lands on the beat instead of near it.
    let mut markers = use_signal(Vec::<f64>::new);
    let snapshot = move || Snapshot {
        clips: clips(),
        overlays: overlays(),
        audios: audios(),
        titles: titles(),
        markers: markers(),
    };
    let mut restore = move |s: Snapshot| {
        clips.set(s.clips);
        overlays.set(s.overlays);
        audios.set(s.audios);
        titles.set(s.titles);
        markers.set(s.markers);
        // Indices just moved under us; a stale selection would edit the wrong item.
        selected.set(None);
        marked.write().clear();
        seek_to(playhead().min(extents(&clips.read()).iter().sum()));
    };
    let mut push_undo = move |tag: &str| {
        if !tag.is_empty() && undo_tag() == tag {
            return;
        }
        undo_tag.set(tag.to_string());
        let snap = snapshot();
        let mut u = undo_stack.write();
        u.push(snap);
        // ponytail: 64 steps, dropped oldest-first — deep enough for a session,
        // bounded so a long edit can't grow the stack without limit.
        if u.len() > 64 {
            u.remove(0);
        }
        drop(u);
        redo_stack.write().clear();
    };
    let mut undo = move |_: ()| {
        let prev = undo_stack.write().pop();
        let Some(prev) = prev else {
            status.set("Nothing to undo.".to_string());
            return;
        };
        redo_stack.write().push(snapshot());
        restore(prev);
        undo_tag.set(String::new());
        status.set("Undo.".to_string());
    };
    let mut redo = move |_: ()| {
        let next = redo_stack.write().pop();
        let Some(next) = next else {
            status.set("Nothing to redo.".to_string());
            return;
        };
        // Straight onto the undo stack, not via push_undo — that would clear
        // the redo stack we are in the middle of walking.
        undo_stack.write().push(snapshot());
        restore(next);
        undo_tag.set(String::new());
        status.set("Redo.".to_string());
    };

    let mut toggle_mark = move |s: Sel| {
        let mut m = marked.write();
        if let Some(p) = m.iter().position(|x| *x == s) {
            m.remove(p);
        } else {
            m.push(s);
        }
    };
    let group_of = move |s: Sel| -> usize {
        match s {
            Sel::Main(i) => clips.read().get(i).map_or(0, |c| c.group),
            Sel::Over(j) => overlays.read().get(j).map_or(0, |o| o.group),
            Sel::Aud(k) => audios.read().get(k).map_or(0, |a| a.group),
            Sel::Title(k) => titles.read().get(k).map_or(0, |t| t.group),
        }
    };
    let mut group_marked = move |_: ()| {
        let m = marked();
        push_undo("");
        if m.len() < 2 {
            status.set("Ctrl+click two or more timeline items, then Ctrl+G to group them.".to_string());
            return;
        }
        // Groups touched by any mark merge into the new one.
        let gids: Vec<usize> = m.iter().map(|&s| group_of(s)).filter(|g| *g != 0).collect();
        let gid = next_group();
        next_group.set(gid + 1);
        let joins = |g: usize| g != 0 && gids.contains(&g);
        for (i, c) in clips.write().iter_mut().enumerate() {
            if m.contains(&Sel::Main(i)) || joins(c.group) {
                c.group = gid;
            }
        }
        for (j, o) in overlays.write().iter_mut().enumerate() {
            if m.contains(&Sel::Over(j)) || joins(o.group) {
                o.group = gid;
            }
        }
        for (k, a) in audios.write().iter_mut().enumerate() {
            if m.contains(&Sel::Aud(k)) || joins(a.group) {
                a.group = gid;
            }
        }
        for (k, t) in titles.write().iter_mut().enumerate() {
            if m.contains(&Sel::Title(k)) || joins(t.group) {
                t.group = gid;
            }
        }
        marked.write().clear();
        status.set("Grouped — the items now move together; Ctrl+Shift+G ungroups.".to_string());
    };
    let mut ungroup_sel = move |_: ()| {
        push_undo("");
        let gid = selected()
            .map(group_of)
            .filter(|g| *g != 0)
            .or_else(|| marked().iter().map(|&s| group_of(s)).find(|g| *g != 0));
        let Some(gid) = gid else {
            status.set("Select a grouped item to ungroup.".to_string());
            return;
        };
        for c in clips.write().iter_mut().filter(|c| c.group == gid) {
            c.group = 0;
        }
        for o in overlays.write().iter_mut().filter(|o| o.group == gid) {
            o.group = 0;
        }
        for a in audios.write().iter_mut().filter(|a| a.group == gid) {
            a.group = 0;
        }
        for t in titles.write().iter_mut().filter(|t| t.group == gid) {
            t.group = 0;
        }
        marked.write().clear();
        status.set("Ungrouped.".to_string());
    };

    // The Group/Ungroup pair every lane's context menu carries — only the
    // "already ungrouped" test differs, so the caller passes it in.
    let group_rows = move |ungroup_disabled: bool| {
        rsx! {
            CtxItem {
                label: "Group marked items".to_string(),
                shortcut: Some("Ctrl+G".to_string()),
                disabled: marked().len() < 2,
                on_action: move |_| group_marked(()),
            }
            CtxItem {
                label: "Ungroup".to_string(),
                shortcut: Some("Ctrl+Shift+G".to_string()),
                disabled: ungroup_disabled,
                on_action: move |_| ungroup_sel(()),
            }
        }
    };

    // Drag a lane item (V2/A1/T) by dt seconds; grouped members ride along,
    // clamped so no member crosses 0.
    // Where a lane item currently sits — used both to move it and to snap it.
    let lane_at = move |target: Sel| -> Option<f64> {
        match target {
            Sel::Over(j) => overlays.read().get(j).map(|o| o.at),
            Sel::Aud(k) => audios.read().get(k).map(|a| a.at),
            Sel::Title(k) => titles.read().get(k).map(|t| t.at),
            Sel::Main(_) => None,
        }
    };
    let mut shift_lane = move |target: Sel, dt: f64| {
        push_undo("drag-lane");
        let gid = group_of(target);
        if gid == 0 {
            let Some(at) = lane_at(target) else { return };
            let at = (at + dt).max(0.0);
            match target {
                Sel::Over(j) => overlays.write()[j].at = at,
                Sel::Aud(k) => audios.write()[k].at = at,
                Sel::Title(k) => titles.write()[k].at = at,
                Sel::Main(_) => {}
            }
            return;
        }
        let min_at = overlays.read().iter().filter(|o| o.group == gid).map(|o| o.at)
            .chain(audios.read().iter().filter(|a| a.group == gid).map(|a| a.at))
            .chain(titles.read().iter().filter(|t| t.group == gid).map(|t| t.at))
            .fold(f64::MAX, f64::min);
        let dt = dt.max(-min_at);
        for o in overlays.write().iter_mut().filter(|o| o.group == gid) {
            o.at += dt;
        }
        for a in audios.write().iter_mut().filter(|a| a.group == gid) {
            a.at += dt;
        }
        for t in titles.write().iter_mut().filter(|t| t.group == gid) {
            t.at += dt;
        }
    };

    // Contiguous run of V1 clips sharing i's group (a lone clip is a run of one).
    let block_of = move |i: usize| -> (usize, usize) {
        let cl = clips.read();
        let gid = cl[i].group;
        if gid == 0 {
            return (i, 1);
        }
        let mut lo = i;
        while lo > 0 && cl[lo - 1].group == gid {
            lo -= 1;
        }
        let mut hi = i;
        while hi + 1 < cl.len() && cl[hi + 1].group == gid {
            hi += 1;
        }
        (lo, hi - lo + 1)
    };

    // Move clips[lo..lo+len] so the block lands at `dest` (index into the
    // sequence with the block removed). Attached items ride via the magnet.
    let mut move_block = move |lo: usize, len: usize, dest: usize| {
        push_undo("drag-clip");
        let old = spans();
        {
            let mut cl = clips.write();
            let block: Vec<Clip> = cl.drain(lo..lo + len).collect();
            for (n, c) in block.into_iter().enumerate() {
                cl.insert(dest + n, c);
            }
        }
        ride(old, &|k| Some(start_of(block_map(k, lo, len, dest))));
    };

    // Split at playhead: a selected V2/A1 item splits if the playhead is inside
    // its span; otherwise the V1 clip under the playhead splits. Selection lands
    // on the right half — where the playhead is — matching seek behavior.
    let mut split_at_playhead = move |_: ()| {
        const MIN: f64 = 0.1;
        push_undo("");
        marked.write().clear(); // marks are positional; a split shifts indices
        let t = playhead();
        let mut too_close = move || status.set("Playhead is too close to an edge to split.".to_string());

        // V2 and A1 split identically — the left half keeps the head, the right
        // half starts at the cut and anchors to the playhead.
        macro_rules! split_lane {
            ($lane:ident, $idx:expr, $sel:path, $noun:literal, $rate:expr) => {{
                let i = $idx;
                let Some(item) = $lane.read().get(i).cloned() else { return };
                // A retimed cutaway covers `rate` seconds of source per second
                // of timeline, so the cut lands further into the source.
                let rate: f64 = $rate(&item);
                match cut_local(item.in_s, item.out_s, item.in_s + (t - item.at) * rate, MIN) {
                    Some(local) => {
                        {
                            let mut lane = $lane.write();
                            let mut right = lane[i].clone();
                            lane[i].out_s = local;
                            right.in_s = local;
                            right.at = t;
                            lane.insert(i + 1, right);
                        }
                        selected.set(Some($sel(i + 1)));
                        status.set(format!(
                            concat!("Split ", $noun, " {} at {}."),
                            item.name,
                            fmt_t(t)
                        ));
                    }
                    None => too_close(),
                }
                return;
            }};
        }
        if let Some(Sel::Over(j)) = selected() {
            split_lane!(overlays, j, Sel::Over, "overlay", |o: &OverlayItem| o.speed.max(0.01));
        }
        if let Some(Sel::Aud(k)) = selected() {
            split_lane!(audios, k, Sel::Aud, "audio", |_: &AudioItem| 1.0);
        }

        let loc = locate(&clips.read(), t);
        let Some((i, local)) = loc else { return };
        let (name, in_s, out_s) = {
            let c = &clips.read()[i];
            (c.name.clone(), c.in_s, c.out_s)
        };
        let Some(local) = cut_local(in_s, out_s, local, MIN) else {
            too_close();
            return;
        };
        let (scrub, path, fr) = {
            let mut cl = clips.write();
            let mut right = cl[i].clone();
            cl[i].out_s = local;
            right.in_s = local;
            cl.insert(i + 1, right);
            (cl[i + 1].scrub_path(), cl[i + 1].path.clone(), cl[i + 1].framing.clone())
        };
        selected.set(Some(Sel::Main(i + 1)));
        status.set(format!("Split {} at {}.", name, fmt_t(t)));
        // The right half's thumbnail still shows the left's frame — retake it
        // at the new in point so the two halves are tellable apart.
        spawn(async move {
            if let Ok(thumb) = engine::frame_data_uri(&scrub, local, 108, 192, &fr, "", engine::Over::default()).await {
                let mut cl = clips.write();
                if let Some(c) = cl.get_mut(i + 1) {
                    if c.path == path && (c.in_s - local).abs() < 1e-6 {
                        c.thumb = thumb;
                    }
                }
            }
        });
    };

    // I/O: trim the V1 clip under the playhead to the playhead.
    let mut set_in_here = move |_: ()| {
        push_undo("");
        let loc = locate(&clips.read(), playhead());
        if let Some((i, local)) = loc {
            let old = spans();
            {
                let mut cl = clips.write();
                cl[i].in_s = local.min(cl[i].out_s - 0.1).max(0.0);
            }
            ride(old, &|k| Some(start_of(k)));
        }
    };
    let mut set_out_here = move |_: ()| {
        push_undo("");
        let loc = locate(&clips.read(), playhead());
        if let Some((i, local)) = loc {
            let old = spans();
            {
                let mut cl = clips.write();
                cl[i].out_s = local.max(cl[i].in_s + 0.1).min(cl[i].duration);
            }
            ride(old, &|k| Some(start_of(k)));
        }
    };

    // One file onto V1 at `insert_at` (None = append). Shared by the Add clips
    // dialog and by drops on the timeline, so both land identically. Returns
    // the error text rather than setting status, so a batch can summarise.
    let import_one_clip = move |path: String, insert_at: Option<usize>| async move {
        let name = file_name_of(&path);
        let (duration, has_audio) = engine::probe(&path).await.map_err(|e| format!("{name}: {e}"))?;
        let thumb =
            engine::frame_data_uri(&path, (duration * 0.1).min(1.0), 108, 192, "", "", engine::Over::default())
                .await
                .unwrap_or_default();
        let clip = Clip {
            path: path.clone(),
            name,
            duration,
            in_s: 0.0,
            out_s: initial_out(&path, duration),
            has_audio,
            effect: "None".to_string(),
            effect_amount: 1.0,
            framing: "Crop".to_string(),
            transform: engine::Transform::default(),
            speed: 1.0,
            volume: 1.0,
            transition: "None".to_string(),
            trans_dur: 0.5,
            thumb,
            wave: String::new(),
            proxy: String::new(),
            group: 0,
        };
        {
            let mut cl = clips.write();
            let i = insert_at.unwrap_or(cl.len()).min(cl.len());
            cl.insert(i, clip);
        }
        queue_proxy(path.clone());
        // A clip's own audio gets the same waveform strip A1 items have, so you
        // can see where the sound is without scrubbing for it. Rendered once per
        // source in the background; splits inherit it by clone.
        if has_audio {
            spawn(async move {
                if let Ok(uri) = engine::waveform_data_uri(&path).await {
                    for c in clips.write().iter_mut().filter(|c| c.path == path) {
                        c.wave = uri.clone();
                    }
                }
            });
        }
        if selected().is_none() {
            select_clip(0);
        }
        Ok::<(), String>(())
    };

    // A batch onto V1, reporting how many made it. Drops and the dialog both
    // arrive here with a list of paths.
    let import_clip_paths = move |paths: Vec<String>, insert_at: Option<usize>| {
        if paths.is_empty() || importing() {
            return;
        }
        spawn(async move {
            importing.set(true);
            push_undo(""); // one undo step for the whole batch
            let (mut ok, mut failed) = (0usize, Vec::<String>::new());
            for (n, path) in paths.into_iter().enumerate() {
                status.set(format!("Importing {}…", file_name_of(&path)));
                // Later files in a batch go after the earlier ones.
                match import_one_clip(path, insert_at.map(|i| i + n)).await {
                    Ok(()) => ok += 1,
                    Err(e) => failed.push(e),
                }
            }
            importing.set(false);
            status.set(if failed.is_empty() {
                format!("{ok} added — {} clip(s) on the timeline.", clips.read().len())
            } else {
                format!("{ok} added, {} skipped: {}", failed.len(), failed.join("; "))
            });
        });
    };

    let import_clips = move |_: ()| {
        if importing() {
            return;
        }
        spawn(async move {
            let Some(files) = rfd::AsyncFileDialog::new()
                .add_filter("Video & photos", &media_ext())
                .add_filter("All files", &["*"])
                .set_title("Add clips")
                .pick_files()
                .await
            else {
                return;
            };
            import_clip_paths(files.iter().map(|f| f.path().display().to_string()).collect(), None);
        });
    };

    // A cutaway on V2 starting at `at`. Shared by the dialog and by drops.
    let add_overlay_path = move |path: String, at: f64| {
        spawn(async move {
            let name = file_name_of(&path);
            match engine::probe(&path).await {
                Ok((duration, _)) => {
                    push_undo("");
                    overlays.write().push(OverlayItem {
                        path: path.clone(),
                        name,
                        duration,
                        in_s: 0.0,
                        out_s: initial_out(&path, duration),
                        at: at.max(0.0),
                        effect: "None".to_string(),
                        effect_amount: 1.0,
                        framing: "Crop".to_string(),
                        transform: engine::Transform::default(),
                        speed: 1.0,
                        proxy: String::new(),
                        group: 0,
                    });
                    queue_proxy(path);
                    selected.set(Some(Sel::Over(overlays.read().len() - 1)));
                    status.set(format!("Overlay at {} — V2 covers V1 while it runs.", fmt_t(at)));
                }
                Err(e) => status.set(format!("Could not add overlay: {e}")),
            }
        });
    };

    let add_overlay = move |_: ()| {
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Video & photos", &media_ext())
                .add_filter("All files", &["*"])
                .set_title("Add overlay (V2)")
                .pick_file()
                .await
            else {
                return;
            };
            add_overlay_path(f.path().display().to_string(), playhead());
        });
    };

    // Sound under the main track from `at`. A video dropped here contributes its
    // soundtrack, which is why the dialog offers video too.
    let add_audio_path = move |path: String, at: f64| {
        spawn(async move {
            let name = file_name_of(&path);
            match engine::probe(&path).await {
                Ok((duration, has_audio)) => {
                    if !has_audio {
                        status.set(format!("{name} has no audio stream."));
                        return;
                    }
                    push_undo("");
                    audios.write().push(AudioItem {
                        path: path.clone(),
                        name,
                        duration,
                        in_s: 0.0,
                        out_s: duration,
                        at: at.max(0.0),
                        volume: 1.0,
                        duck: 0.0,
                        wave: String::new(),
                        group: 0,
                    });
                    selected.set(Some(Sel::Aud(audios.read().len() - 1)));
                    status.set(format!("Audio at {} — mixed under the main track.", fmt_t(at)));
                    // Waveform renders in the background; splits share it by path.
                    spawn(async move {
                        if let Ok(uri) = engine::waveform_data_uri(&path).await {
                            for a in audios.write().iter_mut().filter(|a| a.path == path) {
                                a.wave = uri.clone();
                            }
                        }
                    });
                }
                Err(e) => status.set(format!("Could not add audio: {e}")),
            }
        });
    };

    let add_audio = move |_: ()| {
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Audio", engine::AUDIO_EXT)
                .add_filter("Video (use its soundtrack)", engine::VIDEO_EXT)
                .add_filter("All files", &["*"])
                .set_title("Add audio (A1)")
                .pick_file()
                .await
            else {
                return;
            };
            add_audio_path(f.path().display().to_string(), playhead());
        });
    };

    let mut add_title = move |_: ()| {
        if clips.read().is_empty() {
            return;
        }
        push_undo("");
        titles.write().push(TitleItem {
            text: "Title".to_string(),
            at: playhead(),
            dur: 3.0,
            font_size: 110.0,
            color: "White".to_string(),
            pos: "Middle".to_string(),
            bevel: "Cameo".to_string(),
            bevel_size: 21.0,
            font: "Sans".to_string(),
            align: "Centre".to_string(),
            anim: "None".to_string(),
            reveal: false,
            kind: "Text".to_string(),
            shape_w: 0.6,
            shape_h: 0.12,
            shape_x: 0.0,
            // Transparent by default: the video shows through, and the relief
            // plus an outline carry legibility without an opaque plate.
            boxed: false,
            outline: 4.0,
            outline_color: "Black".to_string(),
            soften: 4.0,
            depth: 100.0,
            angle: 120.0,
            altitude: 30.0,
            hi_opacity: 0.75,
            sh_opacity: 0.75,
            caption: false,
            pngs: Vec::new(),
            group: 0,
        });
        let k = titles.read().len() - 1;
        selected.set(Some(Sel::Title(k)));
        rerender_title(k);
        status.set("Title added at the playhead — edit it in the inspector.".to_string());
    };

    let gather_specs = move || -> (Vec<ClipSpec>, Vec<OverlaySpec>, Vec<TitleSpec>, Vec<AudioSpec>) {
        let specs = clips.read().iter().map(Clip::spec).collect();
        let ospecs = overlays
            .read()
            .iter()
            .map(|o| OverlaySpec {
                path: o.path.clone(),
                in_s: o.in_s,
                out_s: o.out_s,
                at: o.at,
                speed: o.speed,
                effect: o.look(),
                framing: o.framing.clone(),
            })
            .collect();
        let tspecs = titles
            .read()
            .iter()
            .filter(|t| !t.pngs.is_empty())
            .flat_map(|t| {
                let segs = t.segments();
                let n = segs.len().min(t.pngs.len());
                // Only the first card slides on and only the last fades out, so
                // a revealed line reads as one title rather than n flashes.
                (0..n)
                    .map(|k| TitleSpec {
                        png: t.pngs[k].clone(),
                        at: segs[k].1,
                        dur: segs[k].2,
                        anim: if k == 0 { t.anim.clone() } else { "None".to_string() },
                        fade_in: k == 0,
                        fade_out: k + 1 == n,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let aspecs = audios
            .read()
            .iter()
            .map(|a| AudioSpec {
                path: a.path.clone(),
                in_s: a.in_s,
                out_s: a.out_s,
                at: a.at,
                volume: a.volume,
                duck: a.duck,
            })
            .collect();
        (specs, ospecs, tspecs, aspecs)
    };

    // In-app playback: a timer walks the playhead in real time and reuses the
    // scrub pipeline (proxies + latest-wins queue), so frames that can't keep
    // up are dropped instead of queued. Audio: the V1+A1 mix renders to a wav
    // (fast — no video encode) and mpv/ffplay plays it alongside; both sides
    // follow wall clock, so they stay in step.
    // ponytail: seeking while playing desyncs audio until the next play.
    let mut playing = use_signal(|| false);
    let mut toggle_play = move |_: ()| {
        if playing() {
            playing.set(false);
            return;
        }
        if clips.read().is_empty() {
            return;
        }
        if playhead() >= total_of() - 0.05 {
            seek_to(0.0); // replay from the top
        }
        playing.set(true);
        spawn(async move {
            let wav = std::env::temp_dir().join("morreel-playmix.wav");
            let (specs, _, _, aspecs) = gather_specs();
            let mut audio = match engine::render_audio_mix(&specs, &aspecs, &wav).await {
                // Guard: paused while the mix was rendering — don't start sound.
                Ok(()) if playing() => match engine::launch_audio(&wav, playhead()) {
                    Ok(child) => Some(child),
                    Err(e) => {
                        status.set(format!("Playing without audio ({e})"));
                        None
                    }
                },
                Ok(()) => None,
                Err(e) => {
                    status.set(format!("Playing without audio ({e})"));
                    None
                }
            };
            let mut last = std::time::Instant::now();
            while playing() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let dt = last.elapsed().as_secs_f64().min(0.5);
                last = std::time::Instant::now();
                let t = playhead() + dt;
                if t >= total_of() {
                    seek_to(total_of());
                    playing.set(false);
                    break;
                }
                seek_to(t);
            }
            if let Some(child) = audio.as_mut() {
                let _ = child.start_kill();
            }
        });
    };

    // Render any title still missing its PNG (e.g. render was interrupted).
    let ensure_titles = move || async move {
        let missing: Vec<(usize, TitleItem)> = titles
            .read()
            .iter()
            .enumerate()
            .filter(|(_, t)| t.pngs.is_empty())
            .map(|(k, t)| (k, t.clone()))
            .collect();
        for (k, t) in missing {
            if let Ok(png) = render_one(&t).await {
                if let Some(item) = titles.write().get_mut(k) {
                    item.pngs = png;
                }
            }
        }
    };

    // Thumbnails, proxies, waveforms and title PNGs are all derived from the
    // sources, so they stay out of the project file and get rebuilt here after
    // a load. Everything is content-addressed or cheap, so this is mostly cache
    // hits on a project you saved a minute ago.
    let mut rehydrate = move || {
        let media: Vec<String> = clips
            .read()
            .iter()
            .map(|c| c.path.clone())
            .chain(overlays.read().iter().map(|o| o.path.clone()))
            .collect();
        for p in media {
            queue_proxy(p);
        }
        for path in audios.read().iter().map(|a| a.path.clone()).collect::<Vec<_>>() {
            spawn(async move {
                if let Ok(uri) = engine::waveform_data_uri(&path).await {
                    for a in audios.write().iter_mut().filter(|a| a.path == path) {
                        a.wave = uri.clone();
                    }
                }
            });
        }
        // V1 clips carry a waveform of their own audio; silent ones never get one.
        let voiced: Vec<String> = {
            let cl = clips.read();
            let mut v: Vec<String> =
                cl.iter().filter(|c| c.has_audio).map(|c| c.path.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        for path in voiced {
            spawn(async move {
                if let Ok(uri) = engine::waveform_data_uri(&path).await {
                    for c in clips.write().iter_mut().filter(|c| c.path == path) {
                        c.wave = uri.clone();
                    }
                }
            });
        }
        let posters: Vec<(usize, String, f64, String)> = clips
            .read()
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.path.clone(), c.in_s, c.framing.clone()))
            .collect();
        spawn(async move {
            for (i, path, t, fr) in posters {
                if let Ok(thumb) = engine::frame_data_uri(&path, t, 108, 192, &fr, "", engine::Over::default()).await {
                    if let Some(c) = clips.write().get_mut(i) {
                        if c.path == path {
                            c.thumb = thumb;
                        }
                    }
                }
            }
        });
        spawn(async move {
            ensure_titles().await;
            seek_to(playhead());
        });
    };

    const PROJECT_EXT: [&str; 2] = ["morreel", "json"];

    let save_project = move |_: ()| {
        spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("MorReel project", &PROJECT_EXT)
                .set_file_name("reel.morreel")
                .set_title("Save project")
                .save_file()
                .await
            else {
                return;
            };
            // The project records the edit, not the media: sources stay
            // referenced by path, so a project file is small and text.
            let res = serde_json::to_string_pretty(&snapshot())
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(file.path(), json).map_err(|e| e.to_string()));
            match res {
                Ok(()) => status.set(format!("Saved {}", file.path().display())),
                Err(e) => status.set(format!("Save failed: {e}")),
            }
        });
    };

    let open_project = move |_: ()| {
        spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("MorReel project", &PROJECT_EXT)
                .set_title("Open project")
                .pick_file()
                .await
            else {
                return;
            };
            let parsed = std::fs::read_to_string(file.path())
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Snapshot>(&t).map_err(|e| e.to_string()));
            let snap = match parsed {
                Ok(s) => s,
                Err(e) => {
                    status.set(format!("Could not open {}: {e}", file.file_name()));
                    return;
                }
            };
            // Sources are referenced by path, so a moved file loads as a clip
            // that will fail at export — say so now rather than then.
            let missing = snap
                .clips
                .iter()
                .map(|c| &c.path)
                .chain(snap.overlays.iter().map(|o| &o.path))
                .chain(snap.audios.iter().map(|a| &a.path))
                .filter(|p| !std::path::Path::new(p).exists())
                .count();
            push_undo(""); // opening is undoable like any other edit
            restore(snap);
            rehydrate();
            status.set(if missing > 0 {
                format!("Opened {} — {missing} source file(s) are missing.", file.file_name())
            } else {
                format!("Opened {}", file.file_name())
            });
        });
    };

    // Auto captions, TikTok-style: transcribe the timeline's audio mix and
    // drop each segment onto the T lane as a normal title item — fix wording,
    // restyle or retime any caption in the inspector like any other title.
    let mut transcribing = use_signal(|| false);
    let mut auto_captions = move |_: ()| {
        if clips.read().is_empty() || transcribing() || export_progress().is_some() {
            return;
        }
        transcribing.set(true);
        spawn(async move {
            status.set("Transcribing audio for captions… (first run downloads the model)".to_string());
            let wav = std::env::temp_dir().join("morreel-captions.wav");
            // Reuses the export progress bar; also parks export/preview while busy.
            export_progress.set(Some(0.0));
            let res = {
                let (specs, _, _, _) = gather_specs();
                let total: f64 = specs.iter().map(ClipSpec::trimmed).sum();
                async move {
                    // V1 audio only: A1 music mixed under would pollute the
                    // transcript. Voiceovers belong on V1 to be captioned.
                    engine::render_audio_mix(&specs, &[], &wav).await?;
                    engine::transcribe(&wav, total, |p| export_progress.set(Some(p))).await
                }
            }
            .await;
            export_progress.set(None);
            match res {
                Ok(segs) if segs.is_empty() => status.set("No speech found to caption.".to_string()),
                Ok(segs) => {
                    push_undo("");
                    let n = segs.len();
                    {
                        let mut ts = titles.write();
                        for (k, (start, end, text)) in segs.iter().enumerate() {
                            // Whisper segments can overlap; end each caption a
                            // hair before the next starts so they never stack.
                            let mut dur = (end - start).max(0.5);
                            if let Some((next, _, _)) = segs.get(k + 1) {
                                dur = dur.min((next - start - 0.05).max(0.3));
                            }
                            ts.push(TitleItem {
                                text: wrap_caption(text, 26),
                                at: *start,
                                dur,
                                font_size: 64.0,
                                color: "White".to_string(),
                                pos: "Lower third".to_string(),
                                bevel: "Off".to_string(),
                                bevel_size: 21.0,
                                font: "Sans".to_string(),
                                align: "Centre".to_string(),
                                anim: "None".to_string(),
                                reveal: false,
                                kind: "Text".to_string(),
                                shape_w: 0.6,
                                shape_h: 0.12,
                                shape_x: 0.0,
                                boxed: true, // backdrop keeps captions readable over busy video
                                outline: 0.0,
                                outline_color: "Black".to_string(),
                                soften: 4.0,
                                depth: 100.0,
                                angle: 120.0,
                                altitude: 30.0,
                                hi_opacity: 0.75,
                                sh_opacity: 0.75,
                                caption: true,
                                pngs: Vec::new(),
                                group: 0,
                            });
                        }
                    }
                    ensure_titles().await;
                    seek_to(playhead());
                    status.set(format!(
                        "{n} caption(s) on the T lane — check the wording in the inspector before export."
                    ));
                }
                Err(e) => status.set(format!("Captions failed: {e}")),
            }
            transcribing.set(false);
        });
    };

    // Bulk-clear a bad transcription; manual titles stay.
    let mut clear_captions = move |_: ()| {
        let n = titles.read().iter().filter(|t| t.caption).count();
        if n == 0 {
            return;
        }
        push_undo("");
        marked.write().clear();
        if matches!(selected(), Some(Sel::Title(_))) {
            selected.set(None); // title indices shift under the retain
        }
        titles.write().retain(|t| !t.caption);
        seek_to(playhead());
        status.set(format!("Removed {n} caption(s) — manual titles kept."));
    };

    // Export settings live in a dialog rather than being hardcoded: the format
    // decides whether the reel even carries audio, so it has to be chosen
    // before the save dialog names the file.
    let mut show_export = use_signal(|| false);
    let mut export_opts = use_signal(engine::ExportOpts::default);

    let mut run_export = move |_: ()| {
        if clips.read().is_empty() || export_progress().is_some() {
            return;
        }
        playing.set(false);
        show_export.set(false);
        spawn(async move {
            let opts = export_opts();
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter(opts.format.label(), &[opts.format.ext()])
                .set_file_name(format!("morreel.{}", opts.format.ext()))
                .set_title("Export portrait video")
                .save_file()
                .await
            else {
                return;
            };
            ensure_titles().await;
            let (specs, ospecs, tspecs, aspecs) = gather_specs();
            export_progress.set(Some(0.0));
            status.set(format!("Exporting {} at {}…", opts.format.label(), engine::size_label(opts.width)));
            let res = engine::export(&specs, &ospecs, &tspecs, &aspecs, file.path(), opts, |p| {
                export_progress.set(Some(p))
            })
            .await;
            export_progress.set(None);
            match res {
                Ok(()) => status.set(format!("Exported {}", file.path().display())),
                Err(e) => status.set(format!("Export failed: {e}")),
            }
        });
    };

    let mut do_export = move |_: ()| {
        if clips.read().is_empty() || export_progress().is_some() {
            return;
        }
        show_export.set(true);
    };

    // smplayer-style full-fidelity preview: fast-render the timeline to a temp
    // file and hand it to a real player (mpv, else ffplay) — audio included.
    let mut play_preview = move |_: ()| {
        if clips.read().is_empty() || export_progress().is_some() {
            return;
        }
        playing.set(false);
        spawn(async move {
            let out = std::env::temp_dir().join("morreel-preview.mp4");
            ensure_titles().await;
            let (specs, ospecs, tspecs, aspecs) = gather_specs();
            export_progress.set(Some(0.0));
            status.set("Rendering preview…".to_string());
            let res = engine::export(&specs, &ospecs, &tspecs, &aspecs, &out, engine::ExportOpts::preview(), |p| {
                export_progress.set(Some(p))
            })
            .await;
            export_progress.set(None);
            match res {
                Ok(()) => match engine::launch_player(&out) {
                    Ok(player) => status.set(format!("Playing preview in {player}.")),
                    Err(e) => status.set(format!("Preview rendered but {e}")),
                },
                Err(e) => status.set(format!("Preview render failed: {e}")),
            }
        });
    };

    let mut move_sel = move |delta: i64| {
        if let Some(Sel::Main(i)) = selected() {
            let j = i as i64 + delta;
            if j >= 0 && (j as usize) < clips.read().len() {
                let j = j as usize;
                push_undo("");
                let old = spans();
                clips.write().swap(i, j);
                selected.set(Some(Sel::Main(j)));
                ride(old, &|k| Some(start_of(if k == i { j } else if k == j { i } else { k })));
            }
        }
    };

    let mut delete_sel = move |_: ()| {
        push_undo("");
        marked.write().clear(); // marks are positional; a delete shifts indices
        match selected() {
            Some(Sel::Main(i)) => {
                let old = spans();
                clips.write().remove(i); // ripple by construction — the gap closes
                ride(old, &|k| (k != i).then(|| start_of(if k > i { k - 1 } else { k })));
                let len = clips.read().len();
                if len == 0 {
                    selected.set(None);
                    preview.set(String::new());
                } else {
                    select_clip(i.min(len - 1));
                }
            }
            Some(Sel::Over(j)) => {
                overlays.write().remove(j);
                selected.set(None);
            }
            Some(Sel::Aud(k)) => {
                audios.write().remove(k);
                selected.set(None);
            }
            Some(Sel::Title(k)) => {
                titles.write().remove(k);
                selected.set(None);
            }
            None => {}
        }
    };

    let mut step_sel = move |d: i64| {
        let len = clips.read().len();
        if len == 0 {
            return;
        }
        let cur = match selected() {
            Some(Sel::Main(i)) => i as i64,
            _ => -1,
        };
        select_clip((cur + d).clamp(0, len as i64 - 1) as usize);
    };

    let mut nudge = move |d: f64| {
        seek_to((playhead() + d).clamp(0.0, total_of()));
    };

    // On-screen transform handles over the monitor. The sliders in the
    // inspector stay the precise way in; this is the direct way.
    let mut show_handles = use_signal(|| true);
    // The monitor's box on screen, measured when a drag starts rather than
    // tracked, so a resized window can never leave stale geometry behind.
    let mut xf_drag =
        use_signal(|| Option::<(XfGrab, (f64, f64), engine::Transform, (f64, f64, f64, f64))>::None);
    let mut phone_el = use_signal(|| Option::<std::rc::Rc<MountedData>>::None);

    // The transform of whatever is selected, if it has one.
    let selected_xf = move || -> Option<engine::Transform> {
        match selected() {
            Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.transform),
            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.transform),
            _ => None,
        }
    };
    // Write it back and refresh the monitor, whichever lane it came from.
    let mut set_selected_xf = move |t: engine::Transform| {
        let target = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                cl[i].transform = t;
                Some((cl[i].scrub_path(), cl[i].in_s, cl[i].framing.clone(), cl[i].look()))
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                ov[j].transform = t;
                Some((ov[j].scrub_path(), ov[j].in_s, ov[j].framing.clone(), ov[j].look()))
            }
            _ => None,
        };
        if let Some((path, at, fr, look)) = target {
            request_preview(path, at, fr, look, engine::Over::default());
        }
    };

    // Grabbing a handle measures the monitor first, so the very first pointer
    // move already has real geometry to work against.
    let mut begin_xf = move |grab: XfGrab, from: (f64, f64)| {
        let Some(start) = selected_xf() else { return };
        let Some(el) = phone_el() else { return };
        push_undo("xf-handle"); // one undo step for the whole drag
        spawn(async move {
            if let Ok(r) = el.get_client_rect().await {
                xf_drag.set(Some((
                    grab,
                    from,
                    start,
                    (r.origin.x, r.origin.y, r.size.width, r.size.height),
                )));
            }
        });
    };

    // The Transform panel. A V1 clip and a V2 cutaway differ only in whether
    // opacity means anything, and both write through the selection, so one
    // definition serves both lanes.
    let transform_panel = move |with_opacity: bool| {
        let Some(xf) = selected_xf() else {
            return rsx! {};
        };
        rsx! {
            h4 { class: "mr-fx-cat", "Transform" }
            if with_opacity {
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Scale below 1 makes this a picture-in-picture — V1 shows through around it."
                }
            }
            for (label, value, min, max, step, set) in transform_knobs(&xf, with_opacity) {
                Slider {
                    key: "{label}",
                    label: Some(label),
                    min, max, step,
                    precision: if step < 0.1 { 3 } else { 0 },
                    value,
                    oninput: Some(EventHandler::new(move |v: f64| {
                        push_undo(&format!("xf-{label}"));
                        // Read the live value rather than the one this render
                        // captured, so a drag never writes back a stale sibling.
                        if let Some(mut t) = selected_xf() {
                            set(&mut t, v);
                            set_selected_xf(t);
                        }
                    })),
                }
            }
            if !xf.is_identity() {
                button {
                    class: "mor-btn mr-reset",
                    onclick: move |_| {
                        push_undo("");
                        set_selected_xf(engine::Transform::default());
                    },
                    "↺ Reset transform"
                }
            }
        }
    };

    // Title style presets, shared across projects: a creator making a series
    // wants reel 47 to look like reel 46.
    let mut presets = use_signal(load_presets);
    let mut show_save_preset = use_signal(|| false);
    let mut preset_name = use_signal(String::new);
    let mut store_preset = move |k: usize| {
        let Some(style) = titles.read().get(k).cloned() else { return };
        let name = preset_name().trim().to_string();
        if name.is_empty() {
            return;
        }
        {
            let mut all = presets.write();
            // Saving under an existing name replaces it, which is what anyone
            // expects from a name they typed on purpose.
            all.retain(|p| p.name != name);
            all.push(TitlePreset { name: name.clone(), style });
            all.sort_by_key(|p| p.name.to_lowercase());
        }
        match save_presets(&presets.read()) {
            Ok(()) => status.set(format!("Saved the style \"{name}\".")),
            Err(e) => status.set(format!("Could not save the preset: {e}")),
        }
        show_save_preset.set(false);
        preset_name.set(String::new());
    };

    let mut drop_marker = move |_: ()| {
        let t = playhead();
        // Tapping the same beat twice is a slip, not a second marker.
        if markers.read().iter().any(|m| (m - t).abs() < 0.02) {
            return;
        }
        push_undo("");
        let mut m = markers.write();
        m.push(t);
        m.sort_by(f64::total_cmp);
        let n = m.len();
        drop(m);
        status.set(format!("Marker at {} ({n} total) — tap M on the beat while it plays.", fmt_t(t)));
    };
    let mut clear_markers = move |_: ()| {
        if markers.read().is_empty() {
            return;
        }
        push_undo("");
        markers.write().clear();
        status.set("Markers cleared.".to_string());
    };

    // Safe-area guides over the monitor — the portrait editor's ruler for
    // "will this caption survive the app's own buttons".
    let mut safe_area = use_signal(|| false);
    let mut toggle_safe = move |_: ()| {
        safe_area.toggle();
        status.set(if safe_area() {
            "Safe areas on — keep titles out of the shaded bands.".to_string()
        } else {
            "Safe areas off.".to_string()
        });
    };

    let mut toggle_handles = move |_: ()| {
        show_handles.toggle();
        status.set(if show_handles() {
            "Transform handles on — drag the picture, a corner to scale, the knob to rotate."
                .to_string()
        } else {
            "Transform handles off.".to_string()
        });
    };

    // Keyboard shortcuts. I/O/S/Delete/Ctrl+O/Ctrl+E are bound by their menu
    // items (the menu is the single source of truth); these have no menu row.
    use_shortcut(Some(" ".into()), Some(EventHandler::new(move |()| toggle_play(()))));
    use_shortcut(Some("BACKSPACE".into()), Some(EventHandler::new(move |()| delete_sel(()))));
    use_shortcut(Some("ARROWLEFT".into()), Some(EventHandler::new(move |()| nudge(-0.1))));
    use_shortcut(Some("ARROWRIGHT".into()), Some(EventHandler::new(move |()| nudge(0.1))));
    use_shortcut(Some("SHIFT+ARROWLEFT".into()), Some(EventHandler::new(move |()| nudge(-1.0))));
    use_shortcut(Some("SHIFT+ARROWRIGHT".into()), Some(EventHandler::new(move |()| nudge(1.0))));
    use_shortcut(Some("HOME".into()), Some(EventHandler::new(move |()| seek_to(0.0))));
    use_shortcut(Some("END".into()), Some(EventHandler::new(move |()| seek_to(total_of()))));
    use_shortcut(Some("[".into()), Some(EventHandler::new(move |()| step_sel(-1))));
    use_shortcut(Some("]".into()), Some(EventHandler::new(move |()| step_sel(1))));
    use_shortcut(Some("ESCAPE".into()), Some(EventHandler::new(move |()| ctx_menu.set(None))));
    use_shortcut(Some("G".into()), Some(EventHandler::new(move |()| toggle_safe(()))));
    use_shortcut(Some("T".into()), Some(EventHandler::new(move |()| toggle_handles(()))));
    use_shortcut(Some("M".into()), Some(EventHandler::new(move |()| drop_marker(()))));
    use_shortcut(Some("SHIFT+M".into()), Some(EventHandler::new(move |()| clear_markers(()))));
    // The menu item binds "~"; this covers layouts where ~ is Shift+` and the
    // combo therefore arrives as SHIFT+~.
    use_shortcut(Some("SHIFT+~".into()), Some(EventHandler::new(move |()| toggle_magnet(()))));

    // Window chrome preference (frameless / native / tiling), persisted like
    // the blogger theme editor; takes effect on next launch.
    let active_mode = UiMode::active();
    let mut preferred_mode = use_signal(|| UiMode::load_preference().unwrap_or(active_mode));
    let mut show_about = use_signal(|| false);
    let mut show_shortcuts = use_signal(|| false);
    let mut set_mode = move |m: UiMode| {
        preferred_mode.set(m);
        let _ = m.save_preference();
        status.set(format!("Window mode → {m} (applies on next launch)"));
    };
    let radio = move |m: UiMode| if preferred_mode() == m { "●" } else { "○" };

    // Pop-out program monitor: the monitor MOVES to its own OS window — the
    // embedded phone hides while it's out, and closing the window docks it back.
    let mut monitor_out = use_signal(|| false);
    let mut open_monitor = move || {
        if monitor_out() {
            return;
        }
        use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
        let dom = VirtualDom::new_with_props(
            Monitor,
            MonitorProps { preview, out: monitor_out, safe: safe_area },
        );
        let cfg = Config::new()
            .with_menu(None::<dioxus::desktop::muda::Menu>)
            .with_window(
                WindowBuilder::new()
                    .with_title("MorReel Monitor")
                    .with_inner_size(LogicalSize::new(414.0, 764.0)),
            );
        let _ = dioxus::desktop::window().new_window(dom, cfg);
        monitor_out.set(true);
        status.set("Monitor popped out — close its window to dock it back.".to_string());
    };

    // Timeline zoom (status-bar control) and middle-mouse panning state.
    let mut zoom = use_signal(|| 1.0f64);
    let mut pan = use_signal(|| Option::<(f64, f64)>::None);

    // Timeline scale (px per second), shared by the rsx and the drag handlers.
    // ponytail: keyed to the shortest clip (min 48px wide) — no per-clip
    // min-width, so ruler/playhead geometry stays exact.
    let calc_scale = move || {
        let min_dur = clips.read().iter().map(Clip::trimmed).fold(f64::MAX, f64::min);
        ((48.0 / min_dur).clamp(14.0, 240.0) * zoom()).clamp(2.0, 960.0)
    };

    // Files dragged in from the file manager. The lane under the cursor decides
    // what the file becomes; `route_drop` has the final say when the two
    // disagree. V1 collects its files into one batch so a multi-file drop is a
    // single undo step and lands in the order they were dropped.
    let mut drop_hover = use_signal(|| Option::<Lane>::None);
    let mut handle_drop = move |paths: Vec<String>, onto: Lane, at: f64| {
        drop_hover.set(None);
        if paths.is_empty() {
            return;
        }
        let (mut to_v1, mut notes, mut refused) = (Vec::new(), Vec::new(), Vec::new());
        for path in paths {
            match route_drop(kind_of(&path), onto) {
                Err(why) => refused.push(format!("{} ({why})", file_name_of(&path))),
                Ok((lane, note)) => {
                    if let Some(n) = note {
                        if !notes.contains(&n) {
                            notes.push(n);
                        }
                    }
                    match lane {
                        Lane::V1 => to_v1.push(path),
                        Lane::V2 => add_overlay_path(path, at),
                        Lane::A1 => add_audio_path(path, at),
                    }
                }
            }
        }
        if !to_v1.is_empty() {
            let index = insert_index(&clips.read(), at);
            import_clip_paths(to_v1, Some(index));
        }
        if !refused.is_empty() {
            status.set(format!("Skipped {}", refused.join(", ")));
        } else if !notes.is_empty() {
            status.set(format!("Dropped — {}.", notes.join(", ")));
        }
    };

    // Turn a drop event into (paths, timeline seconds under the cursor).
    let drop_payload = move |evt: &Event<DragData>| -> (Vec<String>, f64) {
        let paths = evt
            .files()
            .iter()
            .map(|f| f.path().display().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        let t = (evt.element_coordinates().x / calc_scale()).max(0.0);
        (paths, t)
    };

    // Left-drag state: (target, last pointer x, V1 block's floating start,
    // total px travelled). `drag_moved` swallows the click after a real drag.
    let mut drag = use_signal(|| Option::<(Sel, f64, f64, f64)>::None);
    let mut drag_moved = use_signal(|| false);
    // Ruler scrub: mousedown on the ruler seeks and keeps seeking while held.
    let mut scrubbing = use_signal(|| false);

    // Inspector tab: "edit" (item parameters) or "fx" (effects browser).
    let mut insp_tab = use_signal(|| "edit");

    // Effects browser thumbnails: the selected item's poster frame through
    // every effect, generated lazily and cached until the frame changes.
    let mut fx_thumbs = use_signal(std::collections::HashMap::<String, String>::new);
    let mut fx_key = use_signal(String::new);
    use_effect(move || {
        if insp_tab() != "fx" {
            return;
        }
        let target = match selected() {
            Some(Sel::Main(i)) => clips.read().get(i).map(|c| (c.scrub_path(), c.in_s, c.framing.clone())),
            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| (o.scrub_path(), o.in_s, o.framing.clone())),
            _ => None,
        };
        let Some((path, t, fr)) = target else { return };
        let key = format!("{path}@{t:.2}@{fr}");
        if fx_key() == key {
            return;
        }
        fx_key.set(key.clone());
        fx_thumbs.write().clear();
        spawn(async move {
            for &(_, name, filter) in EFFECTS {
                if fx_key() != key {
                    return; // selection moved on — this batch is stale
                }
                if let Ok(uri) = engine::frame_data_uri(&path, t, 108, 192, &fr, filter, engine::Over::default()).await {
                    if fx_key() == key {
                        fx_thumbs.write().insert(name.to_string(), uri);
                    }
                }
            }
        });
    });

    let mut apply_effect = move |name: String| {
        push_undo("");
        match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let (path, t, fr, look) = {
                    let mut cl = clips.write();
                    cl[i].effect = name.clone();
                    (cl[i].scrub_path(), cl[i].in_s, cl[i].framing.clone(), cl[i].look())
                };
                request_preview(path, t, fr, look, engine::Over::default());
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let (path, t, fr, look) = {
                    let mut ov = overlays.write();
                    ov[j].effect = name.clone();
                    (ov[j].scrub_path(), ov[j].in_s, ov[j].framing.clone(), ov[j].look())
                };
                request_preview(path, t, fr, look, engine::Over::default());
            }
            _ => status.set("Select a V1 clip or V2 overlay to apply an effect.".to_string()),
        }
    };

    // Live strength change for the selected video item's effect.
    let mut set_effect_amount = move |v: f64| {
        push_undo("fx-amount");
        match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let (path, t, fr, eff) = {
                    let mut cl = clips.write();
                    cl[i].effect_amount = v;
                    (cl[i].scrub_path(), cl[i].in_s, cl[i].framing.clone(), cl[i].look())
                };
                request_preview(path, t, fr, eff, engine::Over::default());
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let (path, t, fr, eff) = {
                    let mut ov = overlays.write();
                    ov[j].effect_amount = v;
                    (ov[j].scrub_path(), ov[j].in_s, ov[j].framing.clone(), ov[j].look())
                };
                request_preview(path, t, fr, eff, engine::Over::default());
            }
            _ => {}
        }
    };

    let total = total_of();
    let exporting = export_progress().is_some();
    let no_clips = clips.read().is_empty();
    let effect_names: Vec<String> = EFFECTS.iter().map(|(_, n, _)| n.to_string()).collect();

    rsx! {
        MorAppFrame {
            title: "MorReel Studio".to_string(),
            subtitle: Some("portrait 9:16".to_string()),
            app_name: "MorReel Studio".to_string(),
            menu: Some(rsx! {
                MorMenuDropdown { label: "File".to_string(),
                    MenuItem {
                        label: "Open project…".to_string(),
                        shortcut: Some("Ctrl+Shift+O".to_string()),
                        disabled: exporting,
                        on_action: move |_| open_project(()),
                    }
                    MenuItem {
                        label: "Save project…".to_string(),
                        shortcut: Some("Ctrl+S".to_string()),
                        disabled: no_clips,
                        on_action: move |_| save_project(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Add clips…".to_string(),
                        shortcut: Some("Ctrl+O".to_string()),
                        disabled: importing() || exporting,
                        on_action: move |_| import_clips(()),
                    }
                    MenuItem {
                        label: "Add overlay (V2)…".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_overlay(()),
                    }
                    MenuItem {
                        label: "Add audio (A1)…".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_audio(()),
                    }
                    MenuItem {
                        label: "Add title (T)".to_string(),
                        shortcut: Some("Ctrl+T".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_title(()),
                    }
                    MenuItem {
                        label: if transcribing() { "Transcribing…".to_string() } else { "Auto captions (transcribe)".to_string() },
                        disabled: no_clips || exporting || transcribing(),
                        on_action: move |_| auto_captions(()),
                    }
                    MenuItem {
                        label: "Remove captions".to_string(),
                        disabled: !titles.read().iter().any(|t| t.caption),
                        on_action: move |_| clear_captions(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Export MP4…".to_string(),
                        shortcut: Some("Ctrl+E".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| do_export(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Quit".to_string(),
                        shortcut: Some("Ctrl+Q".to_string()),
                        on_action: move |_| { dioxus::desktop::window().close(); },
                    }
                }
                MorMenuDropdown { label: "Edit".to_string(),
                    MenuItem {
                        label: "Undo".to_string(),
                        shortcut: Some("Ctrl+Z".to_string()),
                        disabled: undo_stack.read().is_empty(),
                        on_action: move |_| undo(()),
                    }
                    MenuItem {
                        label: "Redo".to_string(),
                        shortcut: Some("Ctrl+Shift+Z".to_string()),
                        disabled: redo_stack.read().is_empty(),
                        on_action: move |_| redo(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Set in point at playhead".to_string(),
                        shortcut: Some("I".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_in_here(()),
                    }
                    MenuItem {
                        label: "Set out point at playhead".to_string(),
                        shortcut: Some("O".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_out_here(()),
                    }
                    MenuItem {
                        label: "Split at playhead".to_string(),
                        shortcut: Some("S".to_string()),
                        disabled: no_clips,
                        on_action: move |_| split_at_playhead(()),
                    }
                    MenuItem {
                        label: "Ripple delete".to_string(),
                        shortcut: Some("Delete".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| delete_sel(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Move clip left".to_string(),
                        disabled: !matches!(selected(), Some(Sel::Main(_))),
                        on_action: move |_| move_sel(-1),
                    }
                    MenuItem {
                        label: "Move clip right".to_string(),
                        disabled: !matches!(selected(), Some(Sel::Main(_))),
                        on_action: move |_| move_sel(1),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Add beat marker at playhead".to_string(),
                        shortcut: Some("M".to_string()),
                        on_action: move |_| drop_marker(()),
                    }
                    MenuItem {
                        label: format!("Clear {} marker(s)", markers.read().len()),
                        shortcut: Some("Shift+M".to_string()),
                        disabled: markers.read().is_empty(),
                        on_action: move |_| clear_markers(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Group marked items".to_string(),
                        shortcut: Some("Ctrl+G".to_string()),
                        disabled: marked().len() < 2,
                        on_action: move |_| group_marked(()),
                    }
                    MenuItem {
                        label: "Ungroup".to_string(),
                        shortcut: Some("Ctrl+Shift+G".to_string()),
                        disabled: selected().map(group_of).unwrap_or(0) == 0,
                        on_action: move |_| ungroup_sel(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: format!("{} Magnetic timeline", if magnet() { "●" } else { "○" }),
                        shortcut: Some("~".to_string()),
                        on_action: move |_| toggle_magnet(()),
                    }
                }
                MorMenuDropdown { label: "Playback".to_string(),
                    MenuItem {
                        label: if playing() { "Pause".to_string() } else { "Play".to_string() },
                        shortcut: Some("Space".to_string()),
                        disabled: no_clips,
                        on_action: move |_| toggle_play(()),
                    }
                    MenuItem {
                        label: "Full preview with audio…".to_string(),
                        shortcut: Some("Ctrl+P".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| play_preview(()),
                    }
                }
                MorMenuDropdown { label: "View".to_string(),
                    MenuItem {
                        label: "Pop out monitor".to_string(),
                        disabled: monitor_out(),
                        on_action: move |_| open_monitor(),
                    }
                    MenuItem {
                        label: format!("{} Safe areas (phone UI)", if safe_area() { "●" } else { "○" }),
                        shortcut: Some("G".to_string()),
                        on_action: move |_| toggle_safe(()),
                    }
                    MenuItem {
                        label: format!("{} Transform handles", if show_handles() { "●" } else { "○" }),
                        shortcut: Some("T".to_string()),
                        on_action: move |_| toggle_handles(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: format!("{} Frameless window", radio(UiMode::Frameless)),
                        on_action: move |_| set_mode(UiMode::Frameless),
                    }
                    MenuItem {
                        label: format!("{} Native OS window", radio(UiMode::Native)),
                        on_action: move |_| set_mode(UiMode::Native),
                    }
                    MenuItem {
                        label: format!("{} Tiling WM window", radio(UiMode::Tiling)),
                        on_action: move |_| set_mode(UiMode::Tiling),
                    }
                }
                MorMenuDropdown { label: "Help".to_string(),
                    MenuItem {
                        label: "Keyboard shortcuts…".to_string(),
                        on_action: move |_| show_shortcuts.set(true),
                    }
                    MenuItem {
                        label: "About MorReel Studio…".to_string(),
                        on_action: move |_| show_about.set(true),
                    }
                }
            }),
            status_left: rsx! { span { class: "mor-statusbar-muted", "{status}" } },
            status_right: rsx! {
                if !marked().is_empty() {
                    span { class: "mor-statusbar-chip", "{marked().len()} marked · Ctrl+G groups" }
                }
                if !magnet() {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "magnet off" }
                }
                if preferred_mode() != active_mode {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "window mode: restart to apply" }
                }
                if let Some(warn) = over_limits(total) {
                    span {
                        class: "mor-statusbar-chip mor-statusbar-warn",
                        title: "Longer than this platform accepts for a portrait upload",
                        "{warn}"
                    }
                }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "{fmt_t(total)} total" }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "1080×1920 • 30 fps" }
                span { class: "mr-zoom",
                    button {
                        title: "Zoom timeline out",
                        onclick: move |_| zoom.set((zoom() / 1.25).max(0.25)),
                        "⊖"
                    }
                    input {
                        r#type: "range",
                        class: "mr-zoom-slider",
                        min: "0.25",
                        max: "6",
                        step: "0.05",
                        value: "{zoom}",
                        title: "Timeline zoom",
                        onkeydown: move |evt| evt.stop_propagation(),
                        oninput: move |evt| {
                            if let Ok(v) = evt.value().parse::<f64>() {
                                zoom.set(v);
                            }
                        },
                    }
                    button {
                        title: "Zoom timeline in",
                        onclick: move |_| zoom.set((zoom() * 1.25).min(6.0)),
                        "⊕"
                    }
                }
            },

            div { class: "mr-root",
                // Releasing the mouse ends an interaction, so the next drag of
                // the same slider or item starts a fresh undo step instead of
                // collapsing into the previous one's snapshot.
                onmouseup: move |_| undo_tag.set(String::new()),
                div { class: "mr-work",
                    div { class: "mr-preview-col",
                        if !monitor_out() {
                            div {
                                class: if drop_hover() == Some(Lane::V2) { "mr-phone mr-drop" } else { "mr-phone" },
                                onmounted: move |evt| phone_el.set(Some(evt.data())),
                                oncontextmenu: move |evt| open_ctx(evt, Ctx::Monitor),
                                // Dropping on the picture means "show me this" —
                                // append to the end of the main track.
                                ondragover: move |evt| {
                                    evt.prevent_default();
                                    if drop_hover() != Some(Lane::V2) { drop_hover.set(Some(Lane::V2)); }
                                },
                                ondragleave: move |_| drop_hover.set(None),
                                ondrop: move |evt| {
                                    evt.prevent_default();
                                    let (paths, _) = drop_payload(&evt);
                                    handle_drop(paths, Lane::V1, total_of());
                                },
                                if preview().is_empty() {
                                    span { "Add clips to preview your reel" }
                                } else {
                                    img { src: "{preview}" }
                                }
                                if safe_area() {
                                    SafeArea {}
                                }
                                if let Some(xf) = selected_xf().filter(|_| show_handles()) {
                                    {
                                        let corners = xf_corners(&xf);
                                        // A box bigger than the frame has its corners
                                        // off screen, and the monitor clips. Pull the
                                        // handles back to the edge so they stay
                                        // grabbable — the maths is distance-based, so
                                        // a clamped handle still scales correctly.
                                        let clamp = |v: f64| (v * 100.0).clamp(1.5, 98.5);
                                        let (bw, bh) = (xf.scale * 100.0, xf.scale * 100.0);
                                        let bl = 50.0 + xf.x * 100.0 - bw / 2.0;
                                        let bt = 50.0 + xf.y * 100.0 - bh / 2.0;
                                        rsx! {
                                            div {
                                                class: "mr-xf",
                                                // Only swallow the pointer mid-drag, so a
                                                // right-click still reaches the monitor.
                                                style: if xf_drag().is_some() { "pointer-events:auto" } else { "pointer-events:none" },
                                                onmousemove: move |evt| {
                                                    let Some((grab, from, start, rect)) = xf_drag() else { return };
                                                    let p = evt.client_coordinates();
                                                    set_selected_xf(xf_apply(grab, start, from, (p.x, p.y), rect));
                                                },
                                                onmouseup: move |_| xf_drag.set(None),
                                                onmouseleave: move |_| xf_drag.set(None),
                                                div {
                                                    class: "mr-xf-box",
                                                    style: "left:{bl}%;top:{bt}%;width:{bw}%;height:{bh}%;transform:rotate({xf.rotation}deg)",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        let p = evt.client_coordinates();
                                                        begin_xf(XfGrab::Move, (p.x, p.y));
                                                    },
                                                }
                                                for (n, (fx, fy)) in corners.into_iter().enumerate() {
                                                    div {
                                                        key: "h{n}",
                                                        class: "mr-xf-h",
                                                        title: "Drag to resize \u{2014} corners keep the shape",
                                                        style: "left:{clamp(fx)}%;top:{clamp(fy)}%",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            let p = evt.client_coordinates();
                                                            begin_xf(XfGrab::Scale, (p.x, p.y));
                                                        },
                                                    }
                                                }
                                                div {
                                                    class: "mr-xf-rot",
                                                    style: "left:{clamp(50.0 + xf.x * 100.0)}%;top:{clamp((50.0 + xf.y * 100.0 - bh / 2.0) - 4.0)}%",
                                                    title: "Drag to rotate",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        let p = evt.client_coordinates();
                                                        begin_xf(XfGrab::Rotate, (p.x, p.y));
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !clips.read().is_empty() {
                            div { class: "mr-scrub",
                                // Deck counter: the master timecode readout — amber at
                                // rest, record-red while the transport is rolling.
                                div { class: if playing() { "mr-deck playing" } else { "mr-deck" },
                                    span { "{fmt_t(playhead().min(total))}" }
                                    span { class: "mr-deck-total", "/ {fmt_t(total)}" }
                                }
                                Slider {
                                    label: Some("Playhead"),
                                    min: 0.0,
                                    max: total,
                                    step: 0.05,
                                    precision: 1,
                                    value: playhead().min(total),
                                    oninput: Some(EventHandler::new(move |v: f64| seek_to(v))),
                                }
                                div { class: "mr-play-row",
                                    button {
                                        class: "mor-btn primary",
                                        onclick: move |_| toggle_play(()),
                                        if playing() { "⏸ Pause" } else { "▶ Play" }
                                    }
                                    button {
                                        class: "mor-btn",
                                        disabled: exporting,
                                        title: "Render a fast preview and open it in mpv/ffplay — with audio",
                                        onclick: move |_| play_preview(()),
                                        "🎬 Full preview"
                                    }
                                    button {
                                        class: "mor-btn",
                                        disabled: monitor_out(),
                                        title: "Move the monitor to its own window — edit on one screen, watch on another",
                                        onclick: move |_| open_monitor(),
                                        "⧉ Pop out"
                                    }
                                }
                            }
                        }
                    }

                    div { class: "mr-inspector",
                        div { class: "mr-toolbar",
                            button {
                                class: "mor-btn primary",
                                disabled: importing() || exporting,
                                onclick: move |_| import_clips(()),
                                "＋ Add clips"
                            }
                            button {
                                class: "mor-btn",
                                disabled: clips.read().is_empty() || exporting,
                                onclick: move |_| add_overlay(()),
                                "＋ Overlay (V2)"
                            }
                            button {
                                class: "mor-btn",
                                disabled: clips.read().is_empty() || exporting,
                                onclick: move |_| add_audio(()),
                                "＋ Audio (A1)"
                            }
                            button {
                                class: "mor-btn",
                                disabled: clips.read().is_empty() || exporting,
                                onclick: move |_| add_title(()),
                                "＋ Title"
                            }
                            button {
                                class: "mor-btn mr-export",
                                disabled: clips.read().is_empty() || exporting,
                                onclick: move |_| do_export(()),
                                "⇪ Export MP4"
                            }
                        }

                        if let Some(p) = export_progress() {
                            div { class: "mr-progress",
                                div { style: "width: {p * 100.0:.1}%" }
                            }
                        }

                        div { class: "mr-tabs",
                            button {
                                class: if insp_tab() == "edit" { "mr-tab active" } else { "mr-tab" },
                                onclick: move |_| insp_tab.set("edit"),
                                "Inspector"
                            }
                            button {
                                class: if insp_tab() == "fx" { "mr-tab active" } else { "mr-tab" },
                                onclick: move |_| insp_tab.set("fx"),
                                "Effects"
                            }
                        }

                        if insp_tab() == "fx" {
                            {
                                let cur = match selected() {
                                    Some(Sel::Main(i)) => clips.read().get(i).map(|c| (c.effect.clone(), c.effect_amount)),
                                    Some(Sel::Over(j)) => overlays.read().get(j).map(|o| (o.effect.clone(), o.effect_amount)),
                                    _ => None,
                                };
                                match cur {
                                    Some((current, amount)) => {
                                        let thumbs = fx_thumbs();
                                        // Categories in table order, deduped.
                                        let cats = {
                                            let mut v: Vec<&str> = Vec::new();
                                            for &(c, _, _) in EFFECTS {
                                                if !v.contains(&c) {
                                                    v.push(c);
                                                }
                                            }
                                            v
                                        };
                                        rsx! {
                                            if current != "None" {
                                                Slider {
                                                    label: Some("Effect strength"),
                                                    min: 0.0,
                                                    max: 1.0,
                                                    step: 0.05,
                                                    precision: 2,
                                                    value: amount,
                                                    oninput: Some(EventHandler::new(move |v: f64| set_effect_amount(v))),
                                                }
                                            }
                                            for cat in cats {
                                                h4 { class: "mr-fx-cat", "{cat}" }
                                                div { class: "mr-fx-grid",
                                                    for (_, name, _) in EFFECTS.iter().copied().filter(move |&(c, _, _)| c == cat) {
                                                        button {
                                                            key: "{name}",
                                                            class: if current == name { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                            onclick: move |_| apply_effect(name.to_string()),
                                                            if let Some(uri) = thumbs.get(name) {
                                                                img { src: "{uri}" }
                                                            } else {
                                                                div { class: "mr-fx-ph" }
                                                            }
                                                            span { "{name}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    None => rsx! {
                                        p { class: "mor-statusbar-muted",
                                            "Effects apply to video — select a V1 clip or V2 overlay on the timeline, then pick a look. Motion looks are ports of moranima's camera moves."
                                        }
                                    },
                                }
                            }
                        } else {

                        {match selected() {
                            Some(Sel::Main(i)) if i < clips.read().len() => {
                                let c = clips.read()[i].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag", "V1" }
                                            " {c.name}"
                                        }
                                        p { class: "mor-statusbar-muted", "{clip_note(&c)}" }
                                    }
                                    Slider {
                                        label: Some("In point"),
                                        min: 0.0,
                                        max: c.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: c.in_s,
                                        oninput: Some(EventHandler::new({
                                            let path = c.scrub_path();
                                            let fr = c.framing.clone();
                                            let eff = c.look();
                                            move |v: f64| {
                                                push_undo(&format!("in{i}"));
                                                let old = spans();
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].in_s = v.min(cl[i].out_s - 0.1).max(0.0);
                                                    cl[i].in_s
                                                };
                                                ride(old, &|k| Some(start_of(k)));
                                                playhead.set(start_of(i));
                                                request_preview(path.clone(), t, fr.clone(), eff.clone(), engine::Over::default());
                                            }
                                        })),
                                    }
                                    Slider {
                                        label: Some("Out point"),
                                        min: 0.0,
                                        max: c.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: c.out_s,
                                        oninput: Some(EventHandler::new({
                                            let path = c.scrub_path();
                                            let fr = c.framing.clone();
                                            let eff = c.look();
                                            move |v: f64| {
                                                push_undo(&format!("out{i}"));
                                                let old = spans();
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].out_s = v.max(cl[i].in_s + 0.1).min(cl[i].duration);
                                                    cl[i].out_s
                                                };
                                                ride(old, &|k| Some(start_of(k)));
                                                playhead.set(start_of(i + 1));
                                                request_preview(path.clone(), t, fr.clone(), eff.clone(), engine::Over::default());
                                            }
                                        })),
                                    }
                                    MorSelect {
                                        label: "Effect".to_string(),
                                        value: c.effect.clone(),
                                        options: effect_names.clone(),
                                        onchange: {
                                            let path = c.scrub_path();
                                            let fr = c.framing.clone();
                                            let amt = c.effect_amount;
                                            move |name: String| {
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].effect = name.clone();
                                                    cl[i].in_s
                                                };
                                                request_preview(path.clone(), t, fr.clone(), effect_filter_amt(&name, amt), engine::Over::default());
                                            }
                                        },
                                    }
                                    if c.effect != "None" {
                                        Slider {
                                            label: Some("Effect strength"),
                                            min: 0.0,
                                            max: 1.0,
                                            step: 0.05,
                                            precision: 2,
                                            value: c.effect_amount,
                                            oninput: Some(EventHandler::new(move |v: f64| set_effect_amount(v))),
                                        }
                                    }
                                    MorSelect {
                                        label: "Framing (9:16)".to_string(),
                                        value: c.framing.clone(),
                                        options: FRAMINGS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: {
                                            let path = c.scrub_path();
                                            let eff = c.look();
                                            move |name: String| {
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].framing = name.clone();
                                                    cl[i].in_s
                                                };
                                                request_preview(path.clone(), t, name, eff.clone(), engine::Over::default());
                                            }
                                        },
                                    }
                                    // A photo has no motion to retime — its length
                                    // is just the Out point.
                                    if !engine::is_still(&c.path) {
                                    Slider {
                                        label: Some("Speed (×)"),
                                        min: 0.25,
                                        max: 4.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: c.speed,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            // Retiming changes how long the clip runs, so the
                                            // magnet has to carry attached items the same way a
                                            // trim does.
                                            push_undo(&format!("speed{i}"));
                                            let old = spans();
                                            clips.write()[i].speed = v.max(0.05);
                                            ride(old, &|k| Some(start_of(k)));
                                        })),
                                    }
                                    }
                                    if c.has_audio {
                                        Slider {
                                            label: Some("Clip volume"),
                                            min: 0.0,
                                            max: 2.0,
                                            step: 0.05,
                                            precision: 2,
                                            value: c.volume,
                                            oninput: Some(EventHandler::new(move |v: f64| {
                                                push_undo(&format!("cvol{i}"));
                                                clips.write()[i].volume = v;
                                            })),
                                        }
                                    }
                                    if i > 0 {
                                        h4 { class: "mr-fx-cat", "Transition in" }
                                        MorSelect {
                                            label: "Transition".to_string(),
                                            value: c.transition.clone(),
                                            options: engine::TRANSITIONS.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>(),
                                            onchange: move |v: String| {
                                                push_undo("");
                                                let old = spans();
                                                clips.write()[i].transition = v;
                                                ride(old, &|k| Some(start_of(k)));
                                                seek_to(playhead().min(total_of()));
                                            },
                                        }
                                        if !engine::xfade_name(&c.transition).is_empty() {
                                            Slider {
                                                label: Some("Transition length"),
                                                min: 0.1,
                                                max: 3.0,
                                                step: 0.05,
                                                precision: 2,
                                                value: c.trans_dur,
                                                oninput: Some(EventHandler::new(move |v: f64| {
                                                    push_undo(&format!("tdur{i}"));
                                                    let old = spans();
                                                    clips.write()[i].trans_dur = v;
                                                    ride(old, &|k| Some(start_of(k)));
                                                    seek_to(playhead().min(total_of()));
                                                })),
                                            }
                                            p { class: "mor-statusbar-muted mr-export-blurb",
                                                "Overlaps the clip before it, so the reel gets shorter by this much."
                                            }
                                        }
                                    }
                                    {transform_panel(false)}
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn", onclick: move |_| move_sel(-1), "◀ Move left" }
                                        button { class: "mor-btn", onclick: move |_| move_sel(1), "Move right ▶" }
                                        button { class: "mor-btn", onclick: move |_| split_at_playhead(()), "✂ Split at playhead" }
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Ripple delete" }
                                    }
                                }
                            }
                            Some(Sel::Over(j)) if j < overlays.read().len() => {
                                let o = overlays.read()[j].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag", "V2" }
                                            " {o.name}"
                                        }
                                        p { class: "mor-statusbar-muted",
                                            "Cutaway covers V1 from {fmt_t(o.at)} for {fmt_t(o.trimmed())} — main audio keeps playing."
                                        }
                                    }
                                    Slider {
                                        label: Some("Position on timeline"),
                                        min: 0.0,
                                        max: total.max(0.5),
                                        step: 0.05,
                                        precision: 1,
                                        value: o.at,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("opos{j}"));
                                            overlays.write()[j].at = v.max(0.0);
                                        })),
                                    }
                                    Slider {
                                        label: Some("In point"),
                                        min: 0.0,
                                        max: o.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: o.in_s,
                                        oninput: Some(EventHandler::new({
                                            let path = o.scrub_path();
                                            let fr = o.framing.clone();
                                            let eff = o.look();
                                            move |v: f64| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].in_s = v.min(ov[j].out_s - 0.1).max(0.0);
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, fr.clone(), eff.clone(), engine::Over::default());
                                            }
                                        })),
                                    }
                                    Slider {
                                        label: Some("Out point"),
                                        min: 0.0,
                                        max: o.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: o.out_s,
                                        oninput: Some(EventHandler::new({
                                            let path = o.scrub_path();
                                            let fr = o.framing.clone();
                                            let eff = o.look();
                                            move |v: f64| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].out_s = v.max(ov[j].in_s + 0.1).min(ov[j].duration);
                                                    ov[j].out_s
                                                };
                                                request_preview(path.clone(), t, fr.clone(), eff.clone(), engine::Over::default());
                                            }
                                        })),
                                    }
                                    MorSelect {
                                        label: "Effect".to_string(),
                                        value: o.effect.clone(),
                                        options: effect_names.clone(),
                                        onchange: {
                                            let path = o.scrub_path();
                                            let fr = o.framing.clone();
                                            let amt = o.effect_amount;
                                            move |name: String| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].effect = name.clone();
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, fr.clone(), effect_filter_amt(&name, amt), engine::Over::default());
                                            }
                                        },
                                    }
                                    if o.effect != "None" {
                                        Slider {
                                            label: Some("Effect strength"),
                                            min: 0.0,
                                            max: 1.0,
                                            step: 0.05,
                                            precision: 2,
                                            value: o.effect_amount,
                                            oninput: Some(EventHandler::new(move |v: f64| set_effect_amount(v))),
                                        }
                                    }
                                    MorSelect {
                                        label: "Framing (9:16)".to_string(),
                                        value: o.framing.clone(),
                                        options: FRAMINGS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: {
                                            let path = o.scrub_path();
                                            let eff = o.look();
                                            move |name: String| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].framing = name.clone();
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, name, eff.clone(), engine::Over::default());
                                            }
                                        },
                                    }
                                    Slider {
                                        label: Some("Speed (×)"),
                                        min: 0.25,
                                        max: 4.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: o.speed,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("ospeed{j}"));
                                            overlays.write()[j].speed = v.max(0.05);
                                        })),
                                    }
                                    {transform_panel(true)}
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove overlay" }
                                    }
                                }
                            }
                            Some(Sel::Title(k)) if k < titles.read().len() => {
                                let t = titles.read()[k].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag title", "T" }
                                            if t.caption { " Caption" } else if t.kind != "Text" { " Shape" } else { " Title" }
                                        }
                                        p { class: "mor-statusbar-muted",
                                            "Shown from {fmt_t(t.at)} for {fmt_t(t.dur)}"
                                            if t.pngs.is_empty() { " • rendering…" }
                                        }
                                    }
                                    MorSelect {
                                        label: "Card".to_string(),
                                        value: t.kind.clone(),
                                        options: engine::TITLE_KINDS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            push_undo("");
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.kind = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    if t.kind != "Text" {
                                        for (label, value, set) in shape_knobs(&t) {
                                            Slider {
                                                key: "{label}",
                                                label: Some(label),
                                                min: if label == "Across" { -0.5 } else { 0.01 },
                                                max: if label == "Across" { 0.5 } else { 1.0 },
                                                step: 0.01,
                                                precision: 2,
                                                value,
                                                oninput: Some(EventHandler::new(move |v: f64| {
                                                    push_undo(&format!("shape{label}{k}"));
                                                    if let Some(item) = titles.write().get_mut(k) {
                                                        set(item, v);
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(k);
                                                })),
                                            }
                                        }
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Outline above 0 makes it a hollow ring; Position sets its height on the frame."
                                        }
                                    }
                                    if t.kind == "Text" {
                                    mor_rust_dioxus_ui_kit::MorTextInput {
                                        label: "Text (type \\n for a line break)".to_string(),
                                        value: t.text.clone(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.text = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    Slider {
                                        label: Some("Position on timeline"),
                                        min: 0.0,
                                        max: total.max(0.5),
                                        step: 0.05,
                                        precision: 1,
                                        value: t.at,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            titles.write()[k].at = v.max(0.0);
                                            seek_to(v.max(0.0));
                                        })),
                                    }
                                    Slider {
                                        label: Some("Duration"),
                                        min: 0.5,
                                        max: 20.0,
                                        step: 0.1,
                                        precision: 1,
                                        value: t.dur,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("tdur{k}"));
                                            titles.write()[k].dur = v;
                                        })),
                                    }
                                    Slider {
                                        label: Some("Font size"),
                                        min: 40.0,
                                        max: 240.0,
                                        step: 2.0,
                                        precision: 0,
                                        value: t.font_size,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.font_size = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        })),
                                    }
                                    MorSelect {
                                        label: "Color".to_string(),
                                        value: t.color.clone(),
                                        options: TITLE_COLORS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.color = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Font".to_string(),
                                        value: t.font.clone(),
                                        options: engine::font_families().to_vec(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.font = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Line-up".to_string(),
                                        value: t.align.clone(),
                                        options: engine::ALIGNMENTS.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.align = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Entrance".to_string(),
                                        value: t.anim.clone(),
                                        options: engine::TITLE_ANIMS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            push_undo("");
                                            titles.write()[k].anim = v;
                                        },
                                    }
                                    MorSelect {
                                        label: "Words appear".to_string(),
                                        value: if t.reveal { "One at a time".to_string() } else { "All at once".to_string() },
                                        options: vec!["All at once".to_string(), "One at a time".to_string()],
                                        onchange: move |v: String| {
                                            push_undo("");
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.reveal = v == "One at a time";
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    if t.reveal {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "{t.segments().len()} card(s) — one per word, revealed over the first 60% of the title, then held."
                                        }
                                    }
                                    if t.anim != "None" {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Slides on and off with the fade. The monitor shows the card in place — press Ctrl+P to watch it move."
                                        }
                                    }
                                    }
                                    MorSelect {
                                        label: "Backdrop".to_string(),
                                        value: if t.boxed { "Box".to_string() } else { "Transparent".to_string() },
                                        options: vec!["Transparent".to_string(), "Box".to_string()],
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.boxed = v == "Box";
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    // An outline is the transparent-friendly way to stay
                                    // legible over busy video — no plate needed.
                                    Slider {
                                        label: Some("Outline"),
                                        min: 0.0,
                                        max: 20.0,
                                        step: 1.0,
                                        precision: 0,
                                        value: t.outline,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.outline = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        })),
                                    }
                                    if t.outline > 0.0 {
                                        MorSelect {
                                            label: "Outline colour".to_string(),
                                            value: t.outline_color.clone(),
                                            options: TITLE_COLORS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>(),
                                            onchange: move |v: String| {
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    item.outline_color = v;
                                                    item.pngs.clear();
                                                }
                                                rerender_title(k);
                                            },
                                        }
                                    }
                                    MorSelect {
                                        label: "Position".to_string(),
                                        value: t.pos.clone(),
                                        options: TITLE_POS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.pos = v;
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Bevel".to_string(),
                                        value: bevel_label(&t.bevel),
                                        options: BEVELS.iter().map(|(_, l)| l.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.bevel = bevel_value(&v);
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    if t.bevel != "Off" {
                                        // The designer app's full control set, same
                                        // ranges and same plain-English labels. Only
                                        // shown once a bevel is actually on.
                                        h4 { class: "mr-fx-cat", "Bevel — light and relief" }
                                        for (label, value, max, step, set) in bevel_knobs(&t) {
                                            Slider {
                                                key: "{label}",
                                                label: Some(label),
                                                min: 0.0,
                                                max,
                                                step,
                                                precision: if step < 1.0 { 2 } else { 0 },
                                                value,
                                                oninput: Some(EventHandler::new(move |v: f64| {
                                                    if let Some(item) = titles.write().get_mut(k) {
                                                        set(item, v);
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(k);
                                                })),
                                            }
                                        }
                                    }
                                    if !presets.read().is_empty() {
                                        MorSelect {
                                            label: "Apply a saved style".to_string(),
                                            value: "—".to_string(),
                                            options: std::iter::once("—".to_string())
                                                .chain(presets.read().iter().map(|p| p.name.clone()))
                                                .collect::<Vec<_>>(),
                                            onchange: move |v: String| {
                                                let found = presets.read().iter().find(|p| p.name == v).cloned();
                                                let Some(p) = found else { return };
                                                push_undo("");
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    *item = restyle(item, &p.style);
                                                }
                                                rerender_title(k);
                                                status.set(format!("Applied \"{}\".", p.name));
                                            },
                                        }
                                    }
                                    button {
                                        class: "mor-btn mr-reset",
                                        title: "Keep this card's look to use on other titles and other reels",
                                        onclick: move |_| show_save_preset.set(true),
                                        "☆ Save this style as a preset"
                                    }
                                    // Auto captions can leave forty title items on the
                                    // lane. Restyling them one at a time is not a job
                                    // anyone should do twice.
                                    if titles.read().iter().filter(|x| x.caption).count() > 1 {
                                        button {
                                            class: "mor-btn mr-reset",
                                            title: "Copy this card's look — font, size, colour, backdrop, outline, bevel — onto every caption",
                                            onclick: move |_| {
                                                push_undo("");
                                                let Some(src) = titles.read().get(k).cloned() else { return };
                                                let mut n = 0;
                                                for t in titles.write().iter_mut().filter(|t| t.caption) {
                                                    *t = restyle(t, &src);
                                                    n += 1;
                                                }
                                                spawn(async move {
                                                    ensure_titles().await;
                                                    seek_to(playhead());
                                                });
                                                status.set(format!("Restyled {n} caption(s)."));
                                            },
                                            "⇊ Apply this style to all captions"
                                        }
                                    }
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove title" }
                                    }
                                }
                            }
                            Some(Sel::Aud(k)) if k < audios.read().len() => {
                                let a = audios.read()[k].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag audio", "A1" }
                                            " {a.name}"
                                        }
                                        p { class: "mor-statusbar-muted",
                                            "Mixed under the main track from {fmt_t(a.at)} for {fmt_t(a.out_s - a.in_s)}."
                                        }
                                    }
                                    Slider {
                                        label: Some("Position on timeline"),
                                        min: 0.0,
                                        max: total.max(0.5),
                                        step: 0.05,
                                        precision: 1,
                                        value: a.at,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("apos{k}"));
                                            audios.write()[k].at = v.max(0.0);
                                        })),
                                    }
                                    Slider {
                                        label: Some("In point"),
                                        min: 0.0,
                                        max: a.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: a.in_s,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            let mut au = audios.write();
                                            au[k].in_s = v.min(au[k].out_s - 0.1).max(0.0);
                                        })),
                                    }
                                    Slider {
                                        label: Some("Out point"),
                                        min: 0.0,
                                        max: a.duration,
                                        step: 0.05,
                                        precision: 1,
                                        value: a.out_s,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            let mut au = audios.write();
                                            au[k].out_s = v.max(au[k].in_s + 0.1).min(au[k].duration);
                                        })),
                                    }
                                    Slider {
                                        label: Some("Volume"),
                                        min: 0.0,
                                        max: 2.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.volume,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("avol{k}"));
                                            audios.write()[k].volume = v;
                                        })),
                                    }
                                    Slider {
                                        label: Some("Duck under video"),
                                        min: 0.0,
                                        max: 1.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.duck,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("aduck{k}"));
                                            audios.write()[k].duck = v;
                                        })),
                                    }
                                    if a.duck > 0.0 {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Pulls this bed down whenever the main track is talking, and lets it back up in the gaps."
                                        }
                                    }
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove audio" }
                                    }
                                }
                            }
                            _ => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Add portrait or landscape clips — each clip's Framing picks crop, letterbox fit, or zoom into 9:16. Select an item on the timeline to edit it."
                                }
                            },
                        }}

                        }

                        p { class: "mor-statusbar-muted mr-keys",
                            "M beat marker · T transform handles · Drop files onto a lane · Ctrl+Z undo · I/O trim · S split · Del ripple delete · ←/→ scrub (Shift = 1s) · drag to move (snaps) · Ctrl+G group · ~ magnet · G safe areas · Ctrl+E export"
                        }
                    }
                }

                div {
                    class: if drop_hover() == Some(Lane::V1) { "mr-timeline mr-drop" } else { "mr-timeline" },
                    oncontextmenu: move |evt| open_ctx(evt, Ctx::Timeline),
                    // Fallback drop target: an empty timeline has no lanes yet,
                    // and the ruler and the gaps between lanes are still the
                    // timeline. Everything here appends to the end of V1.
                    ondragover: move |evt| {
                        evt.prevent_default();
                        if drop_hover().is_none() { drop_hover.set(Some(Lane::V1)); }
                    },
                    ondragleave: move |_| drop_hover.set(None),
                    ondrop: move |evt| {
                        evt.prevent_default();
                        let (paths, _) = drop_payload(&evt);
                        handle_drop(paths, Lane::V1, total_of());
                    },
                    // Middle-mouse drag pans the timeline on both axes.
                    onmousedown: move |evt| {
                        if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Auxiliary) {
                            evt.prevent_default(); // no Linux middle-click paste / autoscroll
                            let p = evt.client_coordinates();
                            pan.set(Some((p.x, p.y)));
                        }
                    },
                    onmousemove: move |evt| {
                        let p = evt.client_coordinates();
                        if let Some((px, py)) = pan() {
                            pan.set(Some((p.x, p.y)));
                            let _ = dioxus::document::eval(&format!(
                                "document.querySelector('.mr-timeline').scrollBy({:.1}, {:.1});",
                                px - p.x,
                                py - p.y
                            ));
                            return;
                        }
                        let Some((target, last_x, t0, acc)) = drag() else { return };
                        let dx = p.x - last_x;
                        if dx == 0.0 {
                            return;
                        }
                        let acc = acc + dx.abs(); // 4px dead zone tells a click from a drag
                        let dt = dx / calc_scale();
                        match target {
                            Sel::Main(i) if i < clips.read().len() => {
                                let t0 = t0 + dt;
                                if acc > 4.0 {
                                    let (lo, len) = block_of(i);
                                    let bdur: f64 =
                                        clips.read()[lo..lo + len].iter().map(Clip::trimmed).sum();
                                    let center = t0 + bdur / 2.0;
                                    // Insertion point: how many other clips' midpoints
                                    // the block's center has passed.
                                    let mut dest = 0;
                                    let mut walked = 0.0;
                                    for (k, c) in clips.read().iter().enumerate() {
                                        if k >= lo && k < lo + len {
                                            continue;
                                        }
                                        if center > walked + c.trimmed() / 2.0 {
                                            dest += 1;
                                        }
                                        walked += c.trimmed();
                                    }
                                    if dest != lo {
                                        move_block(lo, len, dest);
                                        let ni = dest + (i - lo);
                                        selected.set(Some(Sel::Main(ni)));
                                        drag.set(Some((Sel::Main(ni), p.x, t0, acc)));
                                        return;
                                    }
                                }
                                drag.set(Some((Sel::Main(i), p.x, t0, acc)));
                            }
                            other => {
                                if acc > 4.0 {
                                    // Snap the item's leading edge to the nearest cut,
                                    // the end of the reel, or the playhead — within a
                                    // few pixels, so it never fights a deliberate drag.
                                    let dt = match lane_at(other) {
                                        Some(at) => {
                                            let scale = calc_scale();
                                            let mut targets: Vec<f64> =
                                                spans().iter().map(|&(s, _)| s).collect();
                                            targets.push(total_of());
                                            targets.push(playhead());
                                            targets.extend(markers.read().iter().copied());
                                            snap_to(at + dt, &targets, 6.0 / scale) - at
                                        }
                                        None => dt,
                                    };
                                    shift_lane(other, dt);
                                }
                                drag.set(Some((other, p.x, t0, acc)));
                            }
                        }
                    },
                    onmouseup: move |_| {
                        if let Some((_, _, _, acc)) = drag() {
                            if acc > 4.0 {
                                drag_moved.set(true); // swallow the click that follows
                            }
                        }
                        drag.set(None);
                        pan.set(None);
                        scrubbing.set(false);
                    },
                    onmouseleave: move |_| {
                        drag.set(None);
                        pan.set(None);
                        scrubbing.set(false);
                    },
                    if clips.read().is_empty() {
                        span { class: "mor-statusbar-muted mr-timeline-hint", "Drop media here, or Add clips (Ctrl+O) — your story builds left to right" }
                    } else {
                        {
                            let scale = calc_scale();
                            let ext = extents(&clips.read());
                            let track_end = total
                                .max(overlays.read().iter().map(|o| o.at + o.trimmed()).fold(0.0, f64::max))
                                .max(titles.read().iter().map(|t| t.at + t.dur).fold(0.0, f64::max))
                                .max(audios.read().iter().map(|a| a.at + a.out_s - a.in_s).fold(0.0, f64::max));
                            // Adaptive ruler: a labeled tick every ~72px whatever the
                            // zoom, minor ticks at quarter steps (dropped when the
                            // timeline is long enough that they'd flood the DOM).
                            let tick_s = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0]
                                .into_iter()
                                .find(|s| s * scale >= 72.0)
                                .unwrap_or(120.0);
                            let minor_s = if track_end / tick_s > 400.0 { tick_s } else { tick_s / 4.0 };
                            let per = (tick_s / minor_s).round() as usize;
                            let ph = playhead().min(total);
                            rsx! {
                                div { class: "mr-track", style: "width: {track_end * scale}px",
                                    div {
                                        class: "mr-ruler",
                                        // Press seeks, holding drags the playhead along.
                                        onmousedown: move |evt| {
                                            if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) {
                                                scrubbing.set(true);
                                                seek_to((evt.element_coordinates().x / calc_scale()).clamp(0.0, total_of()));
                                            }
                                        },
                                        onmousemove: move |evt| {
                                            if scrubbing() {
                                                seek_to((evt.element_coordinates().x / calc_scale()).clamp(0.0, total_of()));
                                            }
                                        },
                                        for k in 0..=((track_end / minor_s) as usize) {
                                            span {
                                                class: if k % per == 0 { "mr-tick major" } else { "mr-tick" },
                                                style: "left: {k as f64 * minor_s * scale}px",
                                                if k % per == 0 {
                                                    "{fmt_t(k as f64 * minor_s)}"
                                                }
                                            }
                                        }
                                    }
                                    for (n, m) in markers.read().iter().copied().enumerate() {
                                        div {
                                            key: "mk{n}",
                                            class: "mr-marker",
                                            style: "left: {m * scale}px",
                                            title: "Beat marker at {fmt_t(m)} — Shift+M clears all",
                                        }
                                    }
                                    div { class: "mr-lane",
                                        span { class: "mr-lane-tag title", "T" }
                                        for (k, t) in titles().into_iter().enumerate() {
                                            div {
                                                key: "title-{k}",
                                                class: item_class("mr-lane-item title", selected() == Some(Sel::Title(k)), marked().contains(&Sel::Title(k))),
                                                style: "left: {t.at * scale}px; width: {t.dur * scale}px",
                                                onmousedown: move |evt| {
                                                    if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) && !evt.modifiers().ctrl() {
                                                        selected.set(Some(Sel::Title(k)));
                                                        drag.set(Some((Sel::Title(k), evt.client_coordinates().x, 0.0, 0.0)));
                                                    }
                                                },
                                                onclick: move |evt| {
                                                    if drag_moved() {
                                                        drag_moved.set(false);
                                                        return;
                                                    }
                                                    if evt.modifiers().ctrl() {
                                                        toggle_mark(Sel::Title(k));
                                                        return;
                                                    }
                                                    let at = titles.read()[k].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Title(k)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Title(k)));
                                                    open_ctx(evt, Ctx::Title(k));
                                                },
                                                if t.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(t.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                if t.kind == "Text" { "𝐓 {t.text}" } else { "◧ {t.kind}" }
                                            }
                                        }
                                    }
                                    div {
                                        class: if drop_hover() == Some(Lane::V2) { "mr-lane mr-drop" } else { "mr-lane" },
                                        ondragover: move |evt| {
                                            // Dioxus's runtime already prevents the window
                                            // default (it would navigate to the file); this is
                                            // the per-element half of the same contract.
                                            evt.prevent_default();
                                            evt.stop_propagation();
                                            if drop_hover() != Some(Lane::V2) { drop_hover.set(Some(Lane::V2)); }
                                        },
                                        ondragleave: move |_| {
                                            if drop_hover() == Some(Lane::V2) { drop_hover.set(None); }
                                        },
                                        ondrop: move |evt| {
                                            evt.prevent_default();
                                            evt.stop_propagation(); // else the timeline fallback imports it twice
                                            let (paths, t) = drop_payload(&evt);
                                            handle_drop(paths, Lane::V2, t);
                                        },
                                        span { class: "mr-lane-tag", "V2" }
                                        for (j, o) in overlays().into_iter().enumerate() {
                                            div {
                                                key: "{j}-{o.path}",
                                                class: item_class("mr-lane-item", selected() == Some(Sel::Over(j)), marked().contains(&Sel::Over(j))),
                                                style: "left: {o.at * scale}px; width: {o.trimmed() * scale}px",
                                                onmousedown: move |evt| {
                                                    if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) && !evt.modifiers().ctrl() {
                                                        selected.set(Some(Sel::Over(j)));
                                                        drag.set(Some((Sel::Over(j), evt.client_coordinates().x, 0.0, 0.0)));
                                                    }
                                                },
                                                onclick: move |evt| {
                                                    if drag_moved() {
                                                        drag_moved.set(false);
                                                        return;
                                                    }
                                                    if evt.modifiers().ctrl() {
                                                        toggle_mark(Sel::Over(j));
                                                        return;
                                                    }
                                                    let at = overlays.read()[j].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Over(j)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Over(j)));
                                                    open_ctx(evt, Ctx::Over(j));
                                                },
                                                if o.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(o.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                "{o.name}"
                                            }
                                        }
                                    }
                                    div {
                                        class: if drop_hover() == Some(Lane::V1) { "mr-clips mr-drop" } else { "mr-clips" },
                                        ondragover: move |evt| {
                                            // Dioxus's runtime already prevents the window
                                            // default (it would navigate to the file); this is
                                            // the per-element half of the same contract.
                                            evt.prevent_default();
                                            evt.stop_propagation();
                                            if drop_hover() != Some(Lane::V1) { drop_hover.set(Some(Lane::V1)); }
                                        },
                                        ondragleave: move |_| {
                                            if drop_hover() == Some(Lane::V1) { drop_hover.set(None); }
                                        },
                                        ondrop: move |evt| {
                                            evt.prevent_default();
                                            evt.stop_propagation(); // else the timeline fallback imports it twice
                                            let (paths, t) = drop_payload(&evt);
                                            handle_drop(paths, Lane::V1, t);
                                        },
                                        span { class: "mr-lane-tag", title: "Primary story — drag clips to reorder", "V1" }
                                        for (i, c) in clips().into_iter().enumerate() {
                                            div {
                                                key: "{i}-{c.path}",
                                                class: item_class("mr-clip", selected() == Some(Sel::Main(i)), marked().contains(&Sel::Main(i))),
                                                style: "width: {ext[i] * scale}px",
                                                onmousedown: move |evt| {
                                                    if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) && !evt.modifiers().ctrl() {
                                                        selected.set(Some(Sel::Main(i)));
                                                        let (lo, _) = block_of(i);
                                                        drag.set(Some((Sel::Main(i), evt.client_coordinates().x, start_of(lo), 0.0)));
                                                    }
                                                },
                                                onclick: move |evt| {
                                                    if drag_moved() {
                                                        drag_moved.set(false);
                                                        return;
                                                    }
                                                    if evt.modifiers().ctrl() {
                                                        toggle_mark(Sel::Main(i));
                                                        return;
                                                    }
                                                    select_clip(i)
                                                },
                                                // Right-click selects without moving the playhead.
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Main(i)));
                                                    open_ctx(evt, Ctx::Clip(i));
                                                },
                                                if c.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(c.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                if fade_in(&clips.read(), i) > 0.0 {
                                                    span {
                                                        class: "mr-xtrans",
                                                        title: "{c.transition}",
                                                        "><"
                                                    }
                                                }
                                                if c.thumb.is_empty() {
                                                    div { class: "mr-thumb-missing" }
                                                } else {
                                                    img { src: "{c.thumb}" }
                                                }
                                                if !c.wave.is_empty() {
                                                    div {
                                                        class: "mr-clip-wave",
                                                        title: "This clip's own audio",
                                                        style: "{wave_css(&c.wave, c.duration, c.in_s, scale, c.speed)}",
                                                    }
                                                }
                                                span { class: "mr-clip-name",
                                                    if c.effect != "None" { "✨ " }
                                                    "{c.name}"
                                                }
                                                span { class: "mr-clip-dur", "{fmt_t(c.trimmed())}" }
                                            }
                                        }
                                    }
                                    div {
                                        class: if drop_hover() == Some(Lane::A1) { "mr-lane mr-lane-a1 mr-drop" } else { "mr-lane mr-lane-a1" },
                                        ondragover: move |evt| {
                                            // Dioxus's runtime already prevents the window
                                            // default (it would navigate to the file); this is
                                            // the per-element half of the same contract.
                                            evt.prevent_default();
                                            evt.stop_propagation();
                                            if drop_hover() != Some(Lane::A1) { drop_hover.set(Some(Lane::A1)); }
                                        },
                                        ondragleave: move |_| {
                                            if drop_hover() == Some(Lane::A1) { drop_hover.set(None); }
                                        },
                                        ondrop: move |evt| {
                                            evt.prevent_default();
                                            evt.stop_propagation(); // else the timeline fallback imports it twice
                                            let (paths, t) = drop_payload(&evt);
                                            handle_drop(paths, Lane::A1, t);
                                        },
                                        span { class: "mr-lane-tag", "A1" }
                                        for (k, a) in audios().into_iter().enumerate() {
                                            div {
                                                key: "{k}-{a.path}",
                                                class: item_class("mr-lane-item audio", selected() == Some(Sel::Aud(k)), marked().contains(&Sel::Aud(k))),
                                                style: "left: {a.at * scale}px; width: {(a.out_s - a.in_s) * scale}px; {wave_css(&a.wave, a.duration, a.in_s, scale, 1.0)}",
                                                onmousedown: move |evt| {
                                                    if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) && !evt.modifiers().ctrl() {
                                                        selected.set(Some(Sel::Aud(k)));
                                                        drag.set(Some((Sel::Aud(k), evt.client_coordinates().x, 0.0, 0.0)));
                                                    }
                                                },
                                                onclick: move |evt| {
                                                    if drag_moved() {
                                                        drag_moved.set(false);
                                                        return;
                                                    }
                                                    if evt.modifiers().ctrl() {
                                                        toggle_mark(Sel::Aud(k));
                                                        return;
                                                    }
                                                    let at = audios.read()[k].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Aud(k)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Aud(k)));
                                                    open_ctx(evt, Ctx::Aud(k));
                                                },
                                                if a.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(a.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                "♪ {a.name}"
                                            }
                                        }
                                    }
                                    div { class: "mr-playhead", style: "left: {ph * scale}px",
                                        span { class: "mr-ph-badge", "{fmt_t(ph)}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some((cx, cy, target)) = ctx_menu() {
            // Invisible backdrop: any click or second right-click dismisses.
            div {
                class: "mr-ctx-backdrop",
                onclick: move |_| ctx_menu.set(None),
                oncontextmenu: move |evt| {
                    evt.prevent_default();
                    ctx_menu.set(None);
                },
                div {
                    class: "mor-menu-dropdown mr-ctx",
                    // transform % resolves against the menu's own box, so it slides
                    // on screen exactly — no estimated heights to keep in sync.
                    style: "left: {cx}px; top: {cy}px; transform: translate(min(0px, calc(100vw - {cx}px - 100%)), min(0px, calc(100vh - {cy}px - 100%)));",
                    {match target {
                        Ctx::Monitor => rsx! {
                            div { class: "mr-ctx-head",
                                span { class: "mr-ctx-tag", "MON" }
                                span { class: "mr-ctx-name", "Program monitor" }
                            }
                            CtxItem {
                                label: if playing() { "Pause".to_string() } else { "Play".to_string() },
                                shortcut: Some("Space".to_string()),
                                disabled: no_clips,
                                on_action: move |_| toggle_play(()),
                            }
                            CtxItem {
                                label: "Full preview with audio…".to_string(),
                                shortcut: Some("Ctrl+P".to_string()),
                                disabled: no_clips || exporting,
                                on_action: move |_| play_preview(()),
                            }
                            MenuSeparator {}
                            CtxItem {
                                label: "Open monitor in new window".to_string(),
                                on_action: move |_| open_monitor(),
                            }
                        },
                        Ctx::Timeline => rsx! {
                            CtxItem {
                                label: "Add clips…".to_string(),
                                shortcut: Some("Ctrl+O".to_string()),
                                disabled: importing() || exporting,
                                on_action: move |_| import_clips(()),
                            }
                            CtxItem {
                                label: "Add overlay (V2)…".to_string(),
                                disabled: no_clips || exporting,
                                on_action: move |_| add_overlay(()),
                            }
                            CtxItem {
                                label: "Add audio (A1)…".to_string(),
                                disabled: no_clips || exporting,
                                on_action: move |_| add_audio(()),
                            }
                            CtxItem {
                                label: "Add title (T)".to_string(),
                                shortcut: Some("Ctrl+T".to_string()),
                                disabled: no_clips || exporting,
                                on_action: move |_| add_title(()),
                            }
                            MenuSeparator {}
                            CtxItem {
                                label: "Export MP4…".to_string(),
                                shortcut: Some("Ctrl+E".to_string()),
                                disabled: no_clips || exporting,
                                on_action: move |_| do_export(()),
                            }
                        },
                        Ctx::Clip(i) => {
                            let len = clips.read().len();
                            let name = clips.read().get(i).map(|c| c.name.clone()).unwrap_or_default();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag", "V1" }
                                    span { class: "mr-ctx-name", "{name}" }
                                }
                                CtxItem {
                                    label: "Split at playhead".to_string(),
                                    shortcut: Some("S".to_string()),
                                    on_action: move |_| split_at_playhead(()),
                                }
                                CtxItem {
                                    label: "Set in point at playhead".to_string(),
                                    shortcut: Some("I".to_string()),
                                    on_action: move |_| set_in_here(()),
                                }
                                CtxItem {
                                    label: "Set out point at playhead".to_string(),
                                    shortcut: Some("O".to_string()),
                                    on_action: move |_| set_out_here(()),
                                }
                                MenuSeparator {}
                                CtxItem {
                                    label: "Move clip left".to_string(),
                                    disabled: i == 0,
                                    on_action: move |_| move_sel(-1),
                                }
                                CtxItem {
                                    label: "Move clip right".to_string(),
                                    disabled: i + 1 >= len,
                                    on_action: move |_| move_sel(1),
                                }
                                MenuSeparator {}
                                {group_rows(clips.read().get(i).map_or(0, |c| c.group) == 0)}
                                MenuSeparator {}
                                CtxItem {
                                    label: "Ripple delete".to_string(),
                                    shortcut: Some("Delete".to_string()),
                                    danger: true,
                                    on_action: move |_| delete_sel(()),
                                }
                            }
                        }
                        Ctx::Over(j) => {
                            let name = overlays.read().get(j).map(|o| o.name.clone()).unwrap_or_default();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag", "V2" }
                                    span { class: "mr-ctx-name", "{name}" }
                                }
                                CtxItem {
                                    label: "Split at playhead".to_string(),
                                    shortcut: Some("S".to_string()),
                                    on_action: move |_| split_at_playhead(()),
                                }
                                {group_rows(overlays.read().get(j).map_or(0, |o| o.group) == 0)}
                                MenuSeparator {}
                                CtxItem {
                                    label: "Remove overlay".to_string(),
                                    danger: true,
                                    on_action: move |_| delete_sel(()),
                                }
                            }
                        }
                        Ctx::Aud(k) => {
                            let name = audios.read().get(k).map(|a| a.name.clone()).unwrap_or_default();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag audio", "A1" }
                                    span { class: "mr-ctx-name", "{name}" }
                                }
                                CtxItem {
                                    label: "Split at playhead".to_string(),
                                    shortcut: Some("S".to_string()),
                                    on_action: move |_| split_at_playhead(()),
                                }
                                {group_rows(audios.read().get(k).map_or(0, |a| a.group) == 0)}
                                MenuSeparator {}
                                CtxItem {
                                    label: "Remove audio".to_string(),
                                    danger: true,
                                    on_action: move |_| delete_sel(()),
                                }
                            }
                        }
                        Ctx::Title(k) => {
                            let text = titles.read().get(k).map(|t| t.text.clone()).unwrap_or_default();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag title", "T" }
                                    span { class: "mr-ctx-name", "{text}" }
                                }
                                {group_rows(titles.read().get(k).map_or(0, |t| t.group) == 0)}
                                MenuSeparator {}
                                CtxItem {
                                    label: "Remove title".to_string(),
                                    danger: true,
                                    on_action: move |_| delete_sel(()),
                                }
                            }
                        }
                    }}
                }
            }
        }

        Modal {
            open: show_shortcuts,
            title: "Keyboard shortcuts".to_string(),
            table { class: "mr-shortcut-table",
                for (keys, what) in [
                    ("Space", "Play / pause (proxy video + audio mix)"),
                    ("Ctrl+P", "Full preview with audio in mpv/ffplay"),
                    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                    ("I / O", "Set in / out point at playhead"),
                    ("S", "Split at playhead"),
                    ("Delete / Backspace", "Ripple delete selection"),
                    ("← / →", "Nudge playhead 0.1s (Shift = 1s)"),
                    ("[ / ]", "Select previous / next clip"),
                    ("Drag", "Move items; snaps to cuts and the playhead, V1 clips reorder"),
                    ("Drop files", "Drag media in from a file manager; the lane decides what it becomes"),
                    ("Ctrl+Click", "Mark items for grouping"),
                    ("Ctrl+G / Ctrl+Shift+G", "Group marked items / ungroup"),
                    ("~", "Toggle magnetic timeline (V2/A1/T ride V1 edits)"),
                    ("G", "Toggle safe-area guides (phone UI zones)"),
                    ("T", "Toggle on-screen transform handles"),
                    ("M / Shift+M", "Drop a beat marker at the playhead / clear them all"),
                    ("Home / End", "Jump to start / end"),
                    ("Ctrl+O", "Add clips"),
                    ("Ctrl+Shift+O / Ctrl+S", "Open / save project"),
                    ("Ctrl+T", "Add title at playhead"),
                    ("Ctrl+E", "Export MP4"),
                    ("Ctrl+Q", "Quit"),
                ] {
                    tr {
                        td { class: "mr-key", "{keys}" }
                        td { "{what}" }
                    }
                }
            }
        }
        Modal {
            open: show_save_preset,
            title: "Save title style".to_string(),
            div { class: "mr-export-dialog",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Keeps the font, size, colour, line-up, backdrop, outline, bevel and entrance — not the words or the timing. Saved outside the project, so other reels can use it."
                }
                mor_rust_dioxus_ui_kit::MorTextInput {
                    label: "Name".to_string(),
                    value: preset_name(),
                    onchange: move |v: String| preset_name.set(v),
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn",
                        onclick: move |_| { show_save_preset.set(false); preset_name.set(String::new()); },
                        "Cancel"
                    }
                    button {
                        class: "mor-btn primary",
                        disabled: preset_name().trim().is_empty(),
                        onclick: move |_| {
                            if let Some(Sel::Title(k)) = selected() {
                                store_preset(k);
                            }
                        },
                        "Save style"
                    }
                }
            }
        }
        Modal {
            open: show_export,
            title: "Export".to_string(),
            div { class: "mr-export-dialog",
                MorSelect {
                    label: "Format".to_string(),
                    value: export_opts().format.label().to_string(),
                    options: engine::Format::ALL.iter().map(|f| f.label().to_string()).collect::<Vec<_>>(),
                    onchange: move |v: String| {
                        let mut o = export_opts();
                        o.format = engine::Format::from_label(&v);
                        export_opts.set(o);
                    },
                }
                p { class: "mor-statusbar-muted mr-export-blurb", "{export_opts().format.blurb()}" }
                MorSelect {
                    label: "Quality".to_string(),
                    value: export_opts().quality.label().to_string(),
                    options: engine::Quality::ALL.iter().map(|q| q.label().to_string()).collect::<Vec<_>>(),
                    onchange: move |v: String| {
                        let mut o = export_opts();
                        o.quality = engine::Quality::from_label(&v);
                        export_opts.set(o);
                    },
                }
                MorSelect {
                    label: "Size".to_string(),
                    value: engine::size_label(export_opts().width),
                    options: engine::SIZES.iter().map(|(l, _, _)| l.to_string()).collect::<Vec<_>>(),
                    onchange: move |v: String| {
                        let w = engine::SIZES.iter().find(|(l, _, _)| *l == v).map_or(1080, |(_, w, _)| *w);
                        export_opts.set(export_opts().with_size(w));
                    },
                }
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "{fmt_t(total)} at 30 fps"
                    if !export_opts().format.has_audio() { " · silent — this format carries no audio" }
                    if let Some(warn) = over_limits(total) { " · {warn}" }
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn",
                        onclick: move |_| show_export.set(false),
                        "Cancel"
                    }
                    button {
                        class: "mor-btn primary mr-export",
                        onclick: move |_| run_export(()),
                        "⇪ Choose file and export"
                    }
                }
            }
        }
        Modal {
            open: show_about,
            title: "About MorReel Studio".to_string(),
            p { "Portrait-only (9:16) video editor for phone reels." }
            p { class: "mor-statusbar-muted",
                "Trim, reorder, split and grade clips on V1; cutaway overlays on V2; music under on A1. "
                "Everything renders through ffmpeg — what you scrub is what you ship, at 1080×1920, 30 fps."
            }
            p { class: "mor-statusbar-muted", "Built with Dioxus and the MOR UI Kit." }
        }
    }
}

const APP_CSS: &str = r#"
/* Fluid + a light retro MMO HUD: brass frames, soft glows, inventory-slot
   clips. Motion is deliberate; labels stay plain English in the app itself. */
.mr-root {
  display: flex; flex-direction: column; gap: 12px; height: 100%; min-height: 0;
  padding: 12px; box-sizing: border-box;
  background:
    radial-gradient(ellipse 90% 55% at 50% -8%, color-mix(in srgb, var(--mor-accent) 9%, transparent), transparent 58%),
    radial-gradient(ellipse 50% 40% at 100% 100%, color-mix(in srgb, var(--mor-success) 5%, transparent), transparent 50%),
    var(--mor-bg);
}
.mr-work { display: flex; gap: 16px; flex: 1; min-height: 0; }
.mr-preview-col { display: flex; flex-direction: column; gap: 10px; align-items: center; min-height: 0; padding-top: 4px; }

/* Buttons inside the editor: longer ease, slight lift, brass-rim primary. */
.mr-root .mor-btn {
  transition: background-color 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease, transform 0.15s ease, color 0.15s ease;
}
.mr-root .mor-btn:hover:not(:disabled) {
  transform: translateY(-1px);
  box-shadow: 0 3px 10px rgba(0, 0, 0, 0.35), 0 0 0 1px color-mix(in srgb, var(--mor-border-light) 60%, transparent);
}
.mr-root .mor-btn:active:not(:disabled) { transform: translateY(0); }
.mr-root .mor-btn.primary {
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-accent-hover) 92%, white), var(--mor-accent));
  border-color: color-mix(in srgb, var(--mor-accent) 70%, #5a4010);
  color: #1a1408;
  font-weight: 600;
  text-shadow: 0 1px 0 color-mix(in srgb, white 25%, transparent);
}
.mr-root .mor-btn.primary:hover:not(:disabled) {
  background: linear-gradient(180deg, var(--mor-accent-hover), color-mix(in srgb, var(--mor-accent) 85%, black));
  box-shadow: 0 4px 14px color-mix(in srgb, var(--mor-accent) 35%, transparent);
}

/* Signature: phone as a framed artifact — dual rim + soft brass bloom.
   Width-driven: aspect-ratio height doesn't feed the flex column's intrinsic
   width in WebKit, so a flex-sized phone overflows. 400px ≈ vertical chrome. */
.mr-phone {
  position: relative; flex: none;
  width: calc((100vh - 400px) * 9 / 16); min-width: 140px; max-height: 100%;
  aspect-ratio: 9 / 16; background: #000;
  border: 5px solid #06060a;
  border-radius: 24px; overflow: hidden;
  display: flex; align-items: center; justify-content: center;
  color: var(--mor-text-muted); font-size: 13px;
  box-shadow:
    0 0 0 1px color-mix(in srgb, var(--mor-accent) 45%, var(--mor-border-light)),
    0 0 0 3px #0a0a10,
    0 16px 44px rgba(0, 0, 0, 0.6),
    0 0 48px color-mix(in srgb, var(--mor-accent) 14%, transparent);
  transition: box-shadow 0.35s ease, transform 0.25s ease;
}
.mr-phone:hover {
  box-shadow:
    0 0 0 1px color-mix(in srgb, var(--mor-accent) 65%, var(--mor-border-light)),
    0 0 0 3px #0a0a10,
    0 18px 48px rgba(0, 0, 0, 0.62),
    0 0 64px color-mix(in srgb, var(--mor-accent) 22%, transparent);
}
.mr-phone > span { text-align: center; padding: 0 16px; }
.mr-phone img { width: 100%; height: 100%; object-fit: cover; display: block; }
/* Punch-hole speaker slit: the bezel reads as a phone at a glance. */
.mr-phone::after {
  content: ""; position: absolute; top: 6px; left: 50%; transform: translateX(-50%);
  width: 22%; height: 5px; border-radius: 3px; background: #06060a;
  box-shadow: inset 0 1px 1px rgba(255, 255, 255, 0.06), 0 0 6px color-mix(in srgb, var(--mor-accent) 20%, transparent);
  z-index: 2; pointer-events: none;
}

/* Drop target: pulse the brass rim without reflowing lane geometry. */
.mr-drop {
  box-shadow:
    inset 0 0 0 2px var(--mor-accent),
    0 0 18px color-mix(in srgb, var(--mor-accent) 38%, transparent) !important;
  background-color: color-mix(in srgb, var(--mor-accent) 12%, transparent);
  animation: mr-drop-pulse 1.1s ease-in-out infinite;
}
.mr-timeline.mr-drop { border-radius: var(--mor-radius); }
@keyframes mr-drop-pulse {
  0%, 100% { box-shadow: inset 0 0 0 2px var(--mor-accent), 0 0 12px color-mix(in srgb, var(--mor-accent) 28%, transparent); }
  50% { box-shadow: inset 0 0 0 2px var(--mor-accent-hover), 0 0 22px color-mix(in srgb, var(--mor-accent) 48%, transparent); }
}

/* Transform handles: gem-square corners, gold dashed frame. */
.mr-xf { position: absolute; inset: 0; z-index: 4; }
.mr-xf-box {
  position: absolute; box-sizing: border-box;
  border: 1px dashed var(--mor-accent);
  background: color-mix(in srgb, var(--mor-accent) 7%, transparent);
  pointer-events: auto; cursor: move;
  box-shadow: 0 0 12px color-mix(in srgb, var(--mor-accent) 18%, transparent);
}
.mr-xf-h {
  position: absolute; width: 13px; height: 13px; margin: -7px 0 0 -7px; box-sizing: border-box;
  background: linear-gradient(145deg, var(--mor-accent-hover), var(--mor-accent));
  border: 1px solid #0f0f12; border-radius: 2px;
  pointer-events: auto; cursor: nwse-resize;
  box-shadow: 0 1px 3px rgba(0,0,0,0.6), 0 0 6px color-mix(in srgb, var(--mor-accent) 40%, transparent);
  transition: transform 0.12s ease;
}
.mr-xf-h:hover { transform: scale(1.15); }
.mr-xf-e { position: absolute; box-sizing: border-box; background: var(--mor-accent); border: 1px solid #0f0f12; border-radius: 2px; pointer-events: auto; box-shadow: 0 1px 3px rgba(0,0,0,0.6); }
.mr-xf-e.wide { width: 9px; height: 17px; margin: -9px 0 0 -5px; cursor: ew-resize; }
.mr-xf-e.tall { width: 17px; height: 9px; margin: -5px 0 0 -9px; cursor: ns-resize; }
.mr-xf-rot {
  position: absolute; width: 13px; height: 13px; margin: -22px 0 0 -7px; box-sizing: border-box;
  background: radial-gradient(circle at 35% 30%, #fff6c8, var(--mor-warning) 55%, #a07820);
  border: 1px solid #0f0f12; border-radius: 50%;
  pointer-events: auto; cursor: grab;
  box-shadow: 0 1px 3px rgba(0,0,0,0.6), 0 0 8px color-mix(in srgb, var(--mor-warning) 45%, transparent);
}
.mr-xf-rot::after {
  content: ""; position: absolute; left: 50%; top: 100%; width: 1px; height: 14px;
  background: var(--mor-warning); opacity: 0.75;
}

/* Safe-area guides (phone app chrome). Non-interactive. */
.mr-safe { position: absolute; inset: 0; z-index: 3; pointer-events: none; }
.mr-safe-zone {
  position: absolute;
  background: color-mix(in srgb, var(--mor-destructive) 15%, transparent);
  border: 1px dashed color-mix(in srgb, var(--mor-destructive) 55%, transparent);
  box-sizing: border-box;
}
.mr-safe-zone span {
  position: absolute; bottom: 2px; right: 4px; font-size: 8px; letter-spacing: 0.04em;
  text-transform: uppercase;
  color: color-mix(in srgb, var(--mor-destructive) 85%, white);
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.9); white-space: nowrap;
}
.mr-safe-top { top: 0; left: 0; right: 0; height: 8%; border-width: 0 0 1px 0; }
.mr-safe-bottom { bottom: 0; left: 0; right: 0; height: 24%; border-width: 1px 0 0 0; }
.mr-safe-bottom span { bottom: auto; top: 2px; }
.mr-safe-rail { top: 8%; bottom: 24%; right: 0; width: 18%; border-width: 0 0 0 1px; }

.mr-monitor {
  height: 100vh; display: flex; align-items: center; justify-content: center;
  padding: 14px; box-sizing: border-box;
  background:
    radial-gradient(ellipse 70% 60% at 50% 40%, color-mix(in srgb, var(--mor-accent) 8%, transparent), transparent 65%),
    var(--mor-bg);
}
.mr-monitor .mr-phone { width: auto; height: 100%; max-width: 100%; min-width: 0; }

.mr-scrub { width: 100%; }
/* Deck: HUD readout — brass inset frame, amber at rest, record-red rolling. */
.mr-deck {
  display: flex; justify-content: center; align-items: baseline; gap: 7px;
  margin-bottom: 4px; padding: 5px 14px 6px;
  background: linear-gradient(180deg, #12121a, #08080c);
  border: 1px solid color-mix(in srgb, var(--mor-accent) 35%, var(--mor-border));
  border-radius: 8px;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 18%, transparent),
    inset 0 -1px 0 rgba(0, 0, 0, 0.45),
    0 0 16px color-mix(in srgb, var(--mor-accent) 12%, transparent);
  font-size: 21px; color: var(--mor-accent); letter-spacing: 0.06em;
  text-shadow: 0 0 12px color-mix(in srgb, var(--mor-accent) 45%, transparent);
  transition: color 0.25s ease, text-shadow 0.25s ease, border-color 0.25s ease, box-shadow 0.25s ease;
}
.mr-deck.playing {
  color: var(--mor-destructive);
  border-color: color-mix(in srgb, var(--mor-destructive) 45%, var(--mor-border));
  text-shadow: 0 0 14px color-mix(in srgb, var(--mor-destructive) 50%, transparent);
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-destructive) 20%, transparent),
    0 0 18px color-mix(in srgb, var(--mor-destructive) 18%, transparent);
  animation: mr-deck-pulse 1.4s ease-in-out infinite;
}
@keyframes mr-deck-pulse {
  0%, 100% { text-shadow: 0 0 10px color-mix(in srgb, var(--mor-destructive) 40%, transparent); }
  50% { text-shadow: 0 0 18px color-mix(in srgb, var(--mor-destructive) 65%, transparent); }
}
.mr-deck-total { font-size: 12px; color: var(--mor-text-muted); text-shadow: none; letter-spacing: 0.03em; }
.mr-play-row { display: flex; gap: 8px; justify-content: center; margin-top: 8px; }

/* Inspector: framed panel like a classic side window. */
.mr-inspector {
  flex: 1; min-width: 280px; display: flex; flex-direction: column; gap: 12px;
  background:
    linear-gradient(180deg, color-mix(in srgb, var(--mor-panel) 92%, white), var(--mor-panel));
  border: 1px solid color-mix(in srgb, var(--mor-accent) 22%, var(--mor-border));
  border-radius: var(--mor-radius);
  padding: 14px; overflow-y: auto;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 12%, transparent),
    0 8px 28px rgba(0, 0, 0, 0.35),
    0 0 0 1px rgba(0, 0, 0, 0.35);
}
.mr-toolbar { display: flex; gap: 8px; flex-wrap: wrap; }
.mr-toolbar .mr-export {
  margin-left: auto; color: var(--mor-warning);
  border-color: color-mix(in srgb, var(--mor-warning) 50%, transparent);
  background: color-mix(in srgb, var(--mor-warning) 8%, var(--mor-btn));
}
.mr-toolbar .mr-export:hover:not(:disabled) {
  border-color: var(--mor-warning);
  box-shadow: 0 0 12px color-mix(in srgb, var(--mor-warning) 30%, transparent);
  color: #fff4c8;
}
.mr-clip-info h3 { margin: 0 0 4px 0; font-size: 14px; overflow-wrap: anywhere; }
.mr-clip-info .mr-ctx-tag { vertical-align: 2px; }
.mr-clip-info p { margin: 0; font-size: 12px; }
.mr-danger { color: var(--mor-destructive); }
.mr-reset { align-self: flex-start; font-size: 11px; }
.mr-keys { margin-top: auto; font-size: 11px; }

.mr-progress {
  height: 7px; background: var(--mor-border); border-radius: 4px; overflow: hidden;
  box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.4);
}
.mr-progress > div {
  height: 100%;
  background: linear-gradient(90deg, var(--mor-accent), var(--mor-accent-hover), var(--mor-warning));
  background-size: 200% 100%;
  transition: width 0.3s ease;
  animation: mr-progress-sheen 2.2s linear infinite;
  box-shadow: 0 0 8px color-mix(in srgb, var(--mor-accent) 40%, transparent);
}
@keyframes mr-progress-sheen {
  0% { background-position: 100% 0; }
  100% { background-position: -100% 0; }
}

/* Timeline: darkest bench, brass outer rim. */
.mr-timeline {
  display: flex; overflow: scroll; padding: 12px 10px 8px;
  background:
    linear-gradient(180deg, color-mix(in srgb, var(--mor-header) 88%, var(--mor-accent)), var(--mor-header));
  border: 1px solid color-mix(in srgb, var(--mor-accent) 18%, var(--mor-border));
  border-radius: var(--mor-radius);
  min-height: 216px; max-height: 40vh;
  align-items: flex-start; flex: none;
  user-select: none; -webkit-user-select: none;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 10%, transparent),
    0 6px 22px rgba(0, 0, 0, 0.4);
}
.mr-timeline-hint { align-self: center; margin: auto; }
.mr-track { position: relative; flex: none; min-width: 100%; }

.mr-tick, .mr-clip-dur, .mr-key, .mr-ph-badge, .mr-deck {
  font-family: ui-monospace, 'Cascadia Mono', 'DejaVu Sans Mono', monospace;
  font-variant-numeric: tabular-nums;
}

.mr-ruler {
  position: relative; height: 22px; margin-bottom: 6px;
  border-bottom: 1px solid color-mix(in srgb, var(--mor-accent) 25%, var(--mor-border-light));
  cursor: ew-resize;
}
.mr-tick {
  position: absolute; bottom: 0; height: 5px;
  border-left: 1px solid var(--mor-border);
  font-size: 9px; color: var(--mor-text-muted);
  pointer-events: none; white-space: nowrap;
}
.mr-tick.major {
  height: 15px;
  border-left-color: color-mix(in srgb, var(--mor-accent) 40%, var(--mor-border-light));
  padding-left: 3px; color: color-mix(in srgb, var(--mor-text-muted) 80%, var(--mor-accent));
}

.mr-lane {
  position: relative; height: 30px; margin-bottom: 6px;
  background: rgba(127, 127, 127, 0.06);
  border-radius: 5px;
  box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.25);
}
.mr-lane-tag {
  position: absolute; top: 4px; left: 4px; z-index: 2;
  font-size: 9px; font-weight: 700; padding: 1px 6px; border-radius: 3px;
  background: linear-gradient(180deg, var(--mor-accent-hover), var(--mor-accent));
  color: #141417; pointer-events: none;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.45);
}
.mr-lane-tag.title { background: linear-gradient(180deg, #f0d078, var(--mor-warning)); }
.mr-lane-a1 .mr-lane-tag { background: linear-gradient(180deg, #5ee8dc, var(--mor-success)); }
.mr-lane-a1 { height: 44px; }

/* Lane items: embossed inventory-slot feel. */
.mr-lane-item {
  position: absolute; top: 2px; bottom: 2px; box-sizing: border-box;
  overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
  font-size: 10px; line-height: 22px; padding: 0 6px 0 30px;
  border-radius: 5px;
  border: 1px solid color-mix(in srgb, var(--mor-accent) 50%, transparent);
  background:
    linear-gradient(180deg, color-mix(in srgb, var(--mor-accent) 32%, transparent), color-mix(in srgb, var(--mor-accent) 16%, transparent));
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 12%, transparent), 0 1px 2px rgba(0, 0, 0, 0.3);
  cursor: grab;
  transition: border-color 0.15s ease, box-shadow 0.18s ease, filter 0.15s ease;
}
.mr-lane-item:hover { filter: brightness(1.08); }
.mr-lane-item.audio {
  border-color: color-mix(in srgb, var(--mor-success) 50%, transparent);
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-success) 30%, transparent), color-mix(in srgb, var(--mor-success) 14%, transparent));
}
.mr-lane-item.title {
  border-color: color-mix(in srgb, var(--mor-warning) 55%, transparent);
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-warning) 34%, transparent), color-mix(in srgb, var(--mor-warning) 16%, transparent));
}
.mr-lane-item.selected {
  border-color: var(--mor-accent);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 15%, transparent), 0 0 12px color-mix(in srgb, var(--mor-accent) 40%, transparent);
}
.mr-lane-item.audio.selected {
  border-color: var(--mor-success);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 15%, transparent), 0 0 12px color-mix(in srgb, var(--mor-success) 40%, transparent);
}
.mr-lane-item.title.selected {
  border-color: var(--mor-warning);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 15%, transparent), 0 0 12px color-mix(in srgb, var(--mor-warning) 40%, transparent);
}

.mr-lane-item.marked, .mr-clip.marked {
  outline: 2px dashed var(--mor-accent-hover); outline-offset: 1px;
}
.mr-group-dot {
  position: absolute; left: 3px; bottom: 3px; z-index: 2;
  width: 7px; height: 7px; border-radius: 50%;
  box-shadow: 0 0 4px rgba(0, 0, 0, 0.7), 0 0 6px color-mix(in srgb, currentColor 40%, transparent);
  pointer-events: none;
}

/* V1 story lane: faint gold bed; clips as raised tiles. */
.mr-clips {
  position: relative; display: flex; margin-bottom: 6px;
  background: color-mix(in srgb, var(--mor-accent) 6%, transparent);
  border-radius: 7px;
  box-shadow: inset 0 1px 3px rgba(0, 0, 0, 0.28);
}
.mr-clip {
  position: relative; flex: none; box-sizing: border-box; overflow: hidden;
  cursor: grab; border: 2px solid transparent; border-radius: 7px; padding: 3px;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-panel) 90%, white), var(--mor-panel));
  display: flex; flex-direction: column; gap: 2px;
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 8%, transparent), 0 1px 3px rgba(0, 0, 0, 0.28);
  transition: border-color 0.16s ease, box-shadow 0.2s ease, transform 0.15s ease, filter 0.15s ease;
}
.mr-clip:hover {
  border-color: var(--mor-border-light);
  filter: brightness(1.05);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 10%, transparent), 0 2px 8px rgba(0, 0, 0, 0.35);
}
.mr-clip.selected {
  border-color: var(--mor-accent);
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, white 12%, transparent),
    0 0 14px color-mix(in srgb, var(--mor-accent) 38%, transparent);
}
.mr-clip img, .mr-thumb-missing {
  width: 100%; height: 72px; object-fit: cover; border-radius: 4px;
  display: block; background: #000;
}
.mr-clip-wave {
  height: 18px; flex: none; border-radius: 3px;
  background-color: color-mix(in srgb, var(--mor-success) 10%, transparent);
}
.mr-xtrans {
  position: absolute; top: 3px; left: 3px; z-index: 2;
  font-size: 8px; line-height: 12px; padding: 0 4px; border-radius: 3px;
  background: linear-gradient(180deg, var(--mor-accent-hover), var(--mor-accent));
  color: #141417; letter-spacing: -1px; pointer-events: none;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
}
.mr-clip-name { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 10px; }
.mr-clip-dur { font-size: 10px; color: var(--mor-text-muted); }

.mr-marker {
  position: absolute; top: 18px; bottom: 0; width: 1px;
  background: color-mix(in srgb, var(--mor-warning) 70%, transparent);
  pointer-events: none; z-index: 1;
}
.mr-marker::before {
  content: ""; position: absolute; top: -6px; left: -3px;
  border: 3px solid transparent; border-top: 5px solid var(--mor-warning);
  filter: drop-shadow(0 0 3px color-mix(in srgb, var(--mor-warning) 50%, transparent));
}

.mr-playhead {
  position: absolute; top: 0; bottom: 0; width: 2px;
  background: var(--mor-destructive);
  box-shadow: 0 0 8px color-mix(in srgb, var(--mor-destructive) 65%, transparent);
  pointer-events: none;
}
/* Diamond cap — a tiny retro-UI gem on the playhead. */
.mr-playhead::before {
  content: ""; position: absolute; top: 1px; left: -4px;
  width: 10px; height: 10px;
  background: linear-gradient(145deg, #ff8a8e, var(--mor-destructive) 50%, #9a2024);
  border: 1px solid color-mix(in srgb, white 25%, var(--mor-destructive));
  border-radius: 1px;
  transform: rotate(45deg);
  box-shadow: 0 0 6px color-mix(in srgb, var(--mor-destructive) 55%, transparent);
}
.mr-ph-badge {
  position: absolute; top: 0; left: 10px; padding: 0 5px; border-radius: 3px;
  background: linear-gradient(180deg, #f06a6e, var(--mor-destructive));
  color: #fff; font-size: 9px; line-height: 14px; white-space: nowrap;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.45);
}

.mr-ctx-backdrop { position: fixed; inset: 0; z-index: 400; }
.mr-ctx {
  display: block; position: fixed; margin: 0; width: 228px; z-index: 401;
  border: 1px solid color-mix(in srgb, var(--mor-accent) 28%, var(--mor-border)) !important;
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(0, 0, 0, 0.3) !important;
}
.mr-ctx-head {
  display: flex; align-items: center; gap: 6px;
  padding: 4px 10px 7px;
  border-bottom: 1px solid color-mix(in srgb, var(--mor-accent) 20%, var(--mor-border-light));
  margin-bottom: 4px;
}
.mr-ctx-tag {
  flex: none; font-size: 9px; font-weight: 700; padding: 1px 6px; border-radius: 3px;
  background: linear-gradient(180deg, var(--mor-accent-hover), var(--mor-accent));
  color: #141417;
}
.mr-ctx-tag.audio { background: linear-gradient(180deg, #5ee8dc, var(--mor-success)); }
.mr-ctx-tag.title { background: linear-gradient(180deg, #f0d078, var(--mor-warning)); }
.mr-ctx-name {
  font-size: 11px; color: var(--mor-text-muted);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.mr-ctx .mor-menu-action.mr-danger { color: var(--mor-destructive); }
.mr-ctx .mor-menu-action.mr-danger:hover:not(:disabled) {
  background-color: var(--mor-destructive); color: #fff;
}

.mr-zoom { display: inline-flex; gap: 4px; align-items: center; }
.mr-zoom button {
  background: none; border: none; color: var(--mor-text-muted);
  font-size: 14px; line-height: 1; padding: 0 2px; cursor: pointer;
  transition: color 0.15s ease, transform 0.12s ease;
}
.mr-zoom button:hover { color: var(--mor-accent-hover); transform: scale(1.12); }
.mr-zoom-slider { width: 90px; accent-color: var(--mor-accent); }

.mr-tabs {
  display: flex; gap: 2px;
  border-bottom: 1px solid color-mix(in srgb, var(--mor-accent) 18%, var(--mor-border));
}
.mr-tab {
  background: none; border: none; border-bottom: 2px solid transparent;
  color: var(--mor-text-muted); font-size: 12px; padding: 5px 14px; cursor: pointer;
  transition: color 0.15s ease, border-color 0.18s ease, text-shadow 0.18s ease;
}
.mr-tab:hover { color: var(--mor-text); }
.mr-tab.active {
  color: var(--mor-accent-hover);
  border-bottom-color: var(--mor-accent);
  text-shadow: 0 0 10px color-mix(in srgb, var(--mor-accent) 35%, transparent);
}

.mr-fx-cat {
  margin: 4px 0 0; font-size: 10px; font-weight: 700;
  letter-spacing: 0.1em; text-transform: uppercase;
  color: color-mix(in srgb, var(--mor-text-muted) 85%, var(--mor-accent));
}
.mr-fx-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(72px, 1fr)); gap: 8px; }
.mr-fx-tile {
  padding: 3px; border: 2px solid transparent; border-radius: 7px;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-btn) 90%, white), var(--mor-btn));
  cursor: pointer; display: flex; flex-direction: column; gap: 2px; align-items: center;
  color: var(--mor-text); font-size: 10px;
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 6%, transparent);
  transition: border-color 0.15s ease, box-shadow 0.18s ease, transform 0.15s ease, filter 0.15s ease;
}
.mr-fx-tile:hover {
  border-color: var(--mor-border-light);
  transform: translateY(-2px);
  filter: brightness(1.06);
}
.mr-fx-tile.active {
  border-color: var(--mor-accent);
  box-shadow: 0 0 12px color-mix(in srgb, var(--mor-accent) 35%, transparent);
}
.mr-fx-tile img, .mr-fx-ph {
  width: 100%; aspect-ratio: 9 / 16; object-fit: cover; border-radius: 4px;
  background: #000; display: block;
}
.mr-fx-tile span { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.mr-export-dialog { display: flex; flex-direction: column; gap: 10px; min-width: 320px; }
.mr-export-blurb { margin: -4px 0 2px; font-size: 12px; }
.mr-export-dialog .mr-toolbar { justify-content: flex-end; margin-top: 4px; }

.mr-shortcut-table { border-collapse: collapse; width: 100%; font-size: 13px; }
.mr-shortcut-table td { padding: 4px 10px 4px 0; }
.mr-key { color: var(--mor-accent-hover); white-space: nowrap; }

@media (max-width: 700px) {
  .mr-work { flex-direction: column; }
  .mr-phone { flex: none; width: auto; height: 45vh; }
  .mr-inspector { min-width: 0; }
}

.mr-root button:focus-visible,
.mr-root input:focus-visible,
.mr-fx-tile:focus-visible,
.mr-tab:focus-visible {
  outline: 2px solid var(--mor-accent-hover);
  outline-offset: 2px;
}

@media (prefers-reduced-motion: reduce) {
  .mr-clip, .mr-lane-item, .mr-fx-tile, .mr-root .mor-btn,
  .mr-progress > div, .mr-deck, .mr-tab, .mr-phone, .mr-xf-h {
    transition: none !important; animation: none !important;
  }
  .mr-drop, .mr-deck.playing, .mr-progress > div { animation: none !important; }
  .mr-root .mor-btn:hover:not(:disabled),
  .mr-fx-tile:hover { transform: none; }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(in_s: f64, out_s: f64) -> Clip {
        Clip {
            path: String::new(),
            name: String::new(),
            duration: out_s,
            in_s,
            out_s,
            has_audio: true,
            effect: "None".to_string(),
            effect_amount: 1.0,
            framing: "Crop".to_string(),
            transform: engine::Transform::default(),
            speed: 1.0,
            volume: 1.0,
            transition: "None".to_string(),
            trans_dur: 0.5,
            thumb: String::new(),
            wave: String::new(),
            proxy: String::new(),
            group: 0,
        }
    }

    /// `a` seconds, then `b` seconds joined by a `d`-second dissolve.
    fn dissolved(a: f64, b: f64, d: f64) -> Vec<Clip> {
        let mut second = clip(0.0, b);
        second.transition = "Cross dissolve".to_string();
        second.trans_dur = d;
        vec![clip(0.0, a), second]
    }

    #[test]
    fn a_transition_takes_its_length_off_the_timeline() {
        // Cuts: each clip owns everything it runs for.
        let cut = [clip(0.0, 5.0), clip(0.0, 4.0)];
        assert_eq!(extents(&cut), vec![5.0, 4.0]);

        // A 1 s dissolve overlaps the join, so the outgoing clip owns 1 s less
        // and the reel is 1 s shorter. The extents still tile the timeline.
        let fade = dissolved(5.0, 4.0, 1.0);
        assert_eq!(extents(&fade), vec![4.0, 4.0]);
        assert_eq!(extents(&fade).iter().sum::<f64>(), 8.0);

        // The playhead maps through the shortened timeline: the second clip now
        // starts at 4 s, and 4 s in is its first frame.
        assert_eq!(locate(&fade, 3.9), Some((0, 3.9)));
        assert_eq!(locate(&fade, 4.0), Some((1, 0.0)));
        assert_eq!(locate(&fade, 6.0), Some((1, 2.0)));

        // "None" is a cut no matter what length is stored against it.
        let mut none = dissolved(5.0, 4.0, 1.0);
        none[1].transition = "None".to_string();
        assert_eq!(extents(&none), vec![5.0, 4.0]);

        // Nothing precedes the first clip, so its transition is inert.
        let mut lead = vec![clip(0.0, 5.0), clip(0.0, 4.0)];
        lead[0].transition = "Cross dissolve".to_string();
        lead[0].trans_dur = 1.0;
        assert_eq!(extents(&lead), vec![5.0, 4.0]);
        assert_eq!(fade_in(&lead, 0), 0.0);
    }

    #[test]
    fn a_transition_longer_than_its_clips_is_clamped() {
        // Left alone this would give a negative extent, and xfade would be
        // handed an offset before the start of the stream.
        let greedy = dissolved(1.0, 1.0, 30.0);
        assert!(fade_in(&greedy, 1) < 1.0, "not clamped: {}", fade_in(&greedy, 1));
        assert!(extents(&greedy).iter().all(|d| *d > 0.0), "{:?}", extents(&greedy));
        assert!(extents(&greedy).iter().sum::<f64>() > 0.0);
    }

    #[test]
    fn transition_at_reports_where_the_blend_has_got_to() {
        // 5 s then 4 s with a 1 s dissolve: clip 0 owns 0..4, and the overlap
        // runs across 3..4 — the last second of what clip 0 owns.
        let fade = dissolved(5.0, 4.0, 1.0);
        assert_eq!(transition_at(&fade, 2.0), None, "before the overlap");
        let (idx, p, src) = transition_at(&fade, 3.0).unwrap();
        assert_eq!(idx, 1, "the blend brings in the second clip");
        assert!(p.abs() < 1e-9, "it has just started: {p}");
        assert!(src.abs() < 1e-9, "from the incoming clip's first frame: {src}");

        let (_, p, src) = transition_at(&fade, 3.5).unwrap();
        assert!((p - 0.5).abs() < 1e-9, "halfway: {p}");
        assert!((src - 0.5).abs() < 1e-9, "half a second into the incoming clip: {src}");

        // Past the overlap the incoming clip is simply the clip under the head.
        assert_eq!(transition_at(&fade, 4.5), None);
        // A cut has no overlap to be inside of.
        assert_eq!(transition_at(&[clip(0.0, 5.0), clip(0.0, 4.0)], 4.9), None);
    }

    #[test]
    fn drops_land_between_clips_once_a_transition_has_moved_them() {
        // The second clip starts at 4 s, not 5 s, so the midpoint that decides
        // "before or after" has moved with it.
        let fade = dissolved(5.0, 4.0, 1.0);
        assert_eq!(insert_index(&fade, 1.0), 0);
        assert_eq!(insert_index(&fade, 3.0), 1); // past clip 0's midpoint of 2 s
        assert_eq!(insert_index(&fade, 7.0), 2); // past clip 1's midpoint of 6 s
    }

    #[test]
    fn locate_maps_global_time_to_clip_and_source_time() {
        // clip 0 keeps 1.0..3.0 (2s), clip 1 keeps 0.0..5.0 (5s) → total 7s
        let clips = [clip(1.0, 3.0), clip(0.0, 5.0)];
        assert_eq!(locate(&clips, 0.0), Some((0, 1.0)));
        assert_eq!(locate(&clips, 1.5), Some((0, 2.5)));
        assert_eq!(locate(&clips, 2.0), Some((1, 0.0))); // boundary lands on next clip
        assert_eq!(locate(&clips, 6.0), Some((1, 4.0)));
        assert_eq!(locate(&clips, 99.0), Some((1, 5.0))); // past the end clamps to last frame
        assert_eq!(locate(&[], 0.0), None);
    }

    #[test]
    fn cut_local_requires_min_on_both_sides() {
        assert_eq!(cut_local(1.0, 5.0, 3.0, 0.1), Some(3.0));
        assert_eq!(cut_local(1.0, 5.0, 1.05, 0.1), None); // left sliver
        assert_eq!(cut_local(1.0, 5.0, 4.95, 0.1), None); // right sliver
        assert_eq!(cut_local(1.0, 5.0, 1.1, 0.1), Some(1.1)); // exactly min is fine
        assert_eq!(cut_local(1.0, 5.0, 0.0, 0.1), None); // before span (overlay math)
        assert_eq!(cut_local(1.0, 5.0, 9.0, 0.1), None); // after span
    }

    #[test]
    fn caption_wrap_is_greedy_and_word_safe() {
        assert_eq!(wrap_caption("one two three", 8), "one two\nthree");
        assert_eq!(wrap_caption("short", 26), "short");
        // a word longer than max gets its own line, never split
        assert_eq!(wrap_caption("hi extraordinarily so", 6), "hi\nextraordinarily\nso");
    }

    #[test]
    fn magnet_delta_rides_v1_edits() {
        // two clips on the timeline: [0,2) and [2,5)
        let old = [(0.0, 2.0), (2.0, 5.0)];
        // swap: clip 0 now starts at 3.0, clip 1 at 0.0
        let swapped = |k: usize| Some(if k == 0 { 3.0 } else { 0.0 });
        assert_eq!(magnet_delta(1.0, &old, swapped), 3.0); // rider on clip 0
        assert_eq!(magnet_delta(2.5, &old, swapped), -2.0); // rider on clip 1
        assert_eq!(magnet_delta(7.0, &old, swapped), 0.0); // past V1: unattached
        // delete clip 0 (2s): clip 1 slides to 0; clip 0's riders hold position
        let deleted = |k: usize| (k != 0).then_some(0.0);
        assert_eq!(magnet_delta(1.0, &old, deleted), 0.0);
        assert_eq!(magnet_delta(3.0, &old, deleted), -2.0);
        // out-trim clip 0 to [0,1.5): clip 1 slides left 0.5, clip 0 riders stay
        let trimmed = |k: usize| Some(if k == 0 { 0.0 } else { 1.5 });
        assert_eq!(magnet_delta(1.0, &old, trimmed), 0.0);
        assert_eq!(magnet_delta(2.0, &old, trimmed), -0.5);
    }

    #[test]
    fn every_effect_has_a_filter_or_is_none() {
        assert_eq!(effect_filter("None"), "");
        assert_eq!(effect_filter("nonsense"), "");
        for (_, name, filter) in EFFECTS.iter().skip(1) {
            assert!(!filter.is_empty(), "effect {name} has no filter");
        }
        // moranima camera-move ports are present
        for port in ["Pulse zoom", "Drift", "Sway"] {
            assert!(!effect_filter(port).is_empty(), "missing moranima port {port}");
        }
    }

    // A Motion look that renders the same at every playhead position is a look
    // you cannot see before you export. Each one has to move with the clock.
    #[tokio::test]
    async fn every_motion_effect_previews_differently_over_time() {
        let dir = std::env::temp_dir().join("morreel-motion-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("still.png").display().to_string();
        // A still, so the only thing that can change between frames is the
        // effect itself.
        let out = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi"])
            .args(["-i", "testsrc=duration=1:size=400x300:rate=1", "-frames:v", "1", &png])
            .output()
            .unwrap();
        assert!(out.status.success());

        for (cat, name, _) in EFFECTS.iter().filter(|(c, _, _)| *c == "Motion") {
            let look = effect_filter_amt(name, 1.0);
            let at = |t: f64| {
                let look = look.clone();
                let png = png.clone();
                async move {
                    engine::frame_data_uri(&png, t, 108, 192, "Crop", &look, engine::Over::default()).await.unwrap()
                }
            };
            assert_ne!(
                at(0.0).await,
                at(2.0).await,
                "{cat}/{name} renders identically at 0s and 2s — it never previews"
            );
        }
    }

    #[test]
    fn effect_strength_interpolates() {
        // endpoints: full strength is byte-identical to the table, zero is off
        for (_, name, filter) in EFFECTS {
            assert_eq!(effect_filter_amt(name, 1.0), *filter, "{name} at full");
            assert_eq!(effect_filter_amt(name, 0.0), "", "{name} at zero");
        }
        assert_eq!(effect_filter_amt("None", 0.5), "");
        assert_eq!(effect_filter_amt("nonsense", 0.5), "");
        // midpoints move toward identity, not a weaker copy of the string
        assert_eq!(effect_filter_amt("B&W", 0.5), "hue=s=0.500");
        assert_eq!(effect_filter_amt("Warm", 0.5), "colortemperature=5500");
        assert!(effect_filter_amt("Sway", 0.5).contains("rotate=0.0175"));
        assert!(effect_filter_amt("Drift", 0.5).contains("65+27.0*sin"));
        // every mid-strength filter stays non-empty and export-shaped
        for (_, name, _) in EFFECTS.iter().skip(1) {
            assert!(!effect_filter_amt(name, 0.5).is_empty(), "{name} at half");
        }
    }

    #[test]
    fn speed_retimes_the_timeline() {
        let mut c = clip(0.0, 4.0);
        assert_eq!(c.trimmed(), 4.0);
        c.speed = 2.0;
        assert_eq!(c.trimmed(), 2.0); // 4 s of source in 2 s of reel
        c.speed = 0.5;
        assert_eq!(c.trimmed(), 8.0); // slow motion stretches it

        // Timeline time maps back to source time through the rate: halfway
        // through a 2x clip is halfway through its source.
        let clips = [{
            let mut c = clip(0.0, 4.0);
            c.speed = 2.0;
            c
        }];
        assert_eq!(locate(&clips, 0.0), Some((0, 0.0)));
        assert_eq!(locate(&clips, 1.0), Some((0, 2.0)));
        assert_eq!(locate(&clips, 2.0), Some((0, 4.0))); // clamped at the out point
    }

    #[test]
    fn waveform_window_tracks_trim_and_speed() {
        // 10 s source at 20 px/s: the image spans the whole source and slides
        // left by the in point, so the visible slice is the kept span.
        let css = wave_css("data:x", 10.0, 2.0, 20.0, 1.0);
        assert!(css.contains("background-size:200.0px 100%"), "{css}");
        assert!(css.contains("background-position:-40.0px 0"), "{css}");

        // At 2x the clip occupies half the width, so the waveform compresses
        // with it instead of drifting out of sync with the audio.
        let css = wave_css("data:x", 10.0, 2.0, 20.0, 2.0);
        assert!(css.contains("background-size:100.0px 100%"), "{css}");
        assert!(css.contains("background-position:-20.0px 0"), "{css}");

        // Nothing rendered yet: no background at all, not a broken url().
        assert_eq!(wave_css("", 10.0, 0.0, 20.0, 1.0), "");

        // The invariant that matters: the slice the CSS exposes is exactly as
        // wide as the clip's box on the timeline, at any speed.
        for speed in [0.5, 1.0, 2.0, 4.0] {
            let (dur, in_s, out_s, scale) = (10.0, 2.0, 6.0, 20.0);
            let mut c = clip(in_s, out_s);
            c.duration = dur;
            c.speed = speed;
            let shown_px = (out_s - in_s) * scale / speed;
            assert!(
                (c.trimmed() * scale - shown_px).abs() < 1e-9,
                "waveform slice and clip width disagree at {speed}x"
            );
        }
    }

    #[test]
    fn dropped_files_are_classified_by_extension() {
        assert_eq!(kind_of("/x/a.mp4"), Kind::Video);
        assert_eq!(kind_of("/x/a.MKV"), Kind::Video);
        assert_eq!(kind_of("/x/a.gif"), Kind::Video); // animated: video, not a still
        assert_eq!(kind_of("/x/a.png"), Kind::Still);
        assert_eq!(kind_of("/x/a.JPEG"), Kind::Still);
        assert_eq!(kind_of("/x/a.mp3"), Kind::Audio);
        assert_eq!(kind_of("/x/a.flac"), Kind::Audio);
        // Unknown falls to video, where probe (and its still fallback) decides.
        assert_eq!(kind_of("/x/mystery.xyz"), Kind::Video);
        assert_eq!(kind_of("/x/noext"), Kind::Video);
    }

    #[test]
    fn drops_route_to_the_lane_the_file_belongs_on() {
        // Dropped where it belongs: no lane change, nothing to explain.
        assert_eq!(route_drop(Kind::Video, Lane::V1), Ok((Lane::V1, None)));
        assert_eq!(route_drop(Kind::Still, Lane::V1), Ok((Lane::V1, None)));
        assert_eq!(route_drop(Kind::Video, Lane::V2), Ok((Lane::V2, None)));
        assert_eq!(route_drop(Kind::Audio, Lane::A1), Ok((Lane::A1, None)));

        // Sound aimed at a video lane still goes to A1, and says so.
        assert_eq!(route_drop(Kind::Audio, Lane::V1), Ok((Lane::A1, Some("audio goes to A1"))));
        assert_eq!(route_drop(Kind::Audio, Lane::V2), Ok((Lane::A1, Some("audio goes to A1"))));

        // A video on A1 contributes its soundtrack rather than being refused.
        assert_eq!(
            route_drop(Kind::Video, Lane::A1),
            Ok((Lane::A1, Some("using its soundtrack")))
        );
        // A photo genuinely has nothing to give an audio track.
        assert!(route_drop(Kind::Still, Lane::A1).is_err());
    }

    #[test]
    fn drop_position_picks_an_insertion_point_not_a_time() {
        // V1 is a concat with no gaps, so a drop can only mean "between these
        // two clips". Clips of 2 s and 3 s: boundaries at 0, 2, 5.
        let clips = [clip(0.0, 2.0), clip(0.0, 3.0)];
        assert_eq!(insert_index(&clips, 0.0), 0);
        assert_eq!(insert_index(&clips, 0.9), 0); // before clip 0's midpoint
        assert_eq!(insert_index(&clips, 1.5), 1); // past it, so after clip 0
        assert_eq!(insert_index(&clips, 3.0), 1); // before clip 1's midpoint (3.5)
        assert_eq!(insert_index(&clips, 4.0), 2); // past it: append
        assert_eq!(insert_index(&clips, 99.0), 2);
        assert_eq!(insert_index(&[], 5.0), 0); // empty timeline
    }

    #[test]
    fn file_name_of_handles_paths_and_junk() {
        assert_eq!(file_name_of("/a/b/clip.mp4"), "clip.mp4");
        assert_eq!(file_name_of("clip.mp4"), "clip.mp4");
        assert_eq!(file_name_of(""), "");
    }

    #[test]
    fn snap_pulls_only_within_tolerance() {
        let targets = [0.0, 5.0, 12.0];
        assert_eq!(snap_to(5.08, &targets, 0.1), 5.0); // inside tolerance
        assert_eq!(snap_to(5.4, &targets, 0.1), 5.4); // outside: left alone
        assert_eq!(snap_to(4.95, &targets, 0.1), 5.0);
        // Ties go to the nearer target, not the first one listed.
        assert_eq!(snap_to(11.0, &[10.0, 12.0], 2.0), 10.0);
        assert_eq!(snap_to(11.5, &[10.0, 12.0], 2.0), 12.0);
        assert_eq!(snap_to(3.0, &[], 1.0), 3.0); // nothing to snap to
    }

    #[test]
    fn platform_limits_warn_only_when_over() {
        assert_eq!(over_limits(30.0), None);
        assert_eq!(over_limits(60.0), None); // exactly at the cap still fits
        let w = over_limits(75.0).unwrap();
        assert!(w.contains("Shorts") && !w.contains("Reels"), "{w}");
        let w = over_limits(120.0).unwrap();
        assert!(w.contains("Shorts") && w.contains("Reels") && !w.contains("TikTok"), "{w}");
        assert!(over_limits(1200.0).unwrap().contains("TikTok"));
    }

    #[test]
    fn project_round_trips_without_derived_data() {
        let mut c = clip(1.0, 3.0);
        c.speed = 1.5;
        c.volume = 0.25;
        c.thumb = "data:image/jpeg;base64,AAAA".to_string(); // derived, must not persist
        c.proxy = "/cache/proxy.mp4".to_string();
        let snap = Snapshot { clips: vec![c], overlays: vec![], audios: vec![], titles: vec![], markers: vec![2.5] };

        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("base64"), "thumbnail leaked into the project file");
        assert!(!json.contains("proxy.mp4"), "proxy path leaked into the project file");

        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.markers, vec![2.5], "beat markers should save with the project");
        let r = &back.clips[0];
        assert_eq!((r.in_s, r.out_s, r.speed, r.volume), (1.0, 3.0, 1.5, 0.25));
        assert!(r.thumb.is_empty() && r.proxy.is_empty(), "derived data should reload empty");
    }

    #[test]
    fn project_without_a_speed_field_loads_at_1x() {
        // A file written before speed existed must not deserialize to 0.0 and
        // divide the timeline by zero.
        let json = r#"{"clips":[{"path":"a.mp4","name":"a","duration":5.0,"in_s":0.0,
            "out_s":5.0,"has_audio":true,"effect":"None","effect_amount":1.0,
            "framing":"Crop","group":0}],"overlays":[],"audios":[],"titles":[]}"#;
        let snap: Snapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.clips[0].speed, 1.0);
        assert_eq!(snap.clips[0].volume, 1.0);
        assert_eq!(snap.clips[0].trimmed(), 5.0);
    }

    /// A title as an older project file would have stored it — before outline
    /// and the extra bevel knobs existed.
    fn legacy_title_json() -> &'static str {
        r#"{"text":"Hi","at":0.0,"dur":3.0,"font_size":110.0,"color":"White",
            "pos":"Middle","bevel":"Cameo","bevel_size":21.0,"font":"Sans",
            "boxed":false,"caption":false,"group":0}"#
    }

    #[test]
    fn word_prefixes_keep_the_breaks_the_text_already_has() {
        assert_eq!(word_prefixes("one two three"), vec!["one", "one two", "one two three"]);
        // A wrapped caption must not be un-wrapped: rejoining on spaces would
        // make the words jump between lines as they arrive.
        assert_eq!(
            word_prefixes("hello there\nfriend"),
            vec!["hello", "hello there", "hello there\nfriend"]
        );
        assert_eq!(word_prefixes("solo"), vec!["solo"]);
        assert_eq!(word_prefixes("  padded  out  "), vec!["  padded", "  padded  out"]);
        assert!(word_prefixes("").is_empty());
        assert!(word_prefixes("   ").is_empty());
    }

    #[test]
    fn a_revealed_title_is_a_run_of_cards_that_tiles_its_span() {
        let mut t: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        t.text = "one two three".into();
        t.at = 2.0;
        t.dur = 3.0;

        // Off: exactly the single card it always was.
        assert_eq!(t.segments(), vec![("one two three".to_string(), 2.0, 3.0)]);

        t.reveal = true;
        let segs = t.segments();
        assert_eq!(segs.len(), 3, "one card per word");
        assert_eq!(segs[0].0, "one");
        assert_eq!(segs[2].0, "one two three");

        // The cards run back to back and finish exactly when the title does.
        assert_eq!(segs[0].1, 2.0, "the first card starts with the title");
        for w in segs.windows(2) {
            assert!((w[0].1 + w[0].2 - w[1].1).abs() < 1e-9, "cards must not gap or overlap");
        }
        let (last_at, last_dur) = (segs[2].1, segs[2].2);
        assert!((last_at + last_dur - 5.0).abs() < 1e-9, "the run must end with the title");
        // The finished line holds far longer than any single word takes.
        assert!(last_dur > segs[0].2 * 2.0, "the whole line should hold to be read");

        // A single word has nothing to reveal, so it stays one card.
        t.text = "solo".into();
        assert_eq!(t.segments().len(), 1);
    }

    #[test]
    fn card_at_finds_the_step_showing_now() {
        let mut t: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        t.text = "one two three".into();
        t.at = 0.0;
        t.dur = 3.0;
        t.reveal = true;
        let segs = t.segments();
        assert_eq!(t.card_at(segs[0].1 + 0.01), Some(0));
        assert_eq!(t.card_at(segs[1].1 + 0.01), Some(1));
        assert_eq!(t.card_at(segs[2].1 + 0.01), Some(2));
        assert_eq!(t.card_at(2.99), Some(2), "the last word holds to the end");
        assert_eq!(t.card_at(5.0), None, "nothing is up after the title is over");
    }

    #[test]
    fn a_style_is_a_look_never_the_words_or_the_timing() {
        let mut src: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        src.text = "SOURCE".into();
        src.at = 9.0;
        src.dur = 1.0;
        src.font_size = 200.0;
        src.color = "Gold".into();
        src.outline = 6.0;
        src.anim = "Slide up".into();

        let mut dst: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        dst.text = "KEEP ME".into();
        dst.at = 3.0;
        dst.dur = 4.0;
        dst.caption = true;
        dst.group = 7;
        dst.pngs = vec!["/cache/old.png".into()];

        let out = restyle(&dst, &src);
        // Its own content and place on the timeline survive untouched.
        assert_eq!(out.text, "KEEP ME");
        assert_eq!((out.at, out.dur), (3.0, 4.0));
        assert!(out.caption && out.group == 7, "lane identity should survive a restyle");
        // The look comes wholesale from the source.
        assert_eq!(out.font_size, 200.0);
        assert_eq!(out.color, "Gold");
        assert_eq!(out.outline, 6.0);
        assert_eq!(out.anim, "Slide up");
        // The rendered card must be dropped, or it would keep the old look.
        assert!(out.pngs.is_empty(), "a restyle has to invalidate the rendered card");
    }

    #[test]
    fn presets_round_trip_through_their_file_format() {
        let mut style: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        style.color = "Cyan".into();
        style.anim = "Slide in left".into();
        style.pngs = vec!["/cache/derived.png".into()]; // derived, must not persist
        let all = vec![TitlePreset { name: "Bold caption".into(), style }];

        let json = serde_json::to_string(&all).unwrap();
        assert!(!json.contains("derived.png"), "a rendered card leaked into the preset file");
        let back: Vec<TitlePreset> = serde_json::from_str(&json).unwrap();
        assert_eq!(back[0].name, "Bold caption");
        assert_eq!(back[0].style.color, "Cyan");
        assert_eq!(back[0].style.anim, "Slide in left");
        assert!(back[0].style.pngs.is_empty());
    }

    #[test]
    fn bevel_labels_round_trip() {
        for (value, label) in BEVELS {
            assert_eq!(bevel_label(value), *label);
            assert_eq!(bevel_value(label), *value);
        }
        // Anything unrecognised falls back to Off rather than a broken render.
        assert_eq!(bevel_value("nonsense"), "Off");
        assert_eq!(bevel_label("nonsense"), "Off");
    }

    /// A 270x480 monitor at the top-left of the screen: centre is (135, 240).
    const RECT: (f64, f64, f64, f64) = (0.0, 0.0, 270.0, 480.0);

    #[test]
    fn dragging_the_box_moves_by_the_fraction_of_the_frame_crossed() {
        let start = engine::Transform::default();
        // Half the monitor's width to the right is x = 0.5, whatever the
        // monitor's pixel size — the transform is stored in frame fractions.
        let t = xf_apply(XfGrab::Move, start, (135.0, 240.0), (270.0, 240.0), RECT, false);
        assert!((t.x - 0.5).abs() < 1e-9, "x = {}", t.x);
        assert_eq!(t.y, 0.0);
        // Up a quarter of the height is negative y.
        let t = xf_apply(XfGrab::Move, start, (135.0, 240.0), (135.0, 120.0), RECT, false);
        assert!((t.y + 0.25).abs() < 1e-9, "y = {}", t.y);
        // Moving does not disturb the other knobs.
        assert_eq!((t.scale, t.rotation, t.opacity), (1.0, 0.0, 1.0));
    }

    #[test]
    fn dragging_a_corner_scales_by_the_distance_from_the_centre() {
        let start = engine::Transform::default();
        // Twice as far from the centre is twice the size.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (335.0, 240.0), RECT);
        assert!((t.scale - 2.0).abs() < 1e-9, "scale = {}", t.scale);
        // Half as far is half the size.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (185.0, 240.0), RECT);
        assert!((t.scale - 0.5).abs() < 1e-9, "scale = {}", t.scale);
        // Clamped to the slider's range rather than collapsing to nothing.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (135.5, 240.0), RECT);
        assert!(t.scale >= 0.1, "scale should not collapse: {}", t.scale);
        // A grab that starts on the centre has no distance to work from.
        assert_eq!(xf_apply(XfGrab::Scale, start, (135.0, 240.0), (200.0, 240.0), RECT).scale, 1.0);
    }

    #[test]
    fn dragging_the_knob_rotates_and_stays_in_slider_range() {
        let start = engine::Transform::default();
        // From straight up to straight right is a quarter turn clockwise.
        let t = xf_apply(XfGrab::Rotate, start, (135.0, 40.0), (235.0, 240.0), RECT);
        assert!((t.rotation - 90.0).abs() < 1e-6, "rotation = {}", t.rotation);
        // Rotation always lands inside what the slider can show.
        for (to_x, to_y) in [(35.0, 240.0), (135.0, 440.0), (200.0, 100.0), (60.0, 300.0)] {
            let r = xf_apply(XfGrab::Rotate, start, (135.0, 40.0), (to_x, to_y), RECT).rotation;
            assert!((-180.0..=180.0).contains(&r), "rotation {r} is outside the slider");
        }
    }

    #[test]
    fn a_zero_sized_monitor_never_produces_nonsense() {
        // The rect is measured asynchronously; a drag must degrade to a no-op
        // rather than divide by zero if it somehow arrives empty.
        let start = engine::Transform { scale: 1.5, x: 0.2, ..Default::default() };
        for grab in [XfGrab::Move, XfGrab::Scale, XfGrab::Rotate] {
            let t = xf_apply(grab, start, (10.0, 10.0), (99.0, 99.0), (0.0, 0.0, 0.0, 0.0));
            assert_eq!(t, start, "{grab:?} on an unmeasured monitor should change nothing");
        }
    }

    #[test]
    fn handle_corners_track_scale_and_rotation() {
        // Unrotated and full size: the corners are the corners of the frame.
        let c = xf_corners(&engine::Transform::default());
        assert_eq!(c[0], (0.0, 0.0));
        assert_eq!(c[2], (1.0, 1.0));

        // Half size, centred: an inset box.
        let c = xf_corners(&engine::Transform { scale: 0.5, ..Default::default() });
        assert_eq!(c[0], (0.25, 0.25));
        assert_eq!(c[2], (0.75, 0.75));

        // Offset moves every corner by the same amount.
        let c = xf_corners(&engine::Transform { scale: 0.5, x: 0.1, y: -0.2, ..Default::default() });
        assert!((c[0].0 - 0.35).abs() < 1e-9 && (c[0].1 - 0.05).abs() < 1e-9, "{c:?}");

        // Rotated: corners must stay equidistant from the centre in *pixel*
        // space, or the box is being sheared by the 9:16 aspect.
        let t = engine::Transform { scale: 0.5, rotation: 37.0, ..Default::default() };
        let ar = engine::W as f64 / engine::H as f64;
        let radii: Vec<f64> = xf_corners(&t)
            .iter()
            .map(|(x, y)| {
                let (px, py) = ((x - 0.5) * ar, y - 0.5); // back to pixel proportions
                (px * px + py * py).sqrt()
            })
            .collect();
        for r in &radii {
            assert!((r - radii[0]).abs() < 1e-9, "rotated box is sheared: {radii:?}");
        }
    }

    #[test]
    fn transform_knob_table_writes_each_field_once() {
        let mut t = engine::Transform::default();
        for (i, (_, _, _, _, _, set)) in transform_knobs(&t, true).into_iter().enumerate() {
            set(&mut t, i as f64 + 1.0);
        }
        assert_eq!((t.scale, t.x, t.y, t.rotation, t.opacity), (1.0, 2.0, 3.0, 4.0, 5.0));
        // V1 has nothing underneath it, so opacity is not offered there.
        assert_eq!(transform_knobs(&t, false).len(), 4);
        assert_eq!(transform_knobs(&t, true).len(), 5);
        assert!(!transform_knobs(&t, false).iter().any(|k| k.0 == "Opacity"));

        // Every slider's range must contain the value it starts at, or the
        // control would jump the moment it is touched.
        let d = engine::Transform::default();
        for (label, value, min, max, _, _) in transform_knobs(&d, true) {
            assert!(value >= min && value <= max, "{label} default {value} is outside {min}..{max}");
        }
    }

    #[test]
    fn a_clips_look_is_geometry_then_grade() {
        let mut c = clip(0.0, 2.0);
        // Untouched: no transform filter at all, just the effect.
        c.effect = "B&W".to_string();
        assert_eq!(c.look(), "hue=s=0");
        // Transformed: geometry first, then the look, comma-joined.
        c.transform.scale = 0.5;
        let look = c.look();
        assert!(look.starts_with("scale="), "geometry should come first: {look}");
        assert!(look.ends_with(",hue=s=0"), "grade should come last: {look}");
        // Neither set: nothing at all, so an untouched clip adds no filters.
        let plain = clip(0.0, 2.0);
        assert_eq!(plain.look(), "");
    }

    #[test]
    fn bevel_knob_table_writes_each_field_once() {
        let mut t: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        // Every setter must land on its own field — a table of seven near
        // identical rows is exactly where a copy-paste lands on the wrong one.
        for (i, (_, _, _, _, set)) in bevel_knobs(&t).into_iter().enumerate() {
            set(&mut t, i as f64 + 1.0);
        }
        assert_eq!(
            (t.bevel_size, t.soften, t.depth, t.angle, t.altitude, t.hi_opacity, t.sh_opacity),
            (1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0)
        );
        assert_eq!(bevel_knobs(&t).len(), 7, "the bevel panel lost a control");
    }

    #[test]
    fn older_titles_load_transparent_with_the_designer_defaults() {
        let t: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        assert_eq!(t.outline, 0.0);
        assert_eq!(t.outline_color, "Black");
        assert_eq!((t.soften, t.depth, t.angle, t.altitude), (4.0, 100.0, 120.0, 30.0));
        assert_eq!((t.hi_opacity, t.sh_opacity), (0.75, 0.75));

        // The item's friendly choices map onto what ffmpeg and the bevel need.
        let s = t.style_of(&t.text);
        assert_eq!((s.color.as_str(), s.outline_color.as_str()), ("white", "black"));
        assert_eq!(s.bevel, "Cameo");
        assert!((s.y_frac - 0.45).abs() < 1e-9);
        assert!(!s.boxed, "a title is transparent unless a backdrop is asked for");
    }

    #[test]
    fn block_map_reorders() {
        // move clips [1,2] of 0..5 to the front: new order 1,2,0,3,4
        assert_eq!((0..5).map(|k| block_map(k, 1, 2, 0)).collect::<Vec<_>>(), vec![2, 0, 1, 3, 4]);
        // move clip [0] to the end: new order 1,2,3,4,0
        assert_eq!((0..5).map(|k| block_map(k, 0, 1, 4)).collect::<Vec<_>>(), vec![4, 0, 1, 2, 3]);
        // no-op move keeps identity
        assert_eq!((0..4).map(|k| block_map(k, 1, 2, 1)).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }
}
