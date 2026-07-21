// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — portrait-only (9:16) video editor.
// V1: main clip track (trim/reorder/split, ripple by construction).
// V2: full-frame cutaway overlays. A1: audio mixed under. Effects per video item.

mod bevel;
mod engine;
mod keyframe;

use dioxus::desktop::tao::window::Icon;
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use engine::{AudioSpec, ClipSpec, OverlaySpec, TitleSpec};
use mor_rust_dioxus_ui_kit::{
    use_shortcut, MenuItem, MenuSeparator, Modal, MorAppFrame, MorCheckbox, MorMenuDropdown,
    MorSelect, MorShortcutRoot, MorStyleProvider, MorTabs, MorTextInput, Slider, UiMode,
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
accent        = "#8f7bf0"
accent_hover  = "#a48ff5"
btn           = "#252530"
btn_hover     = "#323240"
font_family   = "Cantarell, 'Segoe UI', system-ui, sans-serif"
font_size_base= "13px"
font_size_h1  = "20px"
padding_base  = "8px"
border_radius = "7px"
destructive   = "#e5484d"
success       = "#3dd6c8"
warning       = "#e86aa6"
"##;

/// Window / taskbar icon (128px RGBA PNG).
fn window_icon() -> Option<Icon> {
    let img = image::load_from_memory(include_bytes!("../assets/icons/morreel-studio-128.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Titlebar system-menu icon as a data URL (64px PNG, base64).
fn system_icon_src() -> String {
    format!(
        "data:image/png;base64,{}",
        include_str!("../assets/icons/morreel-studio-64.b64").trim()
    )
}

fn main() {
    let mode = UiMode::resolve();
    mode.apply_env();
    let is_native = mode.is_native();
    let cfg = Config::new()
        .with_menu(None::<dioxus::desktop::muda::Menu>)
        .with_window(
            WindowBuilder::new()
                .with_title("MorReel Studio")
                .with_inner_size(LogicalSize::new(1100.0, 720.0))
                .with_decorations(is_native)
                .with_transparent(!is_native)
                .with_window_icon(window_icon()),
        );
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
/// center-crops, Blur fits over a blurred fill, Fit letterboxes on black, Zoom
/// punches in 1.5× then crops.
const FRAMINGS: &[&str] = &["Crop", "Blur", "Fit", "Zoom"];

/// One-line explanation of a framing mode, shown under the picker.
fn framing_hint(name: &str) -> &'static str {
    match name {
        "Blur" => "Whole picture over a blurred fill of itself — best for landscape footage.",
        "Fit" => "Letterboxed on black — nothing cropped, bars top and bottom.",
        "Zoom" => "Punches in 1.5× then crops — tighter, loses the edges.",
        _ => "Fills the frame and center-crops — the usual portrait fit.",
    }
}

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

/// Which platforms the reel has outgrown, e.g. "over Shorts 1:00.0". `platform`
/// narrows the check to a single target ("All platforms" checks every cap).
/// None while it still fits.
fn over_limits(total: f64, platform: &str) -> Option<String> {
    let over: Vec<String> = LIMITS
        .iter()
        .filter(|(name, _)| platform == "All platforms" || *name == platform)
        .filter(|(_, cap)| total > *cap)
        .map(|(name, cap)| format!("{name} {}", fmt_t(*cap)))
        .collect();
    (!over.is_empty()).then(|| format!("over {}", over.join(", ")))
}

/// Editing-key schemes for people arriving from another NLE. MorReel's command
/// set is small, so a scheme only remaps the verbs that genuinely differ between
/// editors — today that's the blade (split at playhead), the muscle memory that
/// trips people up most, and every editor spells it differently.
///
/// Mac Cmd shortcuts are given in their Ctrl form (MorReel is a Linux/Windows
/// desktop build). Avid (Add Edit = Ctrl+E) is left off because it collides with
/// Export, and Pro Tools is a DAW — better to omit them than fake a mapping.
#[derive(Clone, Copy, PartialEq, Debug)]
enum KeyScheme {
    MorReel,
    Resolve,
    Premiere,
    FinalCut,
}

impl KeyScheme {
    const ALL: [KeyScheme; 4] = [Self::MorReel, Self::Resolve, Self::Premiere, Self::FinalCut];

    fn label(self) -> &'static str {
        match self {
            Self::MorReel => "MorReel (default)",
            Self::Resolve => "DaVinci Resolve",
            Self::Premiere => "Adobe Premiere Pro",
            Self::FinalCut => "Final Cut Pro",
        }
    }

    /// Stable token for persistence — never shown, so it can stay terse.
    fn id(self) -> &'static str {
        match self {
            Self::MorReel => "morreel",
            Self::Resolve => "resolve",
            Self::Premiere => "premiere",
            Self::FinalCut => "finalcut",
        }
    }

    fn from_id(s: &str) -> Self {
        Self::ALL.into_iter().find(|k| k.id() == s).unwrap_or(Self::MorReel)
    }

    /// The split-at-playhead (blade / add-edit) key in this editor's convention.
    fn split(self) -> &'static str {
        match self {
            Self::MorReel => "S",
            Self::Resolve => "Ctrl+\\",
            Self::Premiere => "Ctrl+K",
            Self::FinalCut => "Ctrl+B",
        }
    }
}

/// The help table's displayed key for a row: the blade row follows the active
/// scheme; every other row is fixed. Pulled out of the rsx loop because an inline
/// `if`-expression there parses as a conditional-render node, not a value.
fn help_key<'a>(keys: &'a str, what: &str, scheme: KeyScheme) -> &'a str {
    if what == "Split at playhead" {
        scheme.split()
    } else {
        keys
    }
}

/// The reel-building phases. The bottom workflow bar switches between them and
/// the inspector reconfigures to show that phase's tools — the panel is organized
/// by task, not by whatever was last clicked.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Add,
    Cut,
    Style,
    Background,
    Text,
    Audio,
    Export,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Add => "Add",
            Phase::Cut => "Cut",
            Phase::Style => "Style",
            Phase::Background => "Background",
            Phase::Text => "Text",
            Phase::Audio => "Audio",
            Phase::Export => "Export",
        }
    }
}

/// The timeline's phase-emphasis class: the CSS rules keyed on it dim the lanes
/// the active phase doesn't touch, spotlighting the ones it does. Add/Export
/// touch everything, so they emphasize nothing (empty string).
fn phase_lane_class(p: Phase) -> &'static str {
    match p {
        Phase::Cut => "mr-phase-cut",
        Phase::Style => "mr-phase-style",
        Phase::Text => "mr-phase-text",
        Phase::Audio => "mr-phase-audio",
        // Background and Cut/Style are all about the picture → spotlight video.
        Phase::Background => "mr-phase-cut",
        Phase::Add | Phase::Export => "",
    }
}

/// The phase a selection naturally belongs to — used to auto-jump when you click
/// an item on the timeline. A clip/overlay stays in Style if you're already there
/// (so browsing clips while grading doesn't kick you back to Cut); everything
/// else maps to its own phase, and an empty selection keeps the current phase.
fn phase_for_selection(sel: Option<Sel>, current: Phase) -> Phase {
    match sel {
        Some(Sel::Title(_)) => Phase::Text,
        Some(Sel::Aud(_)) => Phase::Audio,
        Some(Sel::Over(_)) | Some(Sel::Main(_)) => {
            if current == Phase::Style {
                Phase::Style
            } else {
                Phase::Cut
            }
        }
        None => current,
    }
}

fn keyscheme_path() -> std::path::PathBuf {
    engine::config_dir().join("keyscheme")
}

/// App-wide, not per-project — a keyboard preference belongs to the person, like
/// the window mode, not to any one reel. Persisted beside the other app config.
fn load_keyscheme() -> KeyScheme {
    std::fs::read_to_string(keyscheme_path())
        .map(|s| KeyScheme::from_id(s.trim()))
        .unwrap_or(KeyScheme::MorReel)
}

fn save_keyscheme(k: KeyScheme) {
    let _ = std::fs::create_dir_all(engine::config_dir());
    let _ = std::fs::write(keyscheme_path(), k.id());
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
    /// A corner: both axes together, so the picture keeps its shape.
    Scale,
    /// A side: one axis only. This is the stretch.
    StretchX,
    StretchY,
    Rotate,
}

/// The four corners of the transform box, as fractions across the monitor
/// (0,0 = top left, 1,1 = bottom right).
///
/// The rotation has to be applied in *pixel* space and converted back, or on a
/// 9:16 frame a rotated box comes out sheared: one fraction of width is not the
/// same distance as one fraction of height.
fn xf_corners(t: &engine::Transform) -> [(f64, f64); 4] {
    let (half_w, half_h) = (t.scale * t.scale_x / 2.0, t.scale * t.scale_y / 2.0);
    let (sin, cos) = t.rotation.to_radians().sin_cos();
    let ar = engine::W as f64 / engine::H as f64;
    // The box turns about its own centre, and that centre is where the
    // position offset put it.
    let (cx, cy) = (0.5 + t.x, 0.5 + t.y);
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(sx, sy)| {
        let (fx, fy) = (sx * half_w, sy * half_h);
        (cx + (fx * cos - fy * sin / ar), cy + (fx * sin * ar + fy * cos))
    })
}

/// The midpoints of the four sides, where the stretch handles live. Same
/// rotation treatment as the corners so they stay glued to the box.
fn xf_edges(t: &engine::Transform) -> [(f64, f64); 4] {
    let (half_w, half_h) = (t.scale * t.scale_x / 2.0, t.scale * t.scale_y / 2.0);
    let (sin, cos) = t.rotation.to_radians().sin_cos();
    let ar = engine::W as f64 / engine::H as f64;
    let (cx, cy) = (0.5 + t.x, 0.5 + t.y);
    // left, right, top, bottom — the first two stretch across, the last two down
    [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)].map(|(sx, sy)| {
        let (fx, fy) = (sx * half_w, sy * half_h);
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
    snap: bool,
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
        // A side handle stretches one axis, measured along that axis alone —
        // radial distance would make dragging sideways change the height too.
        XfGrab::StretchX => {
            let (d0, d1) = ((from.0 - cx).abs(), (to.0 - cx).abs());
            if d0 > 2.0 {
                t.scale_x = (start.scale_x * d1 / d0).clamp(0.1, 4.0);
            }
        }
        XfGrab::StretchY => {
            let (d0, d1) = ((from.1 - cy).abs(), (to.1 - cy).abs());
            if d0 > 2.0 {
                t.scale_y = (start.scale_y * d1 / d0).clamp(0.1, 4.0);
            }
        }
        XfGrab::Rotate => {
            let a0 = (from.1 - cy).atan2(from.0 - cx);
            let a1 = (to.1 - cy).atan2(to.0 - cx);
            let mut deg = start.rotation + (a1 - a0).to_degrees();
            // Shift snaps to 15°, which is how you get a level horizon or a
            // clean right angle without nudging a slider.
            if snap {
                deg = (deg / 15.0).round() * 15.0;
            }
            // Keep it in the -180..180 the slider shows.
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
        ("Stretch across", t.scale_x, 0.1, 4.0, 0.01, |x, v| x.scale_x = v),
        ("Stretch down", t.scale_y, 0.1, 4.0, 0.01, |x, v| x.scale_y = v),
        ("Position X", t.x, -1.0, 1.0, 0.005, |x, v| x.x = v),
        ("Position Y", t.y, -1.0, 1.0, 0.005, |x, v| x.y = v),
        ("Rotation", t.rotation, -180.0, 180.0, 1.0, |x, v| x.rotation = v),
    ];
    if with_opacity {
        knobs.push(("Opacity", t.opacity, 0.0, 1.0, 0.01, |x, v| x.opacity = v));
    }
    knobs
}

/// One row of the grade panel: label, current value, min, max, step, and how to
/// write it back — the same shape as [`XformKnob`], so both drive the identical
/// slider loop. Ranges are the reel-sane subset of each ffmpeg control.
type GradeKnob = (&'static str, f64, f64, f64, f64, fn(&mut engine::Grade, f64));

fn grade_knobs(g: &engine::Grade) -> Vec<GradeKnob> {
    let set_exp: fn(&mut engine::Grade, f64) = |x, v| x.exposure = v;
    vec![
        ("Exposure", g.exposure, -0.3, 0.3, 0.01, set_exp),
        ("Contrast", g.contrast, 0.5, 1.8, 0.01, |x, v| x.contrast = v),
        ("Saturation", g.saturation, 0.0, 2.5, 0.01, |x, v| x.saturation = v),
        ("Warmth (K)", g.warmth, 4000.0, 9000.0, 100.0, |x, v| x.warmth = v),
    ]
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
    transform: engine::AnimatedTransform,
    /// Primary colour grade — exposure/contrast/saturation/warmth. Runs before
    /// the effect preset in `look()`; identity by default so old projects load.
    #[serde(default)]
    grade: engine::Grade,
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
            bg: self.transform.bg,
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
            join_chain(self.transform.chain(engine::W, engine::H, false), self.grade.chain()),
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
    let audio = if !c.has_audio {
        " • no audio"
    } else if c.volume <= 0.0 {
        " • audio muted (detached or silent)"
    } else {
        ""
    };
    format!(
        "{} source • keeping {}{}{}{}",
        fmt_t(c.duration),
        fmt_t(c.trimmed()),
        if (c.speed - 1.0).abs() > 0.01 { format!(" at {:.2}×", c.speed) } else { String::new() },
        audio,
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
    transform: engine::AnimatedTransform,
    /// Primary colour grade — same as a V1 clip, runs before the effect look.
    #[serde(default)]
    grade: engine::Grade,
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
            join_chain(self.transform.chain(engine::W, engine::H, true), self.grade.chain()),
            effect_filter_amt(&self.effect, self.effect_amount),
        )
    }

    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

fn audio_lane_default() -> u8 {
    1
}
/// Sentinel for "volume end equals volume start" so older projects stay flat.
fn vol_end_default() -> f64 {
    -1.0
}
/// afftdn noise floor for projects saved before the knob existed — the value
/// that was hardcoded, so old projects denoise exactly as they used to.
fn noise_floor_default() -> f64 {
    -25.0
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct AudioItem {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    at: f64,
    /// Start gain; with `vol_end` forms a linear automation ramp.
    volume: f64,
    /// End gain for volume automation. Negative = same as `volume`.
    #[serde(default = "vol_end_default")]
    vol_end: f64,
    /// How hard this bed ducks under the main track while it is talking.
    /// 0 = never. Music under a voiceover is the reason this exists.
    #[serde(default)]
    duck: f64,
    /// Fade in from silence (seconds of the kept span).
    #[serde(default)]
    fade_in: f64,
    /// Fade out to silence (seconds of the kept span).
    #[serde(default)]
    fade_out: f64,
    /// Spectral denoise 0..=1.
    #[serde(default)]
    denoise: f64,
    /// afftdn noise floor in dB (−80..=−20); the sensitivity knob.
    #[serde(default = "noise_floor_default")]
    noise_floor: f64,
    /// Adaptively track the noise floor over the clip (afftdn `tn`).
    #[serde(default)]
    track_noise: bool,
    /// Broadband compression 0..=1.
    #[serde(default)]
    compress: f64,
    /// Noise gate 0..=1 (`agate`) — kills room tone between words.
    #[serde(default)]
    gate: f64,
    /// De-click strength 0..=1 (`adeclick`) — pops in field audio.
    #[serde(default)]
    declick: f64,
    /// One of engine::AUDIO_TREATS — EQ / voice shaping.
    #[serde(default = "none_label")]
    treat: String,
    /// Mix bus: 1 = A1 (music), 2 = A2 (VO / second bed). Both under V1.
    #[serde(default = "audio_lane_default")]
    lane: u8,
    /// Full-source waveform data URI; empty until the background render lands.
    #[serde(skip)]
    wave: String,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
}

impl AudioItem {
    fn end_gain(&self) -> f64 {
        if self.vol_end < 0.0 {
            self.volume
        } else {
            self.vol_end
        }
    }

    fn lane_tag(&self) -> &'static str {
        if self.lane >= 2 { "A2" } else { "A1" }
    }

    fn span(&self) -> f64 {
        (self.out_s - self.in_s).max(0.01)
    }
}

/// One mixer channel: a gain fader, a mute, and a solo. The three strips the
/// mixer holds are V1 (every clip's own audio), A1 and A2 (the two beds).
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Strip {
    #[serde(default = "unity")]
    gain: f64,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
}
impl Default for Strip {
    fn default() -> Self {
        Strip { gain: 1.0, mute: false, solo: false }
    }
}

/// Track index into [`Mixer::tracks`]. Kept as plain constants — three fixed
/// buses, not a growing list, so an enum would be ceremony.
const MIX_V1: usize = 0;
const MIX_A1: usize = 1;
const MIX_A2: usize = 2;
const MIX_LABELS: [&str; 3] = ["V1", "A1", "A2"];

/// A tiny 3-channel mixer plus a master fader. It is the one place track-level
/// level, mute and solo live; per-clip gain still rides on top. Applied once in
/// `gather_specs`, so preview and export hear exactly the same balance.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Mixer {
    #[serde(default)]
    tracks: [Strip; 3],
    #[serde(default = "unity")]
    master: f64,
}
impl Default for Mixer {
    fn default() -> Self {
        Mixer { tracks: Default::default(), master: 1.0 }
    }
}
impl Mixer {
    fn any_solo(&self) -> bool {
        self.tracks.iter().any(|s| s.solo)
    }
    /// Effective linear gain for track `i`, folding in mute, solo and master.
    /// 0.0 means silent — muted, or soloed-out while another track solos.
    fn gain_of(&self, i: usize) -> f64 {
        let s = &self.tracks[i];
        if s.mute || (self.any_solo() && !s.solo) {
            0.0
        } else {
            (s.gain * self.master).max(0.0)
        }
    }
    /// Track index for an audio bed's lane number (1 = A1, ≥2 = A2).
    fn lane_track(lane: u8) -> usize {
        if lane >= 2 {
            MIX_A2
        } else {
            MIX_A1
        }
    }
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

/// The out-of-the-box title look every new card and every built-in style starts
/// from. Callers override `text`/`at`/`dur` per instance.
fn base_title() -> TitleItem {
    TitleItem {
        text: "Text".to_string(),
        at: 0.0,
        dur: 3.0,
        font_size: 110.0,
        color: "White".to_string(),
        pos: "Middle".to_string(),
        // Flat by default: bevel is an opt-in effect, not the default look. An
        // outline still carries legibility on its own.
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
        // Transparent + outline: the video shows through with no opaque plate.
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
    }
}

/// Ready-made looks offered above the user's saved presets — the starter
/// gallery OpenShot/kdenlive ship, but built from MorReel's own knobs so every
/// one renders through the real title engine. `restyle` keeps each card's own
/// words and timing; only the look is copied.
fn builtin_title_styles() -> Vec<TitlePreset> {
    let named = |name: &str, style: TitleItem| TitlePreset { name: name.to_string(), style };
    vec![
        // The punchy phone caption: big, heavy outline, sat in the lower third.
        named("Bold caption", TitleItem { font_size: 150.0, outline: 12.0, pos: "Lower third".into(), ..base_title() }),
        // News lower-third: opaque plate instead of an outline.
        named("Lower-third box", TitleItem { boxed: true, outline: 0.0, font_size: 90.0, pos: "Lower third".into(), ..base_title() }),
        // The embossed look — MorReel's signature bevel, in gold.
        named("Gold chisel", TitleItem { color: "Gold".into(), bevel: "Cameo".into(), outline: 0.0, font_size: 130.0, ..base_title() }),
        named("Neon pop", TitleItem { color: "Cyan".into(), outline: 8.0, font_size: 130.0, ..base_title() }),
        named("Red alert", TitleItem { color: "Red".into(), outline_color: "White".into(), outline: 6.0, font_size: 140.0, ..base_title() }),
        // Carved into the video rather than standing off it.
        named("Carved", TitleItem { bevel: "Intaglio".into(), outline: 0.0, font_size: 130.0, ..base_title() }),
    ]
}

/// A named workspace arrangement. In this app the Inspector is the only panel
/// that docks/floats/hides, so a "layout" is just its state — no per-panel
/// registry until a second dockable panel exists (see the View menu comment).
/// Saved outside the project (like title presets) so every reel shares them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Layout {
    name: String,
    inspector_open: bool,
    inspector_float: bool,
    #[serde(default)]
    float_xy: Option<(f64, f64)>,
    #[serde(default)]
    float_size: Option<(f64, f64)>,
}

/// Built-in arrangements, always offered above the user's saved ones.
fn preset_layouts() -> [Layout; 3] {
    [
        Layout { name: "Editing".into(), inspector_open: true, inspector_float: false, float_xy: None, float_size: None },
        Layout { name: "Focus".into(), inspector_open: false, inspector_float: false, float_xy: None, float_size: None },
        Layout { name: "Floating".into(), inspector_open: true, inspector_float: true, float_xy: None, float_size: None },
    ]
}

fn layouts_path() -> std::path::PathBuf {
    engine::config_dir().join("layouts.json")
}

fn load_layouts() -> Vec<Layout> {
    std::fs::read_to_string(layouts_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_layouts(all: &[Layout]) -> Result<(), String> {
    let dir = engine::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(layouts_path(), json).map_err(|e| e.to_string())
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
    /// Track mixer (V1/A1/A2 level, mute, solo + master). Older projects load
    /// with a flat, un-muted mixer.
    #[serde(default)]
    mixer: Mixer,
}

/// Whether `current` differs from the last-saved baseline JSON — the "● Edited"
/// test. Comparing serialized JSON (not the struct) is deliberate: thumb/wave/
/// proxy are `#[serde(skip)]`, so a background proxy or waveform landing never
/// counts as an edit — only what would be written to disk does. With no baseline
/// (never saved this session), any content on the timeline counts as unsaved.
fn timeline_dirty(current: &Snapshot, baseline: Option<&str>) -> bool {
    let empty = current.clips.is_empty()
        && current.overlays.is_empty()
        && current.audios.is_empty()
        && current.titles.is_empty();
    let cur = serde_json::to_string(current).unwrap_or_default();
    baseline.map_or(!empty, |b| b != cur)
}

// --- OpenTimelineIO export (principle 8) ---------------------------------
//
// OTIO is a documented JSON schema, so it's emitted directly with serde_json —
// pulling in the OTIO crate to write a few nested objects would be the kind of
// dependency ponytail exists to refuse. This is export only: a one-way bridge
// out to FCP / Resolve / Premiere, not a round-trip project format (that's what
// .morreel is). Speed retimes and transitions are dropped — clips land at their
// source spans, which is what an interchange hand-off actually needs.

/// An OTIO `RationalTime`: a count of frames at `fps`.
fn otio_rt(seconds: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": (seconds * fps).round() })
}

fn otio_range(start: f64, dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": otio_rt(start, fps),
        "duration": otio_rt(dur.max(0.0), fps),
    })
}

/// A clip referencing an external media file by `source_range` (the portion of
/// the source used). `path` becomes a `file://` URL the receiving app resolves.
fn otio_clip(name: &str, path: &str, src_start: f64, src_dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({
        "OTIO_SCHEMA": "Clip.1",
        "name": name,
        "source_range": otio_range(src_start, src_dur, fps),
        "media_reference": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": format!("file://{path}") },
    })
}

fn otio_gap(dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({ "OTIO_SCHEMA": "Gap.1", "source_range": otio_range(0.0, dur, fps) })
}

/// Lay `(at, dur, node)` items onto one track, inserting a `Gap` before any item
/// that doesn't start where the previous one ended. Items must be sorted by `at`.
fn otio_lay(items: Vec<(f64, f64, serde_json::Value)>, fps: f64) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut cursor = 0.0;
    for (at, dur, node) in items {
        if at > cursor + 1e-6 {
            out.push(otio_gap(at - cursor, fps));
        }
        out.push(node);
        cursor = at + dur;
    }
    out
}

/// Serialize a timeline snapshot to an OpenTimelineIO document. V1 is the
/// primary storyline (contiguous); V2 / A1 / A2 / captions are placed by their
/// absolute `at` with gaps. Captions carry a `MissingReference` — they're timed
/// text, not media — so an NLE shows them on their own track to re-style.
fn snapshot_to_otio(snap: &Snapshot, name: &str, fps: f64) -> String {
    let v1: Vec<_> = snap
        .clips
        .iter()
        .map(|c| otio_clip(&c.name, &c.path, c.in_s, c.out_s - c.in_s, fps))
        .collect();

    let mut ov: Vec<_> = snap
        .overlays
        .iter()
        .map(|o| (o.at, o.trimmed(), otio_clip(&o.name, &o.path, o.in_s, o.out_s - o.in_s, fps)))
        .collect();
    ov.sort_by(|a, b| a.0.total_cmp(&b.0));

    let audio_track = |bus_hi: bool| {
        let mut a: Vec<_> = snap
            .audios
            .iter()
            .filter(|a| (a.lane >= 2) == bus_hi)
            .map(|a| (a.at, a.out_s - a.in_s, otio_clip(&a.name, &a.path, a.in_s, a.out_s - a.in_s, fps)))
            .collect();
        a.sort_by(|x, y| x.0.total_cmp(&y.0));
        otio_lay(a, fps)
    };

    let mut caps: Vec<_> = snap
        .titles
        .iter()
        .map(|t| {
            let node = serde_json::json!({
                "OTIO_SCHEMA": "Clip.1",
                "name": t.text.replace('\n', " ").chars().take(48).collect::<String>(),
                "source_range": otio_range(0.0, t.dur, fps),
                "media_reference": { "OTIO_SCHEMA": "MissingReference.1" },
            });
            (t.at, t.dur, node)
        })
        .collect();
    caps.sort_by(|a, b| a.0.total_cmp(&b.0));

    let track = |name: &str, kind: &str, children: Vec<serde_json::Value>| {
        serde_json::json!({ "OTIO_SCHEMA": "Track.1", "name": name, "kind": kind, "children": children })
    };

    let doc = serde_json::json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": name,
        "global_start_time": otio_rt(0.0, fps),
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": [
                track("V1", "Video", v1),
                track("V2", "Video", otio_lay(ov, fps)),
                track("A1", "Audio", audio_track(false)),
                track("A2", "Audio", audio_track(true)),
                track("Captions", "Video", otio_lay(caps, fps)),
            ],
        },
    });
    serde_json::to_string_pretty(&doc).unwrap()
}

/// Per-project settings — the portrait-reel analog of kdenlive's project
/// profile/metadata. Kept out of `Snapshot` on purpose: these are not edits, so
/// changing the platform or the title must not land on the undo stack. Saved
/// alongside the timeline via [`Project`], defaulted so old files still load.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct ProjectSettings {
    /// One of the `LIMITS` names, or "All platforms" to warn against every cap.
    platform: String,
    /// Target export width (a `SIZES` entry); 9:16 fixes the height.
    resolution: u32,
    /// Whether safe-area guides start on for this project.
    guides: bool,
    title: String,
    author: String,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self { platform: "All platforms".into(), resolution: 1080, guides: false, title: String::new(), author: String::new() }
    }
}

/// The on-disk `.morreel` file: the timeline plus its settings. `flatten` keeps
/// the timeline fields at the top level, so a pre-settings project (a bare
/// `Snapshot`) still deserializes — `settings` just falls back to default.
#[derive(serde::Serialize, serde::Deserialize)]
struct Project {
    #[serde(flatten)]
    snap: Snapshot,
    #[serde(default)]
    settings: ProjectSettings,
}

/// The duration cap for one platform, if it has one.
fn platform_cap(platform: &str) -> Option<f64> {
    LIMITS.iter().find(|(n, _)| *n == platform).map(|(_, c)| *c)
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
    A2,
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
        // Sound is sound — A1/A2 both accept it; elsewhere it defaults to A1.
        (Kind::Audio, Lane::A1) | (Kind::Audio, Lane::A2) => Ok((onto, None)),
        (Kind::Audio, _) => Ok((Lane::A1, Some("audio goes to A1"))),
        // A video dropped on an audio lane contributes its soundtrack.
        (Kind::Video, Lane::A1) | (Kind::Video, Lane::A2) => {
            Ok((onto, Some("using its soundtrack")))
        }
        (Kind::Still, Lane::A1) | (Kind::Still, Lane::A2) => {
            Err("a photo has no sound to mix")
        }
        (_, lane) => Ok((lane, None)),
    }
}

/// Lane tag → bus number stored on AudioItem.
fn lane_num(lane: Lane) -> u8 {
    match lane {
        Lane::A2 => 2,
        _ => 1,
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

/// Which grip is driving a floated-panel interaction.
#[derive(Clone, Copy, PartialEq)]
enum FloatGrab {
    Move,
    N,
    S,
    E,
    W,
    Ne,
    Nw,
    Se,
    Sw,
}

/// Logical viewport size for placing floated panels (desktop window, CSS px).
fn viewport_logical() -> (f64, f64) {
    let win = dioxus::desktop::window();
    let size = win.inner_size();
    let scale = win.scale_factor().max(0.1);
    (size.width as f64 / scale, size.height as f64 / scale)
}

/// Default float placement: right side, under the chrome, matching the old CSS.
fn float_default_geom() -> (f64, f64, f64, f64) {
    let (vw, vh) = viewport_logical();
    let w = 380.0_f64.min(vw - 24.0).max(280.0);
    let h = (vh * 0.72).min(760.0).min(vh - 24.0).max(220.0);
    let x = (vw - w - 18.0).max(8.0);
    let y = 72.0_f64.min(vh - h - 8.0).max(8.0);
    (x, y, w, h)
}

/// Apply a float move/resize step; clamps size and keeps the panel on-screen.
fn float_apply(
    grab: FloatGrab,
    origin: (f64, f64, f64, f64),
    from: (f64, f64),
    to: (f64, f64),
) -> (f64, f64, f64, f64) {
    let (ox, oy, ow, oh) = origin;
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let (mut x, mut y, mut w, mut h) = (ox, oy, ow, oh);
    const MIN_W: f64 = 280.0;
    const MIN_H: f64 = 220.0;
    match grab {
        FloatGrab::Move => {
            x = ox + dx;
            y = oy + dy;
        }
        FloatGrab::E => w = (ow + dx).max(MIN_W),
        FloatGrab::S => h = (oh + dy).max(MIN_H),
        FloatGrab::W => {
            w = (ow - dx).max(MIN_W);
            x = ox + (ow - w);
        }
        FloatGrab::N => {
            h = (oh - dy).max(MIN_H);
            y = oy + (oh - h);
        }
        FloatGrab::Se => {
            w = (ow + dx).max(MIN_W);
            h = (oh + dy).max(MIN_H);
        }
        FloatGrab::Sw => {
            w = (ow - dx).max(MIN_W);
            x = ox + (ow - w);
            h = (oh + dy).max(MIN_H);
        }
        FloatGrab::Ne => {
            w = (ow + dx).max(MIN_W);
            h = (oh - dy).max(MIN_H);
            y = oy + (oh - h);
        }
        FloatGrab::Nw => {
            w = (ow - dx).max(MIN_W);
            x = ox + (ow - w);
            h = (oh - dy).max(MIN_H);
            y = oy + (oh - h);
        }
    }
    let (vw, vh) = viewport_logical();
    w = w.min(vw - 16.0).max(MIN_W);
    h = h.min(vh - 16.0).max(MIN_H);
    x = x.clamp(0.0, (vw - 48.0).max(0.0));
    y = y.clamp(0.0, (vh - 48.0).max(0.0));
    (x, y, w, h)
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
                    // Background is global (the Bg swatch sets every clip at once),
                    // so any clip's bg is the reel's — first() is representative and
                    // fills a "Fit" letterbox to match the export.
                    // ponytail: reads first(), not the playhead clip; identical while
                    // bg stays uniform, which the only bg control guarantees.
                    let fill = clips
                        .read()
                        .first()
                        .map_or("black", |c| c.transform.bg.color());
                    if let Ok(uri) = engine::frame_data_uri_fill(&p, t, 540, 960, &fr, &e, ov, fill).await {
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
                Err(e) => status.set(format!("Text render failed: {e}")),
            }
        });
    };

    // Rendering a PNG per keystroke makes the text field lag and fight the caret:
    // each render finishes async and re-renders the inspector, resetting the
    // input's value mid-type. Debounce — the caller updates the text now (cheap),
    // the render fires once typing settles. A generation counter drops every
    // render a newer keystroke supersedes, so only the last one runs.
    let mut title_render_gen = use_signal(|| 0u64);
    let mut rerender_title_soon = move |k: usize| {
        let g = title_render_gen() + 1;
        title_render_gen.set(g);
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if title_render_gen() == g {
                rerender_title(k);
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
    let mut mixer = use_signal(Mixer::default);
    let snapshot = move || Snapshot {
        clips: clips(),
        overlays: overlays(),
        audios: audios(),
        titles: titles(),
        markers: markers(),
        mixer: mixer(),
    };
    // Unsaved-changes tracking. Baseline = the reel's serialized form as last
    // saved or opened; None means never saved this session. Comparing the *JSON*
    // (not the struct) is deliberate: thumb/wave/proxy are `#[serde(skip)]`, so a
    // background proxy landing never counts as an edit — only what hits disk does.
    let mut saved_json = use_signal(|| Option::<String>::None);
    let is_dirty = move || timeline_dirty(&snapshot(), saved_json().as_deref());
    let mut restore = move |s: Snapshot| {
        clips.set(s.clips);
        overlays.set(s.overlays);
        audios.set(s.audios);
        titles.set(s.titles);
        markers.set(s.markers);
        mixer.set(s.mixer);
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

    // Auto-cut silence: dialog knobs + busy flag while ffmpeg detects.
    let mut show_autocut = use_signal(|| false);
    let mut autocut_busy = use_signal(|| false);
    /// Silence threshold as positive dB magnitude (UI shows −N dB).
    let mut autocut_noise = use_signal(|| 32.0_f64);
    let mut autocut_min_sil = use_signal(|| 0.35_f64);
    let mut autocut_pad = use_signal(|| 0.08_f64);
    let mut autocut_min_keep = use_signal(|| 0.15_f64);
    /// true = only the selected V1 clip; false = every V1 clip with audio.
    let mut autocut_sel_only = use_signal(|| true);

    // Drop quiet stretches from V1 using ffmpeg silencedetect. Optional: only
    // the selected clip, or every clip with audio. Shortens the reel; magnet
    // rides attached items to the first surviving piece of each old clip.
    let mut run_autocut = move |_: ()| {
        if clips.read().is_empty() || autocut_busy() {
            return;
        }
        let sel_only = autocut_sel_only();
        let noise_db = -autocut_noise().abs();
        let min_sil = autocut_min_sil();
        let pad = autocut_pad();
        let min_keep = autocut_min_keep();
        let targets: Vec<usize> = if sel_only {
            match selected() {
                Some(Sel::Main(i)) if clips.read().get(i).is_some_and(|c| c.has_audio) => {
                    vec![i]
                }
                _ => {
                    status.set(
                        "Select a V1 clip with audio, or turn off “selection only” to cut every clip."
                            .into(),
                    );
                    return;
                }
            }
        } else {
            clips
                .read()
                .iter()
                .enumerate()
                .filter(|(_, c)| c.has_audio && !engine::is_still(&c.path))
                .map(|(i, _)| i)
                .collect()
        };
        if targets.is_empty() {
            status.set("No clips with audio to auto-cut.".into());
            return;
        }
        show_autocut.set(false);
        autocut_busy.set(true);
        status.set(format!("Detecting silence on {} clip(s)…", targets.len()));
        spawn(async move {
            // Snapshot sources before we rewrite the timeline.
            let snapshot: Vec<(usize, Clip)> = {
                let cl = clips.read();
                targets
                    .iter()
                    .filter_map(|&i| cl.get(i).cloned().map(|c| (i, c)))
                    .collect()
            };
            let mut keeps_by_old: std::collections::HashMap<usize, Vec<(f64, f64)>> =
                std::collections::HashMap::new();
            let mut errors = Vec::new();
            for (i, c) in &snapshot {
                match engine::detect_silence(&c.path, noise_db, min_sil).await {
                    Ok(sil) => {
                        let keeps =
                            engine::keep_loud_ranges(c.in_s, c.out_s, &sil, pad, min_keep);
                        keeps_by_old.insert(*i, keeps);
                    }
                    Err(e) => errors.push(format!("{}: {e}", c.name)),
                }
            }
            // Build the new V1 list: each old clip becomes 0..N loud pieces.
            let old_spans = spans();
            let old_len = clips.read().len();
            let mut first_new: Vec<Option<usize>> = vec![None; old_len];
            let mut new_clips: Vec<Clip> = Vec::new();
            let mut cut_count = 0usize;
            let mut removed_s = 0.0_f64;
            {
                let cl = clips.read().clone();
                for (oi, c) in cl.into_iter().enumerate() {
                    let Some(keeps) = keeps_by_old.get(&oi) else {
                        first_new[oi] = Some(new_clips.len());
                        new_clips.push(c);
                        continue;
                    };
                    if keeps.is_empty() {
                        // Fully silent: drop the clip (true auto-cut).
                        removed_s += c.trimmed();
                        cut_count += 1;
                        continue;
                    }
                    // Unchanged single full span — leave the clip alone.
                    if keeps.len() == 1
                        && (keeps[0].0 - c.in_s).abs() < 1e-3
                        && (keeps[0].1 - c.out_s).abs() < 1e-3
                    {
                        first_new[oi] = Some(new_clips.len());
                        new_clips.push(c);
                        continue;
                    }
                    let old_span = c.out_s - c.in_s;
                    let keep_span: f64 = keeps.iter().map(|(a, b)| b - a).sum();
                    removed_s += ((old_span - keep_span) / c.speed.max(0.01)).max(0.0);
                    for (j, &(a, b)) in keeps.iter().enumerate() {
                        let mut piece = c.clone();
                        piece.in_s = a;
                        piece.out_s = b;
                        if j > 0 {
                            piece.transition = "None".into();
                            piece.trans_dur = 0.5;
                            // Force a fresh poster so halves are tellable apart.
                            piece.thumb.clear();
                        }
                        if first_new[oi].is_none() {
                            first_new[oi] = Some(new_clips.len());
                        }
                        new_clips.push(piece);
                        cut_count += 1;
                    }
                }
            }
            if new_clips.is_empty() {
                autocut_busy.set(false);
                status.set(
                    "Auto-cut would remove everything — raise the threshold or lower min silence."
                        .into(),
                );
                return;
            }
            push_undo("");
            marked.write().clear();
            clips.set(new_clips);
            ride(old_spans, &|k| {
                first_new.get(k).copied().flatten().map(|ni| start_of(ni))
            });
            selected.set(None);
            // Refresh empty thumbs for new piece in-points.
            let need: Vec<(usize, String, f64, String)> = clips
                .read()
                .iter()
                .enumerate()
                .filter(|(_, c)| c.thumb.is_empty())
                .map(|(i, c)| (i, c.scrub_path(), c.in_s, c.framing.clone()))
                .collect();
            for (i, path, t, fr) in need {
                spawn(async move {
                    if let Ok(thumb) =
                        engine::frame_data_uri(&path, t, 108, 192, &fr, "", engine::Over::default())
                            .await
                    {
                        if let Some(c) = clips.write().get_mut(i) {
                            if c.thumb.is_empty() {
                                c.thumb = thumb;
                            }
                        }
                    }
                });
            }
            seek_to(playhead().min(total_of()));
            autocut_busy.set(false);
            let mut msg = if cut_count == 0 {
                "Auto-cut: nothing quiet enough to remove.".to_string()
            } else {
                format!(
                    "Auto-cut: {cut_count} piece(s), removed ~{} of silence.",
                    fmt_t(removed_s)
                )
            };
            if !errors.is_empty() {
                msg.push_str(&format!(" Skipped: {}.", errors.join("; ")));
            }
            status.set(msg);
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
            transform: engine::AnimatedTransform::default(),
            grade: engine::Grade::default(),
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
                        transform: engine::AnimatedTransform::default(),
                        grade: engine::Grade::default(),
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

    // Sound under the main track from `at` onto bus `lane` (1=A1, 2=A2). A
    // video dropped here contributes its soundtrack.
    let add_audio_path = move |path: String, at: f64, lane: u8| {
        spawn(async move {
            let name = file_name_of(&path);
            match engine::probe(&path).await {
                Ok((duration, has_audio)) => {
                    if !has_audio {
                        status.set(format!("{name} has no audio stream."));
                        return;
                    }
                    push_undo("");
                    let bus = if lane >= 2 { 2 } else { 1 };
                    audios.write().push(AudioItem {
                        path: path.clone(),
                        name,
                        duration,
                        in_s: 0.0,
                        out_s: duration,
                        at: at.max(0.0),
                        volume: 1.0,
                        vol_end: -1.0,
                        duck: 0.0,
                        fade_in: 0.0,
                        fade_out: 0.0,
                        denoise: 0.0,
                        noise_floor: noise_floor_default(),
                        track_noise: false,
                        compress: 0.0,
                        gate: 0.0,
                        declick: 0.0,
                        treat: "None".into(),
                        lane: bus,
                        wave: String::new(),
                        group: 0,
                    });
                    selected.set(Some(Sel::Aud(audios.read().len() - 1)));
                    let tag = if bus >= 2 { "A2" } else { "A1" };
                    status.set(format!(
                        "Audio on {tag} at {} — mixed under the main track.",
                        fmt_t(at)
                    ));
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

    let add_audio = move |lane: u8| {
        if clips.read().is_empty() {
            return;
        }
        let tag = if lane >= 2 { "A2" } else { "A1" };
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Audio", engine::AUDIO_EXT)
                .add_filter("Video (use its soundtrack)", engine::VIDEO_EXT)
                .add_filter("All files", &["*"])
                .set_title(format!("Add audio ({tag})"))
                .pick_file()
                .await
            else {
                return;
            };
            add_audio_path(f.path().display().to_string(), playhead(), lane);
        });
    };

    let mut add_title = move |_: ()| {
        if clips.read().is_empty() {
            return;
        }
        push_undo("");
        titles.write().push(TitleItem { at: playhead(), ..base_title() });
        let k = titles.read().len() - 1;
        selected.set(Some(Sel::Title(k)));
        rerender_title(k);
        status.set("Text added at the playhead — edit it in the inspector.".to_string());
    };

    let gather_specs = move || -> (Vec<ClipSpec>, Vec<OverlaySpec>, Vec<TitleSpec>, Vec<AudioSpec>) {
        // The mixer folds in once, here: V1 gain scales every clip's own audio;
        // A1/A2 gain scales (or, when muted/soloed-out, drops) each bed. Both
        // preview and export come through this function, so they stay in step.
        let m = mixer();
        let gv1 = m.gain_of(MIX_V1);
        let specs = clips
            .read()
            .iter()
            .map(Clip::spec)
            .map(|mut s| {
                s.volume *= gv1;
                s
            })
            .collect();
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
            .filter_map(|a| {
                let g = m.gain_of(Mixer::lane_track(a.lane));
                // A muted or soloed-out bed drops out of the mix entirely rather
                // than riding along at volume 0 — fewer inputs into amix.
                if g <= 0.0 {
                    return None;
                }
                Some(AudioSpec {
                    path: a.path.clone(),
                    in_s: a.in_s,
                    out_s: a.out_s,
                    at: a.at,
                    volume: a.volume * g,
                    // Keep the "same as start" sentinel; a real end gain scales too.
                    vol_end: if a.vol_end < 0.0 { -1.0 } else { a.vol_end * g },
                    duck: a.duck,
                    fade_in: a.fade_in,
                    fade_out: a.fade_out,
                    denoise: a.denoise,
                    noise_floor: a.noise_floor,
                    track_noise: a.track_noise,
                    compress: a.compress,
                    gate: a.gate,
                    declick: a.declick,
                    treat: a.treat.clone(),
                    lane: a.lane,
                })
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

    // Portrait-reel project settings — persisted in the .morreel file, kept out
    // of the undo stack (see ProjectSettings). Data signal lives here so both
    // save/open below and the settings dialog can reach it.
    let mut settings = use_signal(ProjectSettings::default);
    // Declared up here (not by their dialogs below) so open_project can seed
    // them from a loaded project's settings.
    let mut export_opts = use_signal(engine::ExportOpts::default);
    let mut safe_area = use_signal(|| false);

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
            let res = serde_json::to_string_pretty(&Project { snap: snapshot(), settings: settings() })
                .map_err(|e| e.to_string())
                .and_then(|json| std::fs::write(file.path(), json).map_err(|e| e.to_string()));
            match res {
                Ok(()) => {
                    saved_json.set(serde_json::to_string(&snapshot()).ok());
                    status.set(format!("Saved {}", file.path().display()));
                }
                Err(e) => status.set(format!("Save failed: {e}")),
            }
        });
    };

    let export_otio = move |_: ()| {
        spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("OpenTimelineIO", &["otio"])
                .set_file_name("reel.otio")
                .set_title("Export OpenTimelineIO")
                .save_file()
                .await
            else {
                return;
            };
            // Interchange hand-off to FCP/Resolve/Premiere — clips by path, timed
            // like the reel. Not a project save; .morreel keeps the full edit.
            let doc = snapshot_to_otio(&snapshot(), "MorReel", 30.0);
            match std::fs::write(file.path(), doc) {
                Ok(()) => status.set(format!("Exported timeline to {}", file.path().display())),
                Err(e) => status.set(format!("OTIO export failed: {e}")),
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
                .and_then(|t| serde_json::from_str::<Project>(&t).map_err(|e| e.to_string()));
            let Project { snap, settings: loaded } = match parsed {
                Ok(p) => p,
                Err(e) => {
                    status.set(format!("Could not open {}: {e}", file.file_name()));
                    return;
                }
            };
            // Apply the project's settings: guides preference seeds the overlay,
            // resolution seeds the export size, the rest just persists.
            safe_area.set(loaded.guides);
            export_opts.set(export_opts().with_size(loaded.resolution));
            settings.set(loaded);
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
            saved_json.set(serde_json::to_string(&snap).ok()); // freshly opened = clean
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
        status.set(format!("Removed {n} caption(s) — manual text kept."));
    };

    // Export settings live in a dialog rather than being hardcoded: the format
    // decides whether the reel even carries audio, so it has to be chosen
    // before the save dialog names the file.
    let mut show_export = use_signal(|| false);

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

    // Lift a V1 clip's soundtrack onto A1 and mute the picture — so you can
    // duck, trim or move the audio without touching the video. Same source
    // range and timeline start; if the clip is retimed, the detached bed is
    // still at 1× (A1 has no speed knob). Grouped so they drag together.
    let mut detach_audio = move |_: ()| {
        let Some(Sel::Main(i)) = selected() else {
            status.set("Select a V1 clip to detach its audio onto A1.".to_string());
            return;
        };
        let Some(c) = clips.read().get(i).cloned() else { return };
        if !c.has_audio {
            status.set(format!("{} has no audio stream to detach.", c.name));
            return;
        }
        push_undo("");
        let at = start_of(i);
        let gid = if c.group != 0 {
            c.group
        } else {
            let g = next_group();
            next_group.set(g + 1);
            g
        };
        {
            let mut cl = clips.write();
            cl[i].volume = 0.0;
            cl[i].wave.clear();
            cl[i].group = gid;
        }
        let wave = c.wave.clone();
        let path = c.path.clone();
        audios.write().push(AudioItem {
            path: path.clone(),
            name: format!("{} (audio)", c.name),
            duration: c.duration,
            in_s: c.in_s,
            out_s: c.out_s,
            at,
            volume: if c.volume > 0.0 { c.volume } else { 1.0 },
            vol_end: -1.0,
            duck: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            denoise: 0.0,
            noise_floor: noise_floor_default(),
            track_noise: false,
            compress: 0.0,
            gate: 0.0,
            declick: 0.0,
            treat: "None".into(),
            lane: 1,
            wave: wave.clone(),
            group: gid,
        });
        let k = audios.read().len() - 1;
        selected.set(Some(Sel::Aud(k)));
        // Waveform may still be rendering for a fresh import — fill A1 when it lands.
        if wave.is_empty() {
            spawn(async move {
                if let Ok(uri) = engine::waveform_data_uri(&path).await {
                    for a in audios.write().iter_mut().filter(|a| a.path == path) {
                        if a.wave.is_empty() {
                            a.wave = uri.clone();
                        }
                    }
                }
            });
        }
        let note = if (c.speed - 1.0).abs() > 0.01 {
            format!(
                "Audio detached to A1 at {} — picture muted. Bed is 1× (clip was {:.2}×).",
                fmt_t(at),
                c.speed
            )
        } else {
            format!("Audio detached to A1 at {} — picture muted. Drag either; they're grouped.", fmt_t(at))
        };
        status.set(note);
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
            Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.transform.pose()),
            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.transform.pose()),
            _ => None,
        }
    };
    // Write it back and refresh the monitor, whichever lane it came from.
    let mut set_selected_xf = move |t: engine::Transform| {
        let target = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                cl[i].transform.set_pose(t);
                Some((cl[i].scrub_path(), cl[i].in_s, cl[i].framing.clone(), cl[i].look()))
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                ov[j].transform.set_pose(t);
                Some((ov[j].scrub_path(), ov[j].in_s, ov[j].framing.clone(), ov[j].look()))
            }
            _ => None,
        };
        if let Some((path, at, fr, look)) = target {
            request_preview(path, at, fr, look, engine::Over::default());
        }
    };

    // The selected element's grade, whichever lane it is on. Mirrors
    // selected_xf so the one grade panel drives both V1 and V2.
    let selected_grade = move || match selected() {
        Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.grade),
        Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.grade),
        _ => None,
    };
    let mut set_selected_grade = move |g: engine::Grade| {
        let target = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                cl[i].grade = g;
                Some((cl[i].scrub_path(), cl[i].in_s, cl[i].framing.clone(), cl[i].look()))
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                ov[j].grade = g;
                Some((ov[j].scrub_path(), ov[j].in_s, ov[j].framing.clone(), ov[j].look()))
            }
            _ => None,
        };
        if let Some((path, at, fr, look)) = target {
            request_preview(path, at, fr, look, engine::Over::default());
        }
    };

    // Snap the selected element's placement to the frame — or to the phone-UI
    // safe area when its guides are on — GIMP-style. Reuses the `safe_area`
    // signal as the reference toggle: aligning to the zones you can see is the
    // obvious meaning, and it saves a second control. One handler, six ops.
    let mut align_sel = move |op: engine::Align| {
        let Some(mut t) = selected_xf() else { return };
        push_undo("align");
        t.align_to(op, if safe_area() { engine::AlignBox::SAFE } else { engine::AlignBox::FRAME });
        set_selected_xf(t);
    };

    // Ken Burns: the concrete payoff of the keyframe spine — author an animated
    // zoom on the selected V1 clip as a `scale` curve over its whole source span.
    // Only V1 (full-frame) animates cleanly through the zoompan path; that's why
    // this is offered on clips, not overlays. See engine::AnimatedTransform::chain.
    let mut set_ken_burns = move |on: bool| {
        let Some(Sel::Main(i)) = selected() else { return };
        if i >= clips.read().len() {
            return;
        }
        push_undo("ken-burns");
        {
            let mut cl = clips.write();
            cl[i].transform.scale = if on {
                let dur = (cl[i].out_s - cl[i].in_s).max(0.1);
                keyframe::Animated::curve(vec![
                    keyframe::Key { t: 0.0, v: 1.0, interp: keyframe::Interp::Smooth },
                    keyframe::Key { t: dur, v: 1.25, interp: keyframe::Interp::Smooth },
                ])
            } else {
                keyframe::Animated::Const(1.0)
            };
        }
        seek_to(playhead()); // rebuilds the preview from the new look
        status.set(if on {
            "Ken Burns zoom added — a slow push over the whole clip. Scrub to see it.".to_string()
        } else {
            "Ken Burns zoom removed.".to_string()
        });
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

    // The Grade panel — a light primary colour correction that rides the same
    // look() chain as the effect presets, so it is WYSIWYG. One definition
    // serves V1 and V2: both write through the selection.
    let grade_panel = move || {
        let Some(g) = selected_grade() else {
            return rsx! {};
        };
        rsx! {
            h4 { class: "mr-fx-cat", "Grade" }
            for (label, value, min, max, step, set) in grade_knobs(&g) {
                Slider {
                    key: "{label}",
                    label: Some(label),
                    min, max, step,
                    precision: if step < 1.0 { 2 } else { 0 },
                    value,
                    oninput: Some(EventHandler::new(move |v: f64| {
                        push_undo(&format!("grade-{label}"));
                        // Read live, not the render-captured grade, so a drag
                        // never writes back a stale sibling knob.
                        if let Some(mut g) = selected_grade() {
                            set(&mut g, v);
                            set_selected_grade(g);
                        }
                    })),
                }
            }
        }
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
            // Only a non-uniform box (a stretch or a band) has an aspect to keep,
            // so the fit choice appears exactly when it does something.
            if (xf.scale_x - xf.scale_y).abs() > 1e-6 {
                MorCheckbox {
                    label: "Keep aspect (crop to fill instead of stretch)".to_string(),
                    checked: xf.cover,
                    onchange: move |on: bool| {
                        push_undo("xf-cover");
                        if let Some(mut t) = selected_xf() {
                            t.cover = on;
                            set_selected_xf(t);
                        }
                    },
                }
            }
            MorSelect {
                label: "Mirror".to_string(),
                value: match (xf.flip_h, xf.flip_v) {
                    (true, true) => "Both".to_string(),
                    (true, false) => "Across".to_string(),
                    (false, true) => "Down".to_string(),
                    _ => "None".to_string(),
                },
                options: ["None", "Across", "Down", "Both"].map(str::to_string).to_vec(),
                onchange: move |v: String| {
                    push_undo("");
                    if let Some(mut t) = selected_xf() {
                        t.flip_h = v == "Across" || v == "Both";
                        t.flip_v = v == "Down" || v == "Both";
                        set_selected_xf(t);
                    }
                },
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

    // Analyse the music bed and fill the marker grid automatically — the tap-M
    // workflow, minus the tapping. Target is the selected A-item, else the first
    // bed on A1. Beats become snap targets like any marker, so items drawn or
    // dragged afterwards land on the beat.
    let mut analyze_beats = move |_: ()| {
        let target = match selected() {
            Some(Sel::Aud(k)) => audios.read().get(k).cloned(),
            _ => audios.read().iter().find(|a| a.lane == 1).cloned(),
        };
        let Some(a) = target else {
            status.set("Add a music bed on A1 (or select an audio item) first.".into());
            return;
        };
        status.set("Analysing music for beats…".into());
        spawn(async move {
            match engine::detect_beats(&a.path, a.in_s, a.out_s).await {
                Ok((bpm, src_beats)) => {
                    // Source seconds → timeline seconds, clipped to the item's span.
                    let span_end = a.at + (a.out_s - a.in_s);
                    let mut m: Vec<f64> = src_beats
                        .into_iter()
                        .map(|s| a.at + (s - a.in_s))
                        .filter(|&t| t >= a.at - 1e-6 && t <= span_end + 1e-6)
                        .collect();
                    if m.is_empty() {
                        status.set("No beats found in that clip.".into());
                        return;
                    }
                    push_undo("");
                    m.sort_by(f64::total_cmp);
                    let n = m.len();
                    markers.set(m);
                    status.set(format!(
                        "{n} beat markers at ~{bpm:.0} BPM — drag items to snap to them, Shift+M clears."
                    ));
                }
                Err(e) => status.set(format!("Beat detection failed: {e}")),
            }
        });
    };

    // Safe-area guides over the monitor — the portrait editor's ruler for
    // "will this caption survive the app's own buttons". (Signal declared above,
    // near the project settings that seed it.)
    let mut toggle_safe = move |_: ()| {
        safe_area.toggle();
        status.set(if safe_area() {
            "Safe areas on — keep text out of the shaded bands.".to_string()
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
    use_shortcut(Some("B".into()), Some(EventHandler::new(move |()| analyze_beats(()))));
    // The menu item binds "~"; this covers layouts where ~ is Shift+` and the
    // combo therefore arrives as SHIFT+~.
    use_shortcut(Some("SHIFT+~".into()), Some(EventHandler::new(move |()| toggle_magnet(()))));

    // Window chrome preference (frameless / native / tiling), persisted like
    // the blogger theme editor; takes effect on next launch.
    let active_mode = UiMode::active();
    let mut preferred_mode = use_signal(|| UiMode::load_preference().unwrap_or(active_mode));
    let mut show_about = use_signal(|| false);
    let mut show_shortcuts = use_signal(|| false);
    let mut show_settings = use_signal(|| false);
    let mut settings_tab = use_signal(|| "Format".to_string());
    // Editing-key scheme (persisted app-wide) + its picker. Reactive: the Split
    // menu item below reads key_scheme().split(), and MorMenuItem rebinds the key
    // when that prop changes, so switching schemes remaps the blade live.
    let mut key_scheme = use_signal(load_keyscheme);
    let mut show_keys = use_signal(|| false);
    // The active workflow phase — the inspector reconfigures to it, the bottom bar
    // lights it up. Selecting a timeline item jumps to its phase; `.peek()` keeps
    // that from re-firing when the phase itself changes (a bar click), so the two
    // never fight in a loop.
    let mut active_phase = use_signal(|| Phase::Cut);
    use_effect(move || {
        let target = phase_for_selection(selected(), *active_phase.peek());
        if *active_phase.peek() != target {
            active_phase.set(target);
        }
    });
    // Sub-tabs inside the Style phase — the dense one — split into Look (grade +
    // framing) and Transform (position/scale/rotate + Ken Burns) so neither view
    // is a long scroll.
    let mut style_tab = use_signal(|| "Look".to_string());
    // Ctrl+, — the platform-conventional "preferences" shortcut.
    use_shortcut(Some("Ctrl+,".into()), Some(EventHandler::new(move |()| show_settings.set(true))));
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
                        Lane::A1 | Lane::A2 => add_audio_path(path, at, lane_num(lane)),
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
    // On-lane audio handles. Fade: (audio index, is_out, grab x, fade at grab).
    // Volume: (audio index, grab y, volume at grab). Both are separate from the
    // move-drag above so grabbing a corner shapes the clip instead of sliding it.
    let mut fade_drag = use_signal(|| Option::<(usize, bool, f64, f64)>::None);
    let mut vol_drag = use_signal(|| Option::<(usize, f64, f64)>::None);
    // Title length: (title index, grab x, dur at grab). Separate from the move-drag
    // so grabbing the right edge stretches the title instead of sliding it.
    let mut len_drag = use_signal(|| Option::<(usize, f64, f64)>::None);
    // Ruler scrub: mousedown on the ruler seeks and keeps seeking while held.
    let mut scrubbing = use_signal(|| false);

    // Inspector chrome: docked in the work row, floated as an in-app panel, or
    // hidden so the monitor + timeline take the full width.
    let mut insp_open = use_signal(|| true);
    let mut insp_float = use_signal(|| false);
    // Floated panel geometry (logical px). Set when the panel undocks so move
    // and resize share one coordinate system instead of fighting CSS defaults.
    let mut float_xy = use_signal(|| Option::<(f64, f64)>::None);
    let mut float_size = use_signal(|| Option::<(f64, f64)>::None);
    // Active float interaction: which grip, pointer origin, panel origin + size.
    let mut float_drag = use_signal(|| Option::<(FloatGrab, f64, f64, f64, f64, f64, f64)>::None);
    let mut show_effects = use_signal(|| false);
    let mut show_add = use_signal(|| false);

    // Pin the floated inspector to a right-side default that matches the old CSS.
    let mut pin_float_geom = move || -> (f64, f64, f64, f64) {
        if let (Some((x, y)), Some((w, h))) = (float_xy(), float_size()) {
            return (x, y, w, h);
        }
        let (x, y, w, h) = float_default_geom();
        float_xy.set(Some((x, y)));
        float_size.set(Some((w, h)));
        (x, y, w, h)
    };
    let mut begin_float = move |grab: FloatGrab, mx: f64, my: f64| {
        let (x, y, w, h) = pin_float_geom();
        float_drag.set(Some((grab, mx, my, x, y, w, h)));
    };
    let mut clear_float_geom = move || {
        float_xy.set(None);
        float_size.set(None);
        float_drag.set(None);
    };

    // Workspace: named layouts (Inspector dock state) + fullscreen. A layout is
    // (de)serialized panel state, nothing more.
    let mut layouts = use_signal(load_layouts);
    let mut show_save_layout = use_signal(|| false);
    let mut layout_name = use_signal(String::new);
    let mut is_fullscreen = use_signal(|| false);

    let mut apply_layout = move |l: Layout| {
        insp_open.set(l.inspector_open);
        insp_float.set(l.inspector_float);
        float_xy.set(l.float_xy);
        float_size.set(l.float_size);
        // Floating with no saved geometry needs the default pinned, mirroring the
        // "Float inspector" action; docking clears it the same way.
        if l.inspector_float {
            if l.float_xy.is_none() {
                let _ = pin_float_geom();
            }
        } else {
            clear_float_geom();
        }
        status.set(format!("Layout \"{}\" applied.", l.name));
    };

    let mut store_layout = move |_: ()| {
        let name = layout_name().trim().to_string();
        if name.is_empty() {
            return;
        }
        let l = Layout {
            name: name.clone(),
            inspector_open: insp_open(),
            inspector_float: insp_float(),
            float_xy: float_xy(),
            float_size: float_size(),
        };
        {
            let mut all = layouts.write();
            // Same as title presets: a name typed on purpose replaces its match.
            all.retain(|x| x.name != name);
            all.push(l);
            all.sort_by_key(|x| x.name.to_lowercase());
        }
        match save_layouts(&layouts.read()) {
            Ok(()) => status.set(format!("Saved layout \"{name}\".")),
            Err(e) => status.set(format!("Could not save layout: {e}")),
        }
        show_save_layout.set(false);
        layout_name.set(String::new());
    };

    let mut toggle_fullscreen = move |_: ()| {
        is_fullscreen.toggle();
        dioxus::desktop::window().set_fullscreen(is_fullscreen());
        status.set(if is_fullscreen() {
            "Fullscreen — F11 or the View menu to exit.".to_string()
        } else {
            "Windowed.".to_string()
        });
    };
    use_shortcut(Some("F11".into()), Some(EventHandler::new(move |()| toggle_fullscreen(()))));

    // Effects browser thumbnails: the selected item's poster frame through
    // every effect, generated lazily and cached until the frame changes.
    let mut fx_thumbs = use_signal(std::collections::HashMap::<String, String>::new);
    let mut fx_key = use_signal(String::new);
    use_effect(move || {
        if !show_effects() {
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
            // Show the project's own name once set — the standard "which document
            // am I editing" affordance — else the format.
            subtitle: Some(if settings().title.trim().is_empty() {
                "portrait 9:16".to_string()
            } else {
                format!("{} · 9:16", settings().title.trim())
            }),
            app_name: "MorReel Studio".to_string(),
            system_icon: Some(system_icon_src()),
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
                    MenuItem {
                        label: "Project settings…".to_string(),
                        on_action: move |_| show_settings.set(true),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Add to reel…".to_string(),
                        disabled: exporting,
                        on_action: move |_| show_add.set(true),
                    }
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
                        on_action: move |_| add_audio(1),
                    }
                    MenuItem {
                        label: "Add audio (A2)…".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_audio(2),
                    }
                    MenuItem {
                        label: "Add text (T)".to_string(),
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
                    MenuItem {
                        label: "Export OpenTimelineIO…".to_string(),
                        disabled: no_clips,
                        on_action: move |_| export_otio(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Quit".to_string(),
                        shortcut: Some("Ctrl+Q".to_string()),
                        on_action: move |_| {
                            // Guard the in-app quit path against losing unsaved work.
                            // (The window's own close button can't be intercepted
                            // cleanly in this Dioxus version — the ● Edited chip is
                            // the always-visible backstop there.)
                            if is_dirty() {
                                spawn(async move {
                                    let r = rfd::AsyncMessageDialog::new()
                                        .set_title("Unsaved changes")
                                        .set_description("This reel has unsaved changes. Quit without saving?")
                                        .set_buttons(rfd::MessageButtons::YesNo)
                                        .show()
                                        .await;
                                    if r == rfd::MessageDialogResult::Yes {
                                        dioxus::desktop::window().close();
                                    }
                                });
                            } else {
                                dioxus::desktop::window().close();
                            }
                        },
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
                        shortcut: Some(key_scheme().split().to_string()),
                        disabled: no_clips,
                        on_action: move |_| split_at_playhead(()),
                    }
                    MenuItem {
                        label: "Auto-cut silence…".to_string(),
                        disabled: no_clips || autocut_busy(),
                        on_action: move |_| show_autocut.set(true),
                    }
                    MenuItem {
                        label: "Ripple delete".to_string(),
                        shortcut: Some("Delete".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| delete_sel(()),
                    }
                    MenuItem {
                        label: "Detach audio to A1".to_string(),
                        shortcut: Some("Ctrl+U".to_string()),
                        disabled: !matches!(
                            selected(),
                            Some(Sel::Main(i)) if clips.read().get(i).is_some_and(|c| c.has_audio)
                        ),
                        on_action: move |_| detach_audio(()),
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
                        label: "Detect beats from music".to_string(),
                        shortcut: Some("B".to_string()),
                        disabled: audios.read().is_empty(),
                        on_action: move |_| analyze_beats(()),
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
                        label: format!("{} Fullscreen", if is_fullscreen() { "●" } else { "○" }),
                        shortcut: Some("F11".to_string()),
                        on_action: move |_| toggle_fullscreen(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Pop out monitor".to_string(),
                        disabled: monitor_out(),
                        on_action: move |_| open_monitor(),
                    }
                    MenuItem {
                        label: if insp_float() {
                            "Dock inspector".to_string()
                        } else {
                            "Float inspector".to_string()
                        },
                        disabled: !insp_open(),
                        on_action: move |_| {
                            if !insp_open() {
                                insp_open.set(true);
                            }
                            if insp_float() {
                                insp_float.set(false);
                                clear_float_geom();
                                status.set("Inspector docked beside the monitor.".to_string());
                            } else {
                                insp_float.set(true);
                                let _ = pin_float_geom();
                                status.set(
                                    "Inspector floating — drag the title bar to move, edges/corners to resize."
                                        .to_string(),
                                );
                            }
                        },
                    }
                    MenuItem {
                        label: if insp_open() {
                            "Hide inspector".to_string()
                        } else {
                            "Show inspector".to_string()
                        },
                        on_action: move |_| {
                            insp_open.toggle();
                            if insp_open() {
                                status.set("Inspector shown.".to_string());
                            } else {
                                status.set("Inspector hidden — View › Show inspector to bring it back.".to_string());
                            }
                        },
                    }
                    MenuItem {
                        label: "Effects palette…".to_string(),
                        on_action: move |_| show_effects.set(true),
                    }
                    MenuItem {
                        label: "Add to reel…".to_string(),
                        disabled: exporting,
                        on_action: move |_| show_add.set(true),
                    }
                    MenuSeparator {}
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
                    // Layouts — built at runtime from presets + saved arrangements,
                    // sorted. When a second dockable panel appears, generate a
                    // checkable toggle per panel here the same way (that is the
                    // whole "runtime panel list" trick — deferred until then).
                    for l in preset_layouts().into_iter().chain(layouts()) {
                        MenuItem {
                            label: format!("Layout: {}", l.name),
                            on_action: move |_| apply_layout(l.clone()),
                        }
                    }
                    MenuItem {
                        label: "Save current layout…".to_string(),
                        on_action: move |_| show_save_layout.set(true),
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
                        label: "Keyboard layout…".to_string(),
                        on_action: move |_| show_keys.set(true),
                    }
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
                if !insp_open() {
                    span {
                        class: "mor-statusbar-chip mor-statusbar-warn mr-status-click",
                        title: "Click to show inspector",
                        onclick: move |_| insp_open.set(true),
                        "inspector hidden"
                    }
                } else if insp_float() {
                    span { class: "mor-statusbar-chip", "inspector floating" }
                }
                if !marked().is_empty() {
                    span { class: "mor-statusbar-chip", "{marked().len()} marked · Ctrl+G groups" }
                }
                if !magnet() {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "magnet off" }
                }
                if preferred_mode() != active_mode {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "window mode: restart to apply" }
                }
                if let Some(warn) = over_limits(total, &settings().platform) {
                    span {
                        class: "mor-statusbar-chip mor-statusbar-warn",
                        title: "Longer than this platform accepts for a portrait upload",
                        "{warn}"
                    }
                }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "{fmt_t(total)} total" }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "{engine::size_label(settings().resolution)} • 30 fps" }
                if is_dirty() {
                    span {
                        class: "mor-statusbar-chip mor-statusbar-warn",
                        title: "Unsaved changes — Ctrl+S to save",
                        "● Edited"
                    }
                } else if saved_json().is_some() {
                    span { class: "mor-statusbar-chip mor-statusbar-muted", "✓ Saved" }
                }
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
                onmouseup: move |_| {
                    undo_tag.set(String::new());
                    float_drag.set(None);
                },
                onmousemove: move |evt| {
                    let Some((grab, mx, my, ox, oy, ow, oh)) = float_drag() else { return };
                    let p = evt.client_coordinates();
                    let (x, y, w, h) =
                        float_apply(grab, (ox, oy, ow, oh), (mx, my), (p.x, p.y));
                    float_xy.set(Some((x, y)));
                    float_size.set(Some((w, h)));
                },
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
                                        let (bw, bh) =
                                            (xf.scale * xf.scale_x * 100.0, xf.scale * xf.scale_y * 100.0);
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
                                                    let snap = evt.modifiers().shift();
                                                    set_selected_xf(xf_apply(grab, start, from, (p.x, p.y), rect, snap));
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
                                                // Sides stretch one axis; corners keep the shape.
                                                for (n, (fx, fy)) in xf_edges(&xf).into_iter().enumerate() {
                                                    div {
                                                        key: "e{n}",
                                                        class: if n < 2 { "mr-xf-e wide" } else { "mr-xf-e tall" },
                                                        title: "Drag to stretch this way only",
                                                        style: "left:{clamp(fx)}%;top:{clamp(fy)}%",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            let p = evt.client_coordinates();
                                                            let grab = if n < 2 { XfGrab::StretchX } else { XfGrab::StretchY };
                                                            begin_xf(grab, (p.x, p.y));
                                                        },
                                                    }
                                                }
                                                div {
                                                    class: "mr-xf-rot",
                                                    style: "left:{clamp(50.0 + xf.x * 100.0)}%;top:{clamp((50.0 + xf.y * 100.0 - bh / 2.0) - 4.0)}%",
                                                    title: "Drag to rotate \u{2014} hold Shift to snap to 15 degrees",
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

                    if !insp_open() {
                        button {
                            class: "mr-insp-reopen",
                            title: "Show the inspector panel",
                            onclick: move |_| insp_open.set(true),
                            "Inspector ›"
                        }
                    }

                    if insp_open() {
                    div {
                        class: if insp_float() { "mr-inspector mr-float-panel" } else { "mr-inspector" },
                        style: {
                            if insp_float() {
                                match (float_xy(), float_size()) {
                                    (Some((x, y)), Some((w, h))) => format!(
                                        "left:{x:.0}px;top:{y:.0}px;width:{w:.0}px;height:{h:.0}px;right:auto;"
                                    ),
                                    _ => String::new(),
                                }
                            } else {
                                String::new()
                            }
                        },
                        div {
                            class: "mr-panel-head",
                            onmousedown: move |evt| {
                                if !insp_float() { return; }
                                if evt.trigger_button() != Some(dioxus::html::input_data::MouseButton::Primary) {
                                    return;
                                }
                                let p = evt.client_coordinates();
                                begin_float(FloatGrab::Move, p.x, p.y);
                            },
                            span { class: "mr-panel-title",
                                if insp_float() { "Inspector · floating" } else { "Inspector" }
                            }
                            div { class: "mr-panel-tools",
                                button {
                                    class: "mr-panel-btn",
                                    title: "Effects palette",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        show_effects.set(true);
                                    },
                                    "✦"
                                }
                                button {
                                    class: "mr-panel-btn",
                                    title: if insp_float() { "Dock inspector" } else { "Float inspector" },
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        if insp_float() {
                                            insp_float.set(false);
                                            clear_float_geom();
                                        } else {
                                            insp_float.set(true);
                                            let _ = pin_float_geom();
                                        }
                                    },
                                    if insp_float() { "⬚" } else { "⧉" }
                                }
                                button {
                                    class: "mr-panel-btn",
                                    title: "Hide inspector",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        insp_open.set(false);
                                        float_drag.set(None);
                                    },
                                    "—"
                                }
                            }
                        }
                        if insp_float() {
                            // Edge + corner grips so the floated sheet is resizable
                            // the same way a desktop dialog would be.
                            for (cls, grab, title) in [
                                ("n", FloatGrab::N, "Resize top"),
                                ("s", FloatGrab::S, "Resize bottom"),
                                ("e", FloatGrab::E, "Resize right"),
                                ("w", FloatGrab::W, "Resize left"),
                                ("ne", FloatGrab::Ne, "Resize top-right"),
                                ("nw", FloatGrab::Nw, "Resize top-left"),
                                ("se", FloatGrab::Se, "Resize bottom-right"),
                                ("sw", FloatGrab::Sw, "Resize bottom-left"),
                            ] {
                                div {
                                    key: "{cls}",
                                    class: "mr-float-grip mr-float-grip-{cls}",
                                    title: "{title}",
                                    onmousedown: move |evt| {
                                        evt.stop_propagation();
                                        if evt.trigger_button()
                                            != Some(dioxus::html::input_data::MouseButton::Primary)
                                        {
                                            return;
                                        }
                                        let p = evt.client_coordinates();
                                        begin_float(grab, p.x, p.y);
                                    },
                                }
                            }
                        }
                        div { class: "mr-toolbar",
                            button {
                                class: "mor-btn primary",
                                disabled: exporting,
                                title: "Import clips, overlays, audio or text",
                                onclick: move |_| show_add.set(true),
                                "＋ Add…"
                            }
                            button {
                                class: "mor-btn",
                                title: "Browse the effects for this workspace",
                                onclick: move |_| show_effects.set(true),
                                {match active_phase() {
                                    Phase::Cut => "⇄ Transitions",
                                    Phase::Text => "✦ Text FX",
                                    Phase::Audio => "♪ Audio FX",
                                    Phase::Background => "▧ Backgrounds",
                                    _ => "✦ Effects",
                                }}
                            }
                            button {
                                class: "mor-btn mr-export",
                                disabled: clips.read().is_empty() || exporting,
                                onclick: move |_| do_export(()),
                                "⇪ Export"
                            }
                        }

                        if let Some(p) = export_progress() {
                            div { class: "mr-progress",
                                div { style: "width: {p * 100.0:.1}%" }
                            }
                        }

                        h4 { class: "mr-phase-head", "{active_phase().label()}" }
                        if active_phase() == Phase::Add {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "Main clips go on V1, cutaways on V2, and music or voiceover underneath."
                            }
                            div { class: "mr-phase-actions",
                                button { class: "mor-btn primary", disabled: exporting, onclick: move |_| import_clips(()), "＋ Add clips (V1)" }
                                button { class: "mor-btn", disabled: exporting, onclick: move |_| add_overlay(()), "⧉ Add b-roll (V2)" }
                                button { class: "mor-btn", onclick: move |_| add_audio(1), "♪ Add music (A1)" }
                                button { class: "mor-btn", onclick: move |_| add_audio(2), "🎙 Add voiceover (A2)" }
                                button { class: "mor-btn", onclick: move |_| add_title(()), "T Add text / caption" }
                                button { class: "mor-btn", onclick: move |_| show_add.set(true), "… More options" }
                            }
                        } else if active_phase() == Phase::Export {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "{fmt_t(total)} · {engine::size_label(settings().resolution)} · 30 fps"
                            }
                            if let Some(warn) = over_limits(total, &settings().platform) {
                                p { class: "mor-statusbar-muted mr-export-blurb", "⚠ over {warn}" }
                            }
                            div { class: "mr-phase-actions",
                                button { class: "mor-btn primary mr-export", disabled: clips.read().is_empty() || exporting, onclick: move |_| do_export(()), "⇪ Export MP4…" }
                                button { class: "mor-btn", disabled: clips.read().is_empty(), onclick: move |_| export_otio(()), "⇄ Export OpenTimelineIO…" }
                            }
                        } else if active_phase() == Phase::Background {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "The colour behind the picture wherever it doesn't fill 9:16 — a banded or shrunk clip. Pair it with a band preset in Style › Transform, then add text above or below."
                            }
                            div { class: "mr-bg-swatches",
                                for b in engine::Bg::ALL {
                                    button {
                                        class: if clips.read().first().is_some_and(|c| c.transform.bg == b) { "mr-bg-swatch active" } else { "mr-bg-swatch" },
                                        style: "background: {b.color()};",
                                        title: "{b.label()}",
                                        onclick: move |_| {
                                            if clips.read().is_empty() { return; }
                                            push_undo("bg");
                                            for c in clips.write().iter_mut() { c.transform.bg = b; }
                                            seek_to(playhead());
                                            status.set(format!("Background: {} — shows behind a banded clip.", b.label()));
                                        },
                                        span { class: "mr-bg-name", "{b.label()}" }
                                    }
                                }
                            }
                        } else {
                        // Track mixer: shown for the whole Audio phase, not tied to
                        // a selection. Level, mute and solo per bus, plus a master.
                        {if active_phase() == Phase::Audio {
                            let mx = mixer();
                            rsx! {
                                h4 { class: "mr-fx-cat", "Mixer" }
                                div { class: "mr-mixer",
                                    for i in 0..3usize {
                                        div { key: "{i}", class: "mr-mixer-row",
                                            span { class: "mr-mixer-tag", "{MIX_LABELS[i]}" }
                                            button {
                                                class: if mx.tracks[i].mute { "mr-mixer-btn on m" } else { "mr-mixer-btn m" },
                                                title: "Mute this track",
                                                onclick: move |_| {
                                                    push_undo("");
                                                    let on = mixer.read().tracks[i].mute;
                                                    mixer.write().tracks[i].mute = !on;
                                                },
                                                "M"
                                            }
                                            button {
                                                class: if mx.tracks[i].solo { "mr-mixer-btn on s" } else { "mr-mixer-btn s" },
                                                title: "Solo — hear only the soloed tracks",
                                                onclick: move |_| {
                                                    push_undo("");
                                                    let on = mixer.read().tracks[i].solo;
                                                    mixer.write().tracks[i].solo = !on;
                                                },
                                                "S"
                                            }
                                            input {
                                                class: "mr-mixer-fader",
                                                r#type: "range", min: "0", max: "2", step: "0.05",
                                                value: "{mx.tracks[i].gain}",
                                                onkeydown: move |evt| evt.stop_propagation(),
                                                oninput: move |evt| {
                                                    if let Ok(v) = evt.value().parse::<f64>() {
                                                        push_undo(&format!("mixg{i}"));
                                                        mixer.write().tracks[i].gain = v;
                                                    }
                                                },
                                            }
                                            span { class: "mr-mixer-val", "{(mx.tracks[i].gain * 100.0) as i32}%" }
                                        }
                                    }
                                    div { class: "mr-mixer-row",
                                        span { class: "mr-mixer-tag", "Mst" }
                                        input {
                                            class: "mr-mixer-fader",
                                            r#type: "range", min: "0", max: "2", step: "0.05",
                                            value: "{mx.master}",
                                            onkeydown: move |evt| evt.stop_propagation(),
                                            oninput: move |evt| {
                                                if let Ok(v) = evt.value().parse::<f64>() {
                                                    push_undo("mixmaster");
                                                    mixer.write().master = v;
                                                }
                                            },
                                        }
                                        span { class: "mr-mixer-val", "{(mx.master * 100.0) as i32}%" }
                                    }
                                }
                                p { class: "mor-statusbar-muted mr-export-blurb",
                                    "V1 is every clip's own sound; A1/A2 are the beds. Solo one to audition it. Applies to preview and export alike."
                                }
                            }
                        } else {
                            rsx! {}
                        }}
                        {match selected() {
                            Some(Sel::Main(i)) if i < clips.read().len() && active_phase() != Phase::Text => {
                                let c = clips.read()[i].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag", "V1" }
                                            " {c.name}"
                                        }
                                        p { class: "mor-statusbar-muted", "{clip_note(&c)}" }
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Trim with I / O at the playhead, or drag the clip edges on the timeline."
                                        }
                                    }
                                    if active_phase() == Phase::Style {
                                    MorTabs {
                                        tabs: vec!["Look".to_string(), "Transform".to_string()],
                                        active: style_tab(),
                                        onchange: move |t: String| style_tab.set(t),
                                    }
                                    if style_tab() == "Look" {
                                    div { class: "mr-field-row",
                                        div { class: "mr-field-grow",
                                            MorSelect {
                                                label: "Effect".to_string(),
                                                value: c.effect.clone(),
                                                options: effect_names.clone(),
                                                onchange: {
                                                    let path = c.scrub_path();
                                                    let fr = c.framing.clone();
                                                    let amt = c.effect_amount;
                                                    move |name: String| {
                                                        push_undo("fx-pick");
                                                        let t = {
                                                            let mut cl = clips.write();
                                                            cl[i].effect = name.clone();
                                                            cl[i].in_s
                                                        };
                                                        request_preview(path.clone(), t, fr.clone(), effect_filter_amt(&name, amt), engine::Over::default());
                                                    }
                                                },
                                            }
                                        }
                                        button {
                                            class: "mor-btn mr-field-side",
                                            title: "Open the effects palette with thumbnails",
                                            onclick: move |_| show_effects.set(true),
                                            "✦ Browse…"
                                        }
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
                                    {grade_panel()}
                                    MorSelect {
                                        label: "Framing (9:16)".to_string(),
                                        value: c.framing.clone(),
                                        options: FRAMINGS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: {
                                            let path = c.scrub_path();
                                            let eff = c.look();
                                            move |name: String| {
                                                push_undo("framing");
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].framing = name.clone();
                                                    cl[i].in_s
                                                };
                                                request_preview(path.clone(), t, name, eff.clone(), engine::Over::default());
                                            }
                                        },
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb", "{framing_hint(&c.framing)}" }
                                    }
                                    }
                                    if active_phase() == Phase::Cut {
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
                                        button {
                                            class: "mor-btn mr-reset",
                                            title: "Copy this clip's soundtrack onto A1 and mute the picture, so you can edit them apart",
                                            onclick: move |_| detach_audio(()),
                                            "⤴ Detach audio to A1"
                                        }
                                        if c.volume <= 0.0 {
                                            p { class: "mor-statusbar-muted mr-export-blurb",
                                                "Picture is silent — raise Clip volume to hear it again, or edit the bed on A1."
                                            }
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
                                    }
                                    if active_phase() == Phase::Style {
                                    if style_tab() == "Transform" {
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Band presets fit the clip into a landscape strip — pick a Background for the surround, then add text above or below."
                                    }
                                    div { class: "mr-preset-row",
                                        button { class: "mor-btn", title: "Fill the whole 9:16 frame",
                                            onclick: move |_| {
                                                push_undo("band");
                                                let mut t = selected_xf().unwrap_or_default();
                                                t.scale = 1.0; t.scale_x = 1.0; t.scale_y = 1.0; t.x = 0.0; t.y = 0.0; t.rotation = 0.0; t.cover = false;
                                                set_selected_xf(t);
                                                status.set("Filling the frame.".to_string());
                                            }, "▢ Fill" }
                                        button { class: "mor-btn", title: "Landscape band near the top — text below",
                                            onclick: move |_| {
                                                push_undo("band");
                                                let mut t = selected_xf().unwrap_or_default();
                                                t.scale = 1.0; t.scale_x = 1.0; t.scale_y = 0.34; t.x = 0.0; t.y = -0.22; t.rotation = 0.0; t.cover = true;
                                                set_selected_xf(t);
                                                status.set("Band set — pick a Background (Bg) for the surround, then add Text below.".to_string());
                                            }, "▭ Band ↑" }
                                        button { class: "mor-btn", title: "Landscape band, centered",
                                            onclick: move |_| {
                                                push_undo("band");
                                                let mut t = selected_xf().unwrap_or_default();
                                                t.scale = 1.0; t.scale_x = 1.0; t.scale_y = 0.34; t.x = 0.0; t.y = 0.0; t.rotation = 0.0; t.cover = true;
                                                set_selected_xf(t);
                                                status.set("Band set — pick a Background (Bg) for the surround, then add Text above or below.".to_string());
                                            }, "▭ Band" }
                                        button { class: "mor-btn", title: "Landscape band near the bottom — text above",
                                            onclick: move |_| {
                                                push_undo("band");
                                                let mut t = selected_xf().unwrap_or_default();
                                                t.scale = 1.0; t.scale_x = 1.0; t.scale_y = 0.34; t.x = 0.0; t.y = 0.22; t.rotation = 0.0; t.cover = true;
                                                set_selected_xf(t);
                                                status.set("Band set — pick a Background (Bg) for the surround, then add Text above.".to_string());
                                            }, "▭ Band ↓" }
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        {if safe_area() {
                                            "Align snaps the clip's box to the safe area (guides on) — computed from its real height, so a band of any size lands flush."
                                        } else {
                                            "Align snaps the clip's box to the frame — press G for safe-area guides to align to those instead. Works at any band height."
                                        }}
                                    }
                                    div { class: "mr-preset-row",
                                        button { class: "mor-btn", title: "Align top edge",
                                            onclick: move |_| align_sel(engine::Align::Top), "⤒ Top" }
                                        button { class: "mor-btn", title: "Centre vertically",
                                            onclick: move |_| align_sel(engine::Align::VCenter), "⇕ Middle" }
                                        button { class: "mor-btn", title: "Align bottom edge",
                                            onclick: move |_| align_sel(engine::Align::Bottom), "⤓ Bottom" }
                                    }
                                    div { class: "mr-preset-row",
                                        button { class: "mor-btn", title: "Align left edge",
                                            onclick: move |_| align_sel(engine::Align::Left), "⇤ Left" }
                                        button { class: "mor-btn", title: "Centre horizontally",
                                            onclick: move |_| align_sel(engine::Align::HCenter), "⇔ Centre" }
                                        button { class: "mor-btn", title: "Align right edge",
                                            onclick: move |_| align_sel(engine::Align::Right), "⇥ Right" }
                                    }
                                    {transform_panel(false)}
                                    MorCheckbox {
                                        label: "Ken Burns zoom (animated push over the whole clip)".to_string(),
                                        checked: c.transform.scale.is_animated(),
                                        onchange: move |on: bool| set_ken_burns(on),
                                    }
                                    }
                                    }
                                    if active_phase() == Phase::Cut {
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn", onclick: move |_| move_sel(-1), "◀ Move left" }
                                        button { class: "mor-btn", onclick: move |_| move_sel(1), "Move right ▶" }
                                        button { class: "mor-btn", onclick: move |_| split_at_playhead(()), "✂ Split at playhead" }
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Ripple delete" }
                                    }
                                    }
                                }
                            }
                            Some(Sel::Over(j)) if j < overlays.read().len() && active_phase() != Phase::Text => {
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
                                    div { class: "mr-field-row",
                                        div { class: "mr-field-grow",
                                            MorSelect {
                                                label: "Effect".to_string(),
                                                value: o.effect.clone(),
                                                options: effect_names.clone(),
                                                onchange: {
                                                    let path = o.scrub_path();
                                                    let fr = o.framing.clone();
                                                    let amt = o.effect_amount;
                                                    move |name: String| {
                                                        push_undo("fx-pick");
                                                        let t = {
                                                            let mut ov = overlays.write();
                                                            ov[j].effect = name.clone();
                                                            ov[j].in_s
                                                        };
                                                        request_preview(path.clone(), t, fr.clone(), effect_filter_amt(&name, amt), engine::Over::default());
                                                    }
                                                },
                                            }
                                        }
                                        button {
                                            class: "mor-btn mr-field-side",
                                            title: "Open the effects palette with thumbnails",
                                            onclick: move |_| show_effects.set(true),
                                            "✦ Browse…"
                                        }
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
                                    {grade_panel()}
                                    MorSelect {
                                        label: "Framing (9:16)".to_string(),
                                        value: o.framing.clone(),
                                        options: FRAMINGS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: {
                                            let path = o.scrub_path();
                                            let eff = o.look();
                                            move |name: String| {
                                                push_undo("framing");
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].framing = name.clone();
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, name, eff.clone(), engine::Over::default());
                                            }
                                        },
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb", "{framing_hint(&o.framing)}" }
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
                                            if t.caption { " Caption" } else if t.kind != "Text" { " Shape" } else { " Text" }
                                        }
                                        p { class: "mor-statusbar-muted",
                                            "Shown from {fmt_t(t.at)} for {fmt_t(t.dur)}"
                                            if t.pngs.is_empty() { " • rendering…" }
                                        }
                                    }
                                    div { class: "mr-toolbar mr-text-nav",
                                        button { class: "mor-btn", onclick: move |_| selected.set(None), "← All text" }
                                        button { class: "mor-btn", onclick: move |_| add_title(()), "＋ Add another" }
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
                                    // A real multi-line field: Enter makes a line break,
                                    // no magic "\n" to type. The render is debounced and
                                    // the old card is left on the monitor while typing, so
                                    // the caret never fights an async re-render.
                                    div { class: "mor-input-wrapper",
                                        div { class: "mor-input-label", "Text — Enter for a new line" }
                                        textarea {
                                            class: "mor-input mr-text-area",
                                            rows: "3",
                                            value: "{t.text}",
                                            // Typing must never reach the shortcut root — a
                                            // single-letter bind would fire on each keystroke.
                                            onkeydown: move |evt| evt.stop_propagation(),
                                            oninput: move |evt| {
                                                let v = evt.value();
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    item.text = v;
                                                }
                                                rerender_title_soon(k);
                                            },
                                        }
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
                                            "{t.segments().len()} card(s) — one per word, revealed over the first 60% of the text, then held."
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
                                    // Built-in starter looks first, then the user's
                                    // saved styles — the gallery OpenShot/kdenlive ship,
                                    // as a one-click list that works before you've saved
                                    // anything of your own.
                                    MorSelect {
                                        label: "Apply a style".to_string(),
                                        value: "—".to_string(),
                                        options: std::iter::once("—".to_string())
                                            .chain(builtin_title_styles().iter().map(|p| p.name.clone()))
                                            .chain(presets.read().iter().map(|p| p.name.clone()))
                                            .collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            let found = builtin_title_styles()
                                                .into_iter()
                                                .chain(presets.read().iter().cloned())
                                                .find(|p| p.name == v);
                                            let Some(p) = found else { return };
                                            push_undo("");
                                            if let Some(item) = titles.write().get_mut(k) {
                                                *item = restyle(item, &p.style);
                                            }
                                            rerender_title(k);
                                            status.set(format!("Applied \"{}\".", p.name));
                                        },
                                    }
                                    button {
                                        class: "mor-btn mr-reset",
                                        title: "Keep this card's look to use on other text cards and other reels",
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
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove text" }
                                    }
                                }
                            }
                            Some(Sel::Aud(k)) if k < audios.read().len() && active_phase() != Phase::Text => {
                                let a = audios.read()[k].clone();
                                let span = a.span();
                                let vend = a.end_gain();
                                let treat_opts: Vec<String> =
                                    engine::AUDIO_TREATS.iter().map(|s| s.to_string()).collect();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 {
                                            span { class: "mr-ctx-tag audio", "{a.lane_tag()}" }
                                            " {a.name}"
                                        }
                                        p { class: "mor-statusbar-muted",
                                            "Mixed under V1 from {fmt_t(a.at)} for {fmt_t(span)}."
                                        }
                                    }

                                    // Whole-source waveform with the kept window bright
                                    // and the trimmed ends and fade ramps shaded — the
                                    // same picture the lane shows, sized to read here.
                                    if !a.wave.is_empty() {
                                        div { class: "mr-insp-wave", style: "background-image:url({a.wave});",
                                            div { class: "mr-insp-trim", style: "left:0; width:{a.in_s / a.duration.max(0.01) * 100.0}%" }
                                            div { class: "mr-insp-trim", style: "right:0; width:{(a.duration - a.out_s).max(0.0) / a.duration.max(0.01) * 100.0}%" }
                                            if a.fade_in > 0.0 {
                                                div { class: "mr-insp-fadein", style: "left:{a.in_s / a.duration.max(0.01) * 100.0}%; width:{a.fade_in / a.duration.max(0.01) * 100.0}%" }
                                            }
                                            if a.fade_out > 0.0 {
                                                div { class: "mr-insp-fadeout", style: "left:{(a.out_s - a.fade_out) / a.duration.max(0.01) * 100.0}%; width:{a.fade_out / a.duration.max(0.01) * 100.0}%" }
                                            }
                                        }
                                    }

                                    h4 { class: "mr-fx-cat", "Mix & track" }
                                    MorSelect {
                                        label: "Track".to_string(),
                                        value: a.lane_tag().to_string(),
                                        options: vec!["A1".to_string(), "A2".to_string()],
                                        onchange: move |v: String| {
                                            push_undo("");
                                            audios.write()[k].lane = if v == "A2" { 2 } else { 1 };
                                        },
                                    }
                                    Slider {
                                        label: Some("Volume start"),
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
                                        label: Some("Volume end (automation)"),
                                        min: 0.0,
                                        max: 2.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: vend,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("avole{k}"));
                                            audios.write()[k].vol_end = v;
                                        })),
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Start and end equal = flat gain. Differ them for a linear volume ramp across the clip."
                                    }
                                    div { class: "mr-toolbar",
                                        button {
                                            class: "mor-btn",
                                            title: "Measure this clip and set its gain to a standard −14 LUFS loudness",
                                            onclick: move |_| {
                                                let (path, in_s, out_s) = match audios.read().get(k) {
                                                    Some(a) => (a.path.clone(), a.in_s, a.out_s),
                                                    None => return,
                                                };
                                                push_undo("");
                                                status.set("Measuring loudness…".to_string());
                                                spawn(async move {
                                                    match engine::measure_loudness(&path, in_s, out_s).await {
                                                        Ok(lufs) => {
                                                            let g = engine::normalize_gain(lufs, -14.0);
                                                            if let Some(a) = audios.write().get_mut(k) {
                                                                a.volume = g;
                                                                a.vol_end = -1.0;
                                                            }
                                                            status.set(format!("Normalized to −14 LUFS (was {lufs:.1}; gain {g:.2}×)."));
                                                        }
                                                        Err(e) => status.set(format!("Normalize failed: {e}")),
                                                    }
                                                });
                                            },
                                            "⚖ Normalize to −14 LUFS"
                                        }
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
                                            "Sidechain compress: bed pulls down when V1 is talking and recovers in the gaps."
                                        }
                                    }

                                    h4 { class: "mr-fx-cat", "Fades" }
                                    Slider {
                                        label: Some("Fade in"),
                                        min: 0.0,
                                        max: (span * 0.49).max(0.1),
                                        step: 0.05,
                                        precision: 2,
                                        value: a.fade_in.min(span * 0.49),
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("afin{k}"));
                                            audios.write()[k].fade_in = v.max(0.0);
                                        })),
                                    }
                                    Slider {
                                        label: Some("Fade out"),
                                        min: 0.0,
                                        max: (span * 0.49).max(0.1),
                                        step: 0.05,
                                        precision: 2,
                                        value: a.fade_out.min(span * 0.49),
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("afout{k}"));
                                            audios.write()[k].fade_out = v.max(0.0);
                                        })),
                                    }
                                    div { class: "mr-toolbar",
                                        button {
                                            class: "mor-btn",
                                            title: "0.5s fade in and out",
                                            onclick: move |_| {
                                                push_undo("");
                                                let mut au = audios.write();
                                                let half = au[k].span() * 0.49;
                                                au[k].fade_in = 0.5_f64.min(half);
                                                au[k].fade_out = 0.5_f64.min(half);
                                            },
                                            "↕ 0.5s both ends"
                                        }
                                        button {
                                            class: "mor-btn",
                                            onclick: move |_| {
                                                push_undo("");
                                                audios.write()[k].fade_in = 0.0;
                                                audios.write()[k].fade_out = 0.0;
                                            },
                                            "Clear fades"
                                        }
                                    }

                                    h4 { class: "mr-fx-cat", "Processing" }
                                    MorSelect {
                                        label: "Treatment".to_string(),
                                        value: a.treat.clone(),
                                        options: treat_opts,
                                        onchange: move |v: String| {
                                            push_undo("");
                                            audios.write()[k].treat = v;
                                        },
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Voice enhance and Podcast shape speech; Warm/Bright/Bass cut are broad EQ. Same in preview and export."
                                    }
                                    Slider {
                                        label: Some("Noise reduction"),
                                        min: 0.0,
                                        max: 1.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.denoise,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("aden{k}"));
                                            audios.write()[k].denoise = v;
                                        })),
                                    }
                                    if a.denoise > 0.001 {
                                        // Sensitivity: how loud a bin must be to
                                        // count as signal. Higher = removes more.
                                        Slider {
                                            label: Some("Noise floor (dB)"),
                                            min: -80.0,
                                            max: -20.0,
                                            step: 1.0,
                                            precision: 0,
                                            value: a.noise_floor,
                                            oninput: Some(EventHandler::new(move |v: f64| {
                                                push_undo(&format!("anf{k}"));
                                                audios.write()[k].noise_floor = v;
                                            })),
                                        }
                                        MorCheckbox {
                                            label: "Adaptive (track the noise across the clip)".to_string(),
                                            checked: a.track_noise,
                                            onchange: move |on: bool| {
                                                push_undo(&format!("atn{k}"));
                                                audios.write()[k].track_noise = on;
                                            },
                                        }
                                    }
                                    Slider {
                                        label: Some("Compression"),
                                        min: 0.0,
                                        max: 1.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.compress,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("acmp{k}"));
                                            audios.write()[k].compress = v;
                                        })),
                                    }
                                    if a.treat == "Podcast" {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Podcast already includes gentle compression — the slider is ignored so it doesn't double-glue."
                                        }
                                    }
                                    Slider {
                                        label: Some("Noise gate"),
                                        min: 0.0,
                                        max: 1.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.gate,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("agate{k}"));
                                            audios.write()[k].gate = v;
                                        })),
                                    }
                                    if a.gate > 0.001 {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Silences room tone between words — raise it just until the gaps go quiet, not so far it clips word tails."
                                        }
                                    }
                                    Slider {
                                        label: Some("De-click"),
                                        min: 0.0,
                                        max: 1.0,
                                        step: 0.05,
                                        precision: 2,
                                        value: a.declick,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("adeclk{k}"));
                                            audios.write()[k].declick = v;
                                        })),
                                    }
                                    if a.declick > 0.001 {
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Repairs clicks and pops in field audio — mouth clicks, cable crackle, edit ticks."
                                        }
                                    }
                                    button {
                                        class: "mor-btn mr-reset",
                                        title: "Voice enhance + light denoise + light compression — a one-click VO polish",
                                        onclick: move |_| {
                                            push_undo("");
                                            let mut au = audios.write();
                                            au[k].treat = "Voice enhance".into();
                                            au[k].denoise = au[k].denoise.max(0.35);
                                            au[k].compress = au[k].compress.max(0.35);
                                        },
                                        "✦ Voice polish"
                                    }

                                    h4 { class: "mr-fx-cat", "Sync & trim" }
                                    Slider {
                                        label: Some("Position on timeline"),
                                        min: 0.0,
                                        max: total.max(0.5),
                                        step: 0.05,
                                        precision: 2,
                                        value: a.at,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("apos{k}"));
                                            audios.write()[k].at = v.max(0.0);
                                        })),
                                    }
                                    div { class: "mr-toolbar",
                                        button {
                                            class: "mor-btn",
                                            title: "Move the start of this item to the playhead",
                                            onclick: move |_| {
                                                push_undo("");
                                                audios.write()[k].at = playhead().max(0.0);
                                                status.set(format!(
                                                    "Synced {} start to {}.",
                                                    audios.read()[k].lane_tag(),
                                                    fmt_t(playhead())
                                                ));
                                            },
                                            "⊙ Align to playhead"
                                        }
                                        button {
                                            class: "mor-btn",
                                            title: "Nudge earlier by one frame (1/30 s)",
                                            onclick: move |_| {
                                                push_undo(&format!("anud{k}"));
                                                let a = &mut audios.write()[k];
                                                a.at = (a.at - 1.0 / 30.0).max(0.0);
                                            },
                                            "−1f"
                                        }
                                        button {
                                            class: "mor-btn",
                                            title: "Nudge later by one frame (1/30 s)",
                                            onclick: move |_| {
                                                push_undo(&format!("anud{k}"));
                                                audios.write()[k].at += 1.0 / 30.0;
                                            },
                                            "+1f"
                                        }
                                    }
                                    Slider {
                                        label: Some("In point"),
                                        min: 0.0,
                                        max: a.duration,
                                        step: 0.05,
                                        precision: 2,
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
                                        precision: 2,
                                        value: a.out_s,
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            let mut au = audios.write();
                                            au[k].out_s = v.max(au[k].in_s + 0.1).min(au[k].duration);
                                        })),
                                    }

                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove audio" }
                                    }
                                }
                            }
                            _ => if active_phase() == Phase::Text {
                                // Snapshot the T lane so the list can be clicked
                                // without holding a read borrow across the rsx.
                                let text_items: Vec<(usize, String, f64)> = titles
                                    .read()
                                    .iter()
                                    .enumerate()
                                    .map(|(k, t)| {
                                        let label = if t.kind != "Text" {
                                            t.kind.clone()
                                        } else if t.text.trim().is_empty() {
                                            "(empty)".to_string()
                                        } else {
                                            t.text.replace('\n', " ")
                                        };
                                        (k, label, t.at)
                                    })
                                    .collect();
                                rsx! {
                                    p { class: "mor-statusbar-muted",
                                        "Add a text card or caption, then pick one below — or on the T lane — to style it."
                                    }
                                    div { class: "mr-phase-actions",
                                        button { class: "mor-btn primary", onclick: move |_| add_title(()), "T Add text / caption" }
                                        button { class: "mor-btn", disabled: no_clips || transcribing(), onclick: move |_| auto_captions(()), "✎ Auto-caption from audio" }
                                    }
                                    if text_items.is_empty() {
                                        p { class: "mor-statusbar-muted mr-text-empty",
                                            "No text on the reel yet — add one above."
                                        }
                                    } else {
                                        div { class: "mr-text-list",
                                            for (k, label, at) in text_items {
                                                button {
                                                    key: "{k}",
                                                    class: "mr-text-row",
                                                    onclick: move |_| { selected.set(Some(Sel::Title(k))); seek_to(at); },
                                                    span { class: "mr-ctx-tag title", "T" }
                                                    span { class: "mr-text-row-label", "{label}" }
                                                    span { class: "mr-text-row-time", "{fmt_t(at)}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if active_phase() == Phase::Audio {
                                rsx! {
                                    p { class: "mor-statusbar-muted",
                                        "Add music or a voiceover under the picture, then pick it on the A lane to trim and mix."
                                    }
                                    div { class: "mr-phase-actions",
                                        button { class: "mor-btn primary", onclick: move |_| add_audio(1), "♪ Add music (A1)" }
                                        button { class: "mor-btn", onclick: move |_| add_audio(2), "🎙 Add voiceover (A2)" }
                                    }
                                }
                            } else {
                                rsx! {
                                    p { class: "mor-statusbar-muted",
                                        "Add portrait or landscape clips — each clip's Framing picks crop, letterbox fit, or zoom into 9:16. Select an item on the timeline to edit it."
                                    }
                                    div { class: "mr-toolbar",
                                        button {
                                            class: "mor-btn primary",
                                            disabled: exporting,
                                            onclick: move |_| show_add.set(true),
                                            "＋ Add to reel…"
                                        }
                                    }
                                }
                            },
                        }}
                        }

                        p { class: "mor-statusbar-muted mr-keys",
                            "M beat · T handles · Drop files · Ctrl+Z · I/O · S split · Del · ←/→ · Ctrl+G · ~ magnet · G safe · Ctrl+E"
                        }
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
                        // Fade corner: horizontal drag away from the edge grows
                        // the fade; toward it shrinks. Capped at half the span so
                        // in and out can't cross.
                        if let Some((k, is_out, grab_x, at_grab)) = fade_drag() {
                            let dt = (p.x - grab_x) / calc_scale();
                            let mut au = audios.write();
                            if let Some(a) = au.get_mut(k) {
                                let cap = a.span() * 0.49;
                                let v = if is_out { at_grab - dt } else { at_grab + dt };
                                let v = v.clamp(0.0, cap);
                                if is_out { a.fade_out = v } else { a.fade_in = v }
                            }
                            return;
                        }
                        // Volume line: drag up = louder. ~180px sweeps the full
                        // 0..2 range, which feels neither twitchy nor sluggish.
                        if let Some((k, grab_y, at_grab)) = vol_drag() {
                            let g = (at_grab - (p.y - grab_y) / 90.0).clamp(0.0, 2.0);
                            let mut au = audios.write();
                            if let Some(a) = au.get_mut(k) {
                                a.volume = g;
                                if a.vol_end >= 0.0 {
                                    a.vol_end = g;
                                }
                            }
                            return;
                        }
                        // Title length: drag the right edge to stretch or shorten.
                        if let Some((k, grab_x, dur_at)) = len_drag() {
                            let dt = (p.x - grab_x) / calc_scale();
                            if let Some(t) = titles.write().get_mut(k) {
                                t.dur = (dur_at + dt).max(0.3);
                            }
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
                        // A completed handle drag also swallows the trailing click
                        // so it doesn't re-seek the playhead to the clip start.
                        if fade_drag().is_some() || vol_drag().is_some() || len_drag().is_some() {
                            drag_moved.set(true);
                        }
                        drag.set(None);
                        fade_drag.set(None);
                        vol_drag.set(None);
                        len_drag.set(None);
                        pan.set(None);
                        scrubbing.set(false);
                    },
                    onmouseleave: move |_| {
                        drag.set(None);
                        fade_drag.set(None);
                        vol_drag.set(None);
                        len_drag.set(None);
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
                                div { class: "mr-track {phase_lane_class(active_phase())}", style: "width: {track_end * scale}px",
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
                                    div { class: "mr-lane mr-lane-t",
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
                                                // Right-edge grip: drag to stretch or shorten the title.
                                                div {
                                                    class: "mr-len-grip",
                                                    title: "Drag to change how long the title shows",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        push_undo(&format!("tlen{k}"));
                                                        let d = titles.read().get(k).map_or(3.0, |t| t.dur);
                                                        len_drag.set(Some((k, evt.client_coordinates().x, d)));
                                                    },
                                                }
                                                if t.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(t.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                if t.kind == "Text" { "𝐓 {t.text}" } else { "◧ {t.kind}" }
                                            }
                                        }
                                    }
                                    div {
                                        class: if drop_hover() == Some(Lane::V2) { "mr-lane mr-lane-v mr-drop" } else { "mr-lane mr-lane-v" },
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
                                    // A1 + A2: same mix bus under V1, two editorial lanes
                                    // so music and VO can live side by side without stacking.
                                    for (bus, lane, tag) in [(1u8, Lane::A1, "A1"), (2u8, Lane::A2, "A2")] {
                                        div {
                                            key: "{tag}",
                                            class: if drop_hover() == Some(lane) {
                                                "mr-lane mr-lane-a1 mr-drop"
                                            } else {
                                                "mr-lane mr-lane-a1"
                                            },
                                            ondragover: move |evt| {
                                                evt.prevent_default();
                                                evt.stop_propagation();
                                                if drop_hover() != Some(lane) { drop_hover.set(Some(lane)); }
                                            },
                                            ondragleave: move |_| {
                                                if drop_hover() == Some(lane) { drop_hover.set(None); }
                                            },
                                            ondrop: move |evt| {
                                                evt.prevent_default();
                                                evt.stop_propagation();
                                                let (paths, t) = drop_payload(&evt);
                                                handle_drop(paths, lane, t);
                                            },
                                            span { class: "mr-lane-tag", "{tag}" }
                                            for (k, a) in audios().into_iter().enumerate().filter(|(_, a)| {
                                                if bus >= 2 { a.lane >= 2 } else { a.lane < 2 }
                                            }) {
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
                                                    // Fade ramps drawn as shaded wedges — dark at the
                                                    // silent edge, clear where the bed is at full level.
                                                    if a.fade_in > 0.0 {
                                                        div { class: "mr-fade-in", style: "width: {a.fade_in * scale}px" }
                                                    }
                                                    if a.fade_out > 0.0 {
                                                        div { class: "mr-fade-out", style: "width: {a.fade_out * scale}px" }
                                                    }
                                                    // Volume line: drag up/down to set the bed's level.
                                                    div {
                                                        class: "mr-vol-line",
                                                        style: "bottom: {a.volume / 2.0 * 100.0}%",
                                                        title: "Drag up or down to set volume",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("avol{k}"));
                                                            let g = audios.read().get(k).map_or(1.0, |a| a.volume);
                                                            vol_drag.set(Some((k, evt.client_coordinates().y, g)));
                                                        },
                                                    }
                                                    // Corner grips: drag inward to fade from/to silence.
                                                    div {
                                                        class: "mr-fade-grip in",
                                                        title: "Drag right to fade in",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("afin{k}"));
                                                            let v = audios.read().get(k).map_or(0.0, |a| a.fade_in);
                                                            fade_drag.set(Some((k, false, evt.client_coordinates().x, v)));
                                                        },
                                                    }
                                                    div {
                                                        class: "mr-fade-grip out",
                                                        title: "Drag left to fade out",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("afout{k}"));
                                                            let v = audios.read().get(k).map_or(0.0, |a| a.fade_out);
                                                            fade_drag.set(Some((k, true, evt.client_coordinates().x, v)));
                                                        },
                                                    }
                                                    if a.group != 0 {
                                                        span { class: "mr-group-dot", style: "background: hsl({(a.group * 67) % 360}, 70%, 60%)" }
                                                    }
                                                    if a.fade_in > 0.0 || a.fade_out > 0.0 || a.duck > 0.0 || a.denoise > 0.0 || a.gate > 0.0 || a.declick > 0.0 || a.treat != "None" {
                                                        span { class: "mr-audio-fx", title: "Processing or fades active", "✦" }
                                                    }
                                                    "♪ {a.name}"
                                                }
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
                // Workflow spine: the reel-building phases left→right, the phase
                // you're in lit up, a ✓ on phases that already have content. Each
                // button is the primary action for its phase, not a menu clone.
                div { class: "mr-workflow",
                    button {
                        class: if active_phase() == Phase::Add { "mr-wf active" } else { "mr-wf" },
                        title: "Add clips, b-roll or music",
                        onclick: move |_| active_phase.set(Phase::Add),
                        span { class: "mr-wf-icon", "＋" }
                        span { class: "mr-wf-label", "Add" }
                        if !clips.read().is_empty() { span { class: "mr-wf-tick", "✓" } }
                    }
                    button {
                        class: if active_phase() == Phase::Cut { "mr-wf active" } else { "mr-wf" },
                        title: "Trim, split and arrange the current clip",
                        onclick: move |_| {
                            if selected().is_none() {
                                if let Some((i, _)) = locate(&clips.read(), playhead()) { selected.set(Some(Sel::Main(i))); }
                            }
                            active_phase.set(Phase::Cut);
                        },
                        span { class: "mr-wf-icon", "✂" }
                        span { class: "mr-wf-label", "Cut" }
                    }
                    button {
                        class: if active_phase() == Phase::Style { "mr-wf active" } else { "mr-wf" },
                        title: "Effects, transform and Ken Burns for the current clip",
                        onclick: move |_| {
                            if selected().is_none() {
                                if let Some((i, _)) = locate(&clips.read(), playhead()) { selected.set(Some(Sel::Main(i))); }
                            }
                            active_phase.set(Phase::Style);
                        },
                        span { class: "mr-wf-icon", "✦" }
                        span { class: "mr-wf-label", "Style" }
                        if clips.read().iter().any(|c| c.effect != "None" || c.transform.scale.is_animated()) {
                            span { class: "mr-wf-tick", "✓" }
                        }
                    }
                    button {
                        class: if active_phase() == Phase::Background { "mr-wf active" } else { "mr-wf" },
                        title: "Frame background behind banded or shrunk clips",
                        onclick: move |_| active_phase.set(Phase::Background),
                        span { class: "mr-wf-icon", "▧" }
                        span { class: "mr-wf-label", "Bg" }
                        if clips.read().iter().any(|c| c.transform.bg != engine::Bg::Black) {
                            span { class: "mr-wf-tick", "✓" }
                        }
                    }
                    button {
                        class: if active_phase() == Phase::Text { "mr-wf active" } else { "mr-wf" },
                        title: "Text and captions",
                        onclick: move |_| active_phase.set(Phase::Text),
                        span { class: "mr-wf-icon", "T" }
                        span { class: "mr-wf-label", "Text" }
                        if !titles.read().is_empty() { span { class: "mr-wf-tick", "✓" } }
                    }
                    button {
                        class: if active_phase() == Phase::Audio { "mr-wf active" } else { "mr-wf" },
                        title: "Music and voiceover under the picture",
                        onclick: move |_| active_phase.set(Phase::Audio),
                        span { class: "mr-wf-icon", "♪" }
                        span { class: "mr-wf-label", "Audio" }
                        if !audios.read().is_empty() { span { class: "mr-wf-tick", "✓" } }
                    }
                    button {
                        class: if active_phase() == Phase::Export { "mr-wf active mr-wf-export" } else { "mr-wf mr-wf-export" },
                        title: "Export your reel",
                        onclick: move |_| active_phase.set(Phase::Export),
                        span { class: "mr-wf-icon", "⇪" }
                        span { class: "mr-wf-label", "Export" }
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
                                label: "Add to reel…".to_string(),
                                disabled: exporting,
                                on_action: move |_| show_add.set(true),
                            }
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
                                on_action: move |_| add_audio(1),
                            }
                            CtxItem {
                                label: "Add audio (A2)…".to_string(),
                                disabled: no_clips || exporting,
                                on_action: move |_| add_audio(2),
                            }
                            CtxItem {
                                label: "Add text (T)".to_string(),
                                shortcut: Some("Ctrl+T".to_string()),
                                disabled: no_clips || exporting,
                                on_action: move |_| add_title(()),
                            }
                            MenuSeparator {}
                            CtxItem {
                                label: "Effects palette…".to_string(),
                                on_action: move |_| show_effects.set(true),
                            }
                            CtxItem {
                                label: "Export…".to_string(),
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
                                CtxItem {
                                    label: "Effects palette…".to_string(),
                                    on_action: move |_| {
                                        insp_open.set(true);
                                        show_effects.set(true);
                                    },
                                }
                                CtxItem {
                                    label: "Detach audio to A1".to_string(),
                                    shortcut: Some("Ctrl+U".to_string()),
                                    disabled: !clips.read().get(i).is_some_and(|c| c.has_audio),
                                    on_action: move |_| detach_audio(()),
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
                                CtxItem {
                                    label: "Effects palette…".to_string(),
                                    on_action: move |_| {
                                        insp_open.set(true);
                                        show_effects.set(true);
                                    },
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
                            let tag = audios.read().get(k).map(|a| a.lane_tag()).unwrap_or("A1");
                            let other = if tag == "A2" { "A1" } else { "A2" };
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag audio", "{tag}" }
                                    span { class: "mr-ctx-name", "{name}" }
                                }
                                CtxItem {
                                    label: "Split at playhead".to_string(),
                                    shortcut: Some("S".to_string()),
                                    on_action: move |_| split_at_playhead(()),
                                }
                                CtxItem {
                                    label: format!("Move to {other}"),
                                    on_action: move |_| {
                                        push_undo("");
                                        if let Some(a) = audios.write().get_mut(k) {
                                            a.lane = if a.lane >= 2 { 1 } else { 2 };
                                        }
                                    },
                                }
                                CtxItem {
                                    label: "Align start to playhead".to_string(),
                                    on_action: move |_| {
                                        push_undo("");
                                        if let Some(a) = audios.write().get_mut(k) {
                                            a.at = playhead().max(0.0);
                                        }
                                    },
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
                                    label: "Remove text".to_string(),
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
            open: show_autocut,
            title: "Auto-cut silence".to_string(),
            style: "min-width:400px; max-width:520px;".to_string(),
            div { class: "mr-export-dialog",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Removes quiet stretches from V1 based on audio volume (ffmpeg silencedetect). \
                     Loud parts stay as separate clips in order. Undo restores the cut."
                }
                Slider {
                    label: Some("Silence threshold (−dB)"),
                    min: 20.0,
                    max: 50.0,
                    step: 1.0,
                    precision: 0,
                    value: autocut_noise(),
                    oninput: Some(EventHandler::new(move |v: f64| autocut_noise.set(v))),
                }
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Cut audio quieter than −{autocut_noise():.0} dB. Higher = more aggressive (cuts softer speech)."
                }
                Slider {
                    label: Some("Min silence to remove"),
                    min: 0.1,
                    max: 2.0,
                    step: 0.05,
                    precision: 2,
                    value: autocut_min_sil(),
                    oninput: Some(EventHandler::new(move |v: f64| autocut_min_sil.set(v))),
                }
                Slider {
                    label: Some("Padding around speech"),
                    min: 0.0,
                    max: 0.4,
                    step: 0.01,
                    precision: 2,
                    value: autocut_pad(),
                    oninput: Some(EventHandler::new(move |v: f64| autocut_pad.set(v))),
                }
                Slider {
                    label: Some("Min keep length"),
                    min: 0.05,
                    max: 1.0,
                    step: 0.05,
                    precision: 2,
                    value: autocut_min_keep(),
                    oninput: Some(EventHandler::new(move |v: f64| autocut_min_keep.set(v))),
                }
                MorSelect {
                    label: "Scope".to_string(),
                    value: if autocut_sel_only() {
                        "Selected V1 clip".to_string()
                    } else {
                        "All V1 clips with audio".to_string()
                    },
                    options: vec![
                        "Selected V1 clip".to_string(),
                        "All V1 clips with audio".to_string(),
                    ],
                    onchange: move |v: String| {
                        autocut_sel_only.set(v.starts_with("Selected"));
                    },
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn",
                        onclick: move |_| show_autocut.set(false),
                        "Cancel"
                    }
                    button {
                        class: "mor-btn primary",
                        disabled: autocut_busy() || no_clips,
                        onclick: move |_| run_autocut(()),
                        if autocut_busy() { "Detecting…" } else { "✂ Cut silence" }
                    }
                }
            }
        }
        Modal {
            open: show_add,
            title: "Add to reel".to_string(),
            style: "min-width:420px; max-width:560px;".to_string(),
            div { class: "mr-add-dialog",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Pick a lane. You can also drag files from a file manager onto the timeline — the lane under the cursor decides what they become."
                }
                div { class: "mr-add-grid",
                    button {
                        class: "mr-add-card",
                        disabled: importing() || exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            import_clips(());
                        },
                        span { class: "mr-add-tag", "V1" }
                        strong { "Clips" }
                        span { class: "mor-statusbar-muted", "Main story track — trim, reorder, split" }
                    }
                    button {
                        class: "mr-add-card",
                        disabled: no_clips || exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            add_overlay(());
                        },
                        span { class: "mr-add-tag", "V2" }
                        strong { "Overlay" }
                        span { class: "mor-statusbar-muted", "B-roll cutaway over V1" }
                    }
                    button {
                        class: "mr-add-card",
                        disabled: no_clips || exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            add_audio(1);
                        },
                        span { class: "mr-add-tag audio", "A1" }
                        strong { "Music / bed" }
                        span { class: "mor-statusbar-muted", "Background track — duck under speech" }
                    }
                    button {
                        class: "mr-add-card",
                        disabled: no_clips || exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            add_audio(2);
                        },
                        span { class: "mr-add-tag audio", "A2" }
                        strong { "VO / second bed" }
                        span { class: "mor-statusbar-muted", "Second mix bus under the picture" }
                    }
                    button {
                        class: "mr-add-card",
                        disabled: no_clips || exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            add_title(());
                        },
                        span { class: "mr-add-tag title", "T" }
                        strong { "Text" }
                        span { class: "mor-statusbar-muted", "Caption card at the playhead" }
                    }
                }
                if !no_clips {
                    div { class: "mr-toolbar mr-add-extra",
                        button {
                            class: "mor-btn",
                            disabled: exporting || transcribing(),
                            onclick: move |_| {
                                show_add.set(false);
                                auto_captions(());
                            },
                            if transcribing() { "Transcribing…" } else { "Auto captions…" }
                        }
                    }
                }
            }
        }
        Modal {
            open: show_effects,
            title: match active_phase() {
                Phase::Cut => "Transitions".to_string(),
                Phase::Text => "Text effects".to_string(),
                Phase::Audio => "Audio effects".to_string(),
                Phase::Background => "Backgrounds".to_string(),
                _ => "Effects palette".to_string(),
            },
            style: "min-width:480px; max-width:720px;".to_string(),
            {
                // Each workspace browses its own effect family: filters in Style,
                // transitions in Cut, entrances in Text, EQ in Audio, colours in
                // Background. Same apply-paths as the inline inspector controls.
                match active_phase() {
                    Phase::Cut => {
                        let sel = match selected() {
                            Some(Sel::Main(i)) if i > 0 && i < clips.read().len() => Some(i),
                            _ => None,
                        };
                        match sel {
                            Some(i) => {
                                let current = clips.read()[i].transition.clone();
                                rsx! {
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "How the selected clip eases in from the one before it. A crossfade overlaps them, so the reel gets a little shorter."
                                        }
                                        div { class: "mr-fx-grid",
                                            for (label, _) in engine::TRANSITIONS.iter().copied() {
                                                button {
                                                    key: "{label}",
                                                    class: if current == label { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                    onclick: move |_| {
                                                        push_undo("");
                                                        let old = spans();
                                                        clips.write()[i].transition = label.to_string();
                                                        ride(old, &|k| Some(start_of(k)));
                                                        seek_to(playhead().min(total_of()));
                                                    },
                                                    div { class: "mr-fx-ph" }
                                                    span { "{label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            None => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Select a V1 clip other than the first — a transition joins it to the clip before it."
                                }
                            },
                        }
                    }
                    Phase::Text => {
                        let sel = match selected() {
                            Some(Sel::Title(k)) if k < titles.read().len() => Some(k),
                            _ => None,
                        };
                        match sel {
                            Some(k) => {
                                let current = titles.read()[k].anim.clone();
                                rsx! {
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "How the selected text card arrives on screen."
                                        }
                                        div { class: "mr-fx-grid",
                                            for name in engine::TITLE_ANIMS.iter().copied() {
                                                button {
                                                    key: "{name}",
                                                    class: if current == name { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                    onclick: move |_| {
                                                        push_undo("");
                                                        titles.write()[k].anim = name.to_string();
                                                    },
                                                    div { class: "mr-fx-ph" }
                                                    span { "{name}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            None => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Select a text card on the T lane to give it an entrance."
                                }
                            },
                        }
                    }
                    Phase::Audio => {
                        let sel = match selected() {
                            Some(Sel::Aud(k)) if k < audios.read().len() => Some(k),
                            _ => None,
                        };
                        match sel {
                            Some(k) => {
                                let current = audios.read()[k].treat.clone();
                                rsx! {
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Voice shaping and EQ for the selected bed. Same in preview and export."
                                        }
                                        div { class: "mr-fx-grid",
                                            for name in engine::AUDIO_TREATS.iter().copied() {
                                                button {
                                                    key: "{name}",
                                                    class: if current == name { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                    onclick: move |_| {
                                                        push_undo("");
                                                        audios.write()[k].treat = name.to_string();
                                                    },
                                                    div { class: "mr-fx-ph" }
                                                    span { "{name}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            None => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Select an A1 or A2 clip to shape its sound."
                                }
                            },
                        }
                    }
                    Phase::Background => {
                        if clips.read().is_empty() {
                            rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Add a clip first — the background shows wherever the picture doesn't fill the 9:16 frame."
                                }
                            }
                        } else {
                            let current = clips.read().first().map(|c| c.transform.bg);
                            rsx! {
                                div { class: "mr-fx-dialog",
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "The colour behind a banded or shrunk clip. Applies to the whole reel."
                                    }
                                    div { class: "mr-fx-grid",
                                        for b in engine::Bg::ALL {
                                            button {
                                                key: "{b.label()}",
                                                class: if current == Some(b) { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                onclick: move |_| {
                                                    push_undo("bg");
                                                    for c in clips.write().iter_mut() { c.transform.bg = b; }
                                                    seek_to(playhead());
                                                },
                                                div { class: "mr-fx-ph", style: "background: {b.color()}" }
                                                span { "{b.label()}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Style (and the non-editing steps) browse the video look filters.
                    _ => {
                        let cur = match selected() {
                            Some(Sel::Main(i)) => clips.read().get(i).map(|c| (c.effect.clone(), c.effect_amount)),
                            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| (o.effect.clone(), o.effect_amount)),
                            _ => None,
                        };
                        match cur {
                            Some((current, amount)) => {
                                let thumbs = fx_thumbs();
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
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Looks apply to the selected V1 clip or V2 overlay. Motion looks animate as you scrub."
                                        }
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
                            }
                            None => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Effects apply to video — select a V1 clip or V2 overlay on the timeline, then open this palette again. Motion looks are ports of moranima's camera moves."
                                }
                            },
                        }
                    }
                }
            }
        }
        Modal {
            open: show_keys,
            title: "Keyboard layout".to_string(),
            div { class: "mr-keys-dialog",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Coming from another editor? Match its blade key. MorReel's command set is small, "
                    "so this remaps the one editing key that differs everywhere — split at playhead — "
                    "rather than fake a whole keymap. More will follow as MorReel grows."
                }
                for k in KeyScheme::ALL {
                    button {
                        class: if key_scheme() == k { "mor-btn primary mr-keys-opt" } else { "mor-btn mr-keys-opt" },
                        onclick: move |_| {
                            key_scheme.set(k);
                            save_keyscheme(k);
                            status.set(format!("Keyboard layout: {} — split is now {}", k.label(), k.split()));
                        },
                        span { class: "mr-keys-name", "{k.label()}" }
                        span { class: "mr-key", "split · {k.split()}" }
                    }
                }
                div { class: "mr-toolbar",
                    button { class: "mor-btn", onclick: move |_| show_keys.set(false), "Done" }
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
                    ("Edit › Auto-cut silence…", "Remove quiet stretches by volume"),
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
                    ("B", "Detect beats from the music bed → fill markers"),
                    ("Home / End", "Jump to start / end"),
                    ("Ctrl+O", "Add clips"),
                    ("Ctrl+Shift+O / Ctrl+S", "Open / save project"),
                    ("Ctrl+,", "Project settings"),
                    ("F11", "Fullscreen"),
                    ("Ctrl+T", "Add text at playhead"),
                    ("Ctrl+U", "Detach selected clip audio to A1"),
                    ("Ctrl+E", "Export MP4"),
                    ("Ctrl+Q", "Quit"),
                ] {
                    // The blade key follows the chosen editor scheme; the rest are fixed.
                    tr {
                        td { class: "mr-key", "{help_key(keys, what, key_scheme())}" }
                        td { "{what}" }
                    }
                }
            }
        }
        Modal {
            open: show_save_preset,
            title: "Save text style".to_string(),
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
            open: show_save_layout,
            title: "Save layout".to_string(),
            div { class: "mr-export-dialog",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Remembers whether the inspector is docked, floated or hidden — and where a floated one sits. Saved outside the project, so every reel can use it."
                }
                mor_rust_dioxus_ui_kit::MorTextInput {
                    label: "Name".to_string(),
                    value: layout_name(),
                    onchange: move |v: String| layout_name.set(v),
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn",
                        onclick: move |_| { show_save_layout.set(false); layout_name.set(String::new()); },
                        "Cancel"
                    }
                    button {
                        class: "mor-btn primary",
                        disabled: layout_name().trim().is_empty(),
                        onclick: move |_| store_layout(()),
                        "Save layout"
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
                    if let Some(warn) = over_limits(total, &settings().platform) { " · {warn}" }
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
            open: show_settings,
            title: "Project settings".to_string(),
            div { class: "mr-settings-dialog",
                MorTabs {
                    tabs: vec!["Format".to_string(), "Platform".to_string(), "About".to_string()],
                    active: settings_tab(),
                    onchange: move |t: String| settings_tab.set(t),
                }
                if settings_tab() == "Format" {
                    // Portrait profile: 9:16 and 30 fps are fixed (the ffmpeg
                    // graph composes at that frame), so only the target size is a
                    // real choice — and it seeds the export dialog's size.
                    MorSelect {
                        label: "Target resolution".to_string(),
                        value: engine::size_label(settings().resolution),
                        options: engine::SIZES.iter().map(|(l, _, _)| l.to_string()).collect::<Vec<_>>(),
                        onchange: move |v: String| {
                            let w = engine::SIZES.iter().find(|(l, _, _)| *l == v).map_or(1080, |(_, w, _)| *w);
                            let mut s = settings();
                            s.resolution = w;
                            settings.set(s);
                            export_opts.set(export_opts().with_size(w));
                        },
                    }
                    p { class: "mor-statusbar-muted mr-settings-note",
                        "Aspect 9:16 portrait · 30 fps — fixed for phone reels. "
                        "This size also becomes the default when you export."
                    }
                }
                if settings_tab() == "Platform" {
                    MorSelect {
                        label: "Target platform".to_string(),
                        value: settings().platform.clone(),
                        options: vec!["All platforms".to_string(), "TikTok".to_string(), "Reels".to_string(), "Shorts".to_string()],
                        onchange: move |v: String| {
                            let mut s = settings();
                            s.platform = v;
                            settings.set(s);
                        },
                    }
                    p { class: "mor-statusbar-muted mr-settings-note",
                        if let Some(cap) = platform_cap(&settings().platform) {
                            "Warns once the reel runs past {fmt_t(cap)}. This reel is {fmt_t(total)}."
                        } else {
                            "Warns against every platform's max length. This reel is {fmt_t(total)}."
                        }
                    }
                    MorCheckbox {
                        label: "Show safe-area guides by default".to_string(),
                        checked: settings().guides,
                        onchange: move |on: bool| {
                            let mut s = settings();
                            s.guides = on;
                            settings.set(s);
                            safe_area.set(on);
                        },
                    }
                }
                if settings_tab() == "About" {
                    MorTextInput {
                        label: "Project title".to_string(),
                        value: settings().title.clone(),
                        onchange: move |v: String| {
                            let mut s = settings();
                            s.title = v;
                            settings.set(s);
                        },
                    }
                    MorTextInput {
                        label: "Author".to_string(),
                        value: settings().author.clone(),
                        onchange: move |v: String| {
                            let mut s = settings();
                            s.author = v;
                            settings.set(s);
                        },
                    }
                    p { class: "mor-statusbar-muted mr-settings-note",
                        "Saved with the project. Sources stay referenced by path — the .morreel file is just the edit."
                    }
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn primary",
                        onclick: move |_| show_settings.set(false),
                        "Done"
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
/* --- Aero glass token layer -------------------------------------------------
   Supplements the theme.rs :root tokens. These are the Windows-Aero pillar the
   base palette lacked: translucent panel tints that let a backdrop blur read
   through, chiseled inset/highlight framing, and a soft focus glow. Applied
   only to overlay surfaces (float panel, modals, context menu, deck, sticky
   heads) — the surfaces with live app content behind them for the blur to
   sample. Docked panels stay opaque; there's nothing behind them to frost. */
:root {
  --mor-glass:        color-mix(in srgb, var(--mor-panel) 72%, transparent);
  --mor-glass-strong: color-mix(in srgb, var(--mor-panel) 86%, transparent);
  --mor-glass-blur:        16px;
  --mor-glass-blur-strong: 26px;
  --mor-inset:     inset 0 1px 2px rgba(0, 0, 0, 0.45), inset 0 -1px 0 color-mix(in srgb, white 5%, transparent);
  --mor-highlight: inset 0 1px 0 color-mix(in srgb, white 10%, transparent);
  --mor-focus-glow: 0 0 0 2px color-mix(in srgb, var(--mor-accent-hover) 55%, transparent),
                    0 0 12px color-mix(in srgb, var(--mor-accent) 40%, transparent);
}

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
.mr-root .mor-btn:active:not(:disabled) {
  transform: translateY(0);
  box-shadow: var(--mor-inset);
  filter: brightness(0.94);
}
.mr-root .mor-btn.primary {
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-accent-hover) 92%, white), var(--mor-accent));
  border-color: color-mix(in srgb, var(--mor-accent) 70%, #2e2160);
  color: #160f2b;
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
  background: radial-gradient(circle at 35% 30%, color-mix(in srgb, var(--mor-warning) 35%, white), var(--mor-warning) 55%, color-mix(in srgb, var(--mor-warning) 55%, black));
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
  padding: 0 14px 14px; overflow-y: auto;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 12%, transparent),
    0 8px 28px rgba(0, 0, 0, 0.35),
    0 0 0 1px rgba(0, 0, 0, 0.35);
}
/* Floated inspector: movable + resizable sheet (geometry set inline). */
.mr-inspector.mr-float-panel {
  position: fixed; z-index: 320; flex: none;
  box-sizing: border-box;
  min-width: 280px; min-height: 220px;
  max-width: calc(100vw - 16px); max-height: calc(100vh - 16px);
  padding-bottom: 14px;
  overflow-x: hidden; overflow-y: auto;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-glass-strong) 90%, white), var(--mor-glass-strong));
  backdrop-filter: blur(var(--mor-glass-blur-strong)) saturate(1.35);
  -webkit-backdrop-filter: blur(var(--mor-glass-blur-strong)) saturate(1.35);
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 14%, transparent),
    0 18px 48px rgba(0, 0, 0, 0.55),
    0 0 0 1px color-mix(in srgb, var(--mor-accent) 30%, transparent),
    0 0 36px color-mix(in srgb, var(--mor-accent) 12%, transparent);
  animation: mr-float-in 0.18s ease-out;
}
@keyframes mr-float-in {
  from { opacity: 0; transform: translateY(8px) scale(0.98); }
  to { opacity: 1; transform: none; }
}
.mr-panel-head {
  display: flex; align-items: center; gap: 8px;
  position: sticky; top: 0; z-index: 2;
  margin: 0 -14px 4px; padding: 9px 12px 8px;
  background: linear-gradient(180deg,
    color-mix(in srgb, var(--mor-header) 82%, transparent),
    color-mix(in srgb, var(--mor-header) 74%, transparent));
  backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.2);
  -webkit-backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.2);
  border-bottom: 1px solid color-mix(in srgb, var(--mor-accent) 22%, var(--mor-border));
  border-radius: var(--mor-radius) var(--mor-radius) 0 0;
  user-select: none;
}
.mr-float-panel .mr-panel-head { cursor: grab; }
.mr-float-panel .mr-panel-head:active { cursor: grabbing; }

/* Float resize grips — thin hit targets on edges, gem squares on corners. */
.mr-float-grip {
  position: absolute; z-index: 5;
  background: transparent;
}
.mr-float-grip-n { top: 0; left: 10px; right: 10px; height: 6px; cursor: ns-resize; }
.mr-float-grip-s { bottom: 0; left: 10px; right: 10px; height: 6px; cursor: ns-resize; }
.mr-float-grip-e { top: 10px; right: 0; bottom: 10px; width: 6px; cursor: ew-resize; }
.mr-float-grip-w { top: 10px; left: 0; bottom: 10px; width: 6px; cursor: ew-resize; }
.mr-float-grip-ne, .mr-float-grip-nw, .mr-float-grip-se, .mr-float-grip-sw {
  width: 12px; height: 12px;
  border: 1px solid color-mix(in srgb, var(--mor-accent) 55%, #0f0f12);
  background: linear-gradient(145deg, var(--mor-accent-hover), var(--mor-accent));
  border-radius: 2px;
  box-shadow: 0 1px 3px rgba(0,0,0,0.5), 0 0 6px color-mix(in srgb, var(--mor-accent) 35%, transparent);
  opacity: 0.85;
  transition: opacity 0.12s ease, transform 0.12s ease;
}
.mr-float-grip-ne:hover, .mr-float-grip-nw:hover, .mr-float-grip-se:hover, .mr-float-grip-sw:hover {
  opacity: 1; transform: scale(1.12);
}
.mr-float-grip-ne { top: -1px; right: -1px; cursor: nesw-resize; }
.mr-float-grip-nw { top: -1px; left: -1px; cursor: nwse-resize; }
.mr-float-grip-se { bottom: -1px; right: -1px; cursor: nwse-resize; }
.mr-float-grip-sw { bottom: -1px; left: -1px; cursor: nesw-resize; }
/* Visible SE affordance so resize is discoverable without hunting. */
.mr-float-grip-se::after {
  content: ""; position: absolute; right: 2px; bottom: 2px;
  width: 7px; height: 7px;
  background:
    linear-gradient(135deg, transparent 45%, color-mix(in srgb, #160f2b 70%, transparent) 46%, color-mix(in srgb, #160f2b 70%, transparent) 54%, transparent 55%),
    linear-gradient(135deg, transparent 60%, color-mix(in srgb, #160f2b 55%, transparent) 61%, color-mix(in srgb, #160f2b 55%, transparent) 69%, transparent 70%);
  pointer-events: none;
}
.mr-panel-title {
  flex: 1; min-width: 0;
  font-size: 12px; font-weight: 650; letter-spacing: 0.04em;
  text-transform: uppercase;
  color: color-mix(in srgb, var(--mor-text) 88%, var(--mor-accent));
}
.mr-panel-tools { display: flex; gap: 2px; flex: none; }
.mr-panel-btn {
  width: 28px; height: 24px; padding: 0;
  border: 1px solid transparent; border-radius: 5px;
  background: transparent; color: var(--mor-text-muted);
  font-size: 13px; line-height: 1; cursor: pointer;
  transition: color 0.12s ease, background 0.12s ease, border-color 0.12s ease;
}
.mr-panel-btn:hover {
  color: var(--mor-accent-hover);
  background: color-mix(in srgb, var(--mor-accent) 12%, transparent);
  border-color: color-mix(in srgb, var(--mor-accent) 28%, transparent);
}
.mr-insp-reopen {
  flex: none; align-self: stretch;
  writing-mode: vertical-rl; text-orientation: mixed;
  letter-spacing: 0.08em; text-transform: uppercase;
  font-size: 11px; font-weight: 650;
  padding: 12px 8px; cursor: pointer;
  color: color-mix(in srgb, var(--mor-text) 80%, var(--mor-accent));
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-panel) 90%, white), var(--mor-panel));
  border: 1px solid color-mix(in srgb, var(--mor-accent) 28%, var(--mor-border));
  border-radius: var(--mor-radius);
  box-shadow: inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 10%, transparent);
  transition: color 0.15s ease, border-color 0.15s ease, box-shadow 0.18s ease;
}
.mr-insp-reopen:hover {
  color: var(--mor-accent-hover);
  border-color: var(--mor-accent);
  box-shadow: 0 0 14px color-mix(in srgb, var(--mor-accent) 28%, transparent);
}
/* The title text field is a textarea; inherit the input look, allow wrapping
   and a vertical drag to grow, and keep the editor font rather than monospace. */
.mr-text-area {
  min-height: 3.4em;
  resize: vertical;
  font-family: inherit;
  line-height: 1.35;
  white-space: pre-wrap;
}
.mr-status-click { cursor: pointer; }
.mr-status-click:hover { filter: brightness(1.1); }
.mr-toolbar { display: flex; gap: 8px; flex-wrap: wrap; }
.mr-toolbar .mr-export {
  margin-left: auto; color: var(--mor-warning);
  border-color: color-mix(in srgb, var(--mor-warning) 50%, transparent);
  background: color-mix(in srgb, var(--mor-warning) 8%, var(--mor-btn));
}
.mr-toolbar .mr-export:hover:not(:disabled) {
  border-color: var(--mor-warning);
  box-shadow: 0 0 12px color-mix(in srgb, var(--mor-warning) 30%, transparent);
  color: color-mix(in srgb, var(--mor-warning) 25%, white);
}
.mr-field-row {
  display: flex; align-items: flex-end; gap: 8px;
}
.mr-field-grow { flex: 1; min-width: 0; }
.mr-field-side {
  flex: none; margin-bottom: 1px; font-size: 12px;
  white-space: nowrap;
}
.mr-clip-info h3 { margin: 0 0 4px 0; font-size: 14px; overflow-wrap: anywhere; }
.mr-clip-info .mr-ctx-tag { vertical-align: 2px; }
.mr-clip-info p { margin: 0; font-size: 12px; }
.mr-danger { color: var(--mor-destructive); }
.mr-reset { align-self: flex-start; font-size: 11px; }
.mr-keys { margin-top: auto; font-size: 11px; }

/* Add-to-reel dialog cards. */
.mr-add-dialog { display: flex; flex-direction: column; gap: 12px; }
.mr-add-grid {
  display: grid; grid-template-columns: 1fr 1fr; gap: 10px;
}
.mr-add-card {
  display: flex; flex-direction: column; align-items: flex-start; gap: 4px;
  text-align: left; padding: 12px 12px 14px; cursor: pointer;
  border: 1px solid color-mix(in srgb, var(--mor-accent) 22%, var(--mor-border));
  border-radius: 9px;
  background: linear-gradient(165deg, color-mix(in srgb, var(--mor-btn) 92%, white), var(--mor-btn));
  color: var(--mor-text);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 7%, transparent);
  transition: border-color 0.15s ease, box-shadow 0.18s ease, transform 0.14s ease, filter 0.14s ease;
}
.mr-add-card:hover:not(:disabled) {
  border-color: var(--mor-accent);
  transform: translateY(-2px);
  filter: brightness(1.05);
  box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35), 0 0 14px color-mix(in srgb, var(--mor-accent) 22%, transparent);
}
.mr-add-card:disabled { opacity: 0.45; cursor: not-allowed; }
.mr-add-card strong { font-size: 14px; font-weight: 650; }
.mr-add-card .mor-statusbar-muted { font-size: 11px; line-height: 1.35; }
.mr-add-tag {
  font-size: 9px; font-weight: 700; padding: 1px 6px; border-radius: 3px;
  background: linear-gradient(180deg, var(--mor-accent-hover), var(--mor-accent));
  color: #141417;
}
.mr-add-tag.audio { background: linear-gradient(180deg, #5ee8dc, var(--mor-success)); }
.mr-add-tag.title { background: linear-gradient(180deg, color-mix(in srgb, var(--mor-warning) 72%, white), var(--mor-warning)); }
.mr-add-extra { justify-content: flex-start; }
.mr-fx-dialog { display: flex; flex-direction: column; gap: 8px; }

/* Dialog chrome polish (export / effects / add / about) + browser resize grip. */
.mor-modal {
  display: flex !important;
  flex-direction: column;
  resize: both;
  overflow: hidden !important;
  min-width: 320px !important;
  min-height: 180px;
  max-width: min(96vw, 920px) !important;
  max-height: 90vh;
  width: min(520px, 92vw);
  border: 1px solid color-mix(in srgb, var(--mor-accent) 28%, var(--mor-border-light)) !important;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-glass-strong) 92%, white), var(--mor-glass-strong)) !important;
  backdrop-filter: blur(var(--mor-glass-blur-strong)) saturate(1.35);
  -webkit-backdrop-filter: blur(var(--mor-glass-blur-strong)) saturate(1.35);
  box-shadow:
    0 22px 56px rgba(0, 0, 0, 0.62),
    0 0 0 1px rgba(0, 0, 0, 0.35),
    var(--mor-highlight),
    0 0 40px color-mix(in srgb, var(--mor-accent) 10%, transparent) !important;
}
.mor-modal-header {
  flex: none;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-header) 90%, var(--mor-accent)), var(--mor-header)) !important;
  border-bottom-color: color-mix(in srgb, var(--mor-accent) 22%, var(--mor-border-light)) !important;
  letter-spacing: 0.03em;
}
.mor-modal-body {
  flex: 1 1 auto;
  min-height: 0;
  max-height: none !important;
  overflow-y: auto;
}
.mor-modal-backdrop {
  background:
    radial-gradient(ellipse 70% 50% at 50% 40%, color-mix(in srgb, var(--mor-accent) 8%, transparent), transparent 60%),
    rgba(0, 0, 0, 0.55) !important;
  backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.1);
  -webkit-backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.1);
}

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
  min-height: 280px; max-height: 42vh;
  align-items: flex-start; flex: none;
  user-select: none; -webkit-user-select: none;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, var(--mor-accent) 10%, transparent),
    0 6px 22px rgba(0, 0, 0, 0.4);
}
.mr-timeline-hint { align-self: center; margin: auto; }

/* Phase emphasis on the timeline: dim the lanes the active phase doesn't touch,
   spotlighting the ones it does. V1 is .mr-clips, V2 .mr-lane-v, T .mr-lane-t,
   A1/A2 .mr-lane-a1. Add/Export set no phase class, so nothing dims. */
.mr-lane, .mr-clips { transition: opacity 0.2s ease, filter 0.2s ease; }
.mr-phase-text .mr-lane-v,
.mr-phase-text .mr-clips,
.mr-phase-text .mr-lane-a1,
.mr-phase-audio .mr-lane-t,
.mr-phase-audio .mr-lane-v,
.mr-phase-audio .mr-clips,
.mr-phase-cut .mr-lane-t,
.mr-phase-cut .mr-lane-a1,
.mr-phase-style .mr-lane-t,
.mr-phase-style .mr-lane-a1 {
  opacity: 0.38; filter: saturate(0.55);
}

/* Phase-driven inspector: header + stacked action buttons for Add/Export. */
.mr-phase-head {
  margin: 0 0 8px; font-size: 12px; font-weight: 700;
  letter-spacing: 0.08em; text-transform: uppercase;
  color: var(--mor-accent);
}
.mr-phase-actions { display: flex; flex-direction: column; gap: 8px; }
.mr-phase-actions .mor-btn { width: 100%; }

/* Text workspace: browse the T lane as a pick-to-edit list. */
.mr-text-nav { margin: 2px 0 6px; }
.mr-text-nav .mor-btn { flex: 1; font-size: 12px; }
.mr-text-empty { margin-top: 12px; opacity: 0.8; }
.mr-text-list { display: flex; flex-direction: column; gap: 6px; margin-top: 14px; }
.mr-text-row {
  display: flex; align-items: center; gap: 8px; width: 100%;
  padding: 8px 10px; text-align: left; cursor: pointer;
  border: 1px solid color-mix(in srgb, var(--mor-accent) 18%, var(--mor-border));
  border-radius: 8px;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-btn) 92%, white), var(--mor-btn));
  color: var(--mor-text);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 6%, transparent);
  transition: border-color 0.15s ease, box-shadow 0.18s ease, transform 0.14s ease, filter 0.14s ease;
}
.mr-text-row:hover {
  border-color: var(--mor-accent);
  transform: translateX(2px);
  filter: brightness(1.05);
  box-shadow: 0 2px 10px rgba(0, 0, 0, 0.3), 0 0 12px color-mix(in srgb, var(--mor-accent) 22%, transparent);
}
.mr-text-row-label {
  flex: 1; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px;
}
.mr-text-row-time {
  flex: none; font-size: 11px; color: var(--mor-text-muted);
  font-variant-numeric: tabular-nums;
}
/* Style sub-tabs sit above the panel's controls with a little room. */
.mr-inspector .mor-tabs { margin: 4px 0 12px; }
/* Band presets + background swatches. */
.mr-preset-row { display: flex; gap: 6px; flex-wrap: wrap; margin: 6px 0 12px; }
.mr-preset-row .mor-btn { flex: 1; min-width: 68px; }
.mr-bg-swatches { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 8px; }
.mr-bg-swatch {
  flex: 1; min-width: 66px; height: 58px; padding: 5px;
  border: 2px solid var(--mor-border); border-radius: 8px; cursor: pointer;
  display: flex; align-items: flex-end; justify-content: center;
  transition: border-color 0.15s ease, box-shadow 0.15s ease;
}
.mr-bg-swatch:hover { border-color: color-mix(in srgb, var(--mor-accent) 60%, transparent); }
.mr-bg-swatch.active {
  border-color: var(--mor-accent);
  box-shadow: 0 0 10px color-mix(in srgb, var(--mor-accent) 45%, transparent);
}
.mr-bg-name {
  font-size: 10px; background: rgba(0, 0, 0, 0.55); color: #fff;
  padding: 1px 6px; border-radius: 4px;
}

/* Workflow spine: the reel-building phases as a bottom bar. */
.mr-workflow {
  display: flex; justify-content: center; gap: 4px; flex: none;
  padding: 5px 8px; margin-top: 6px;
  background: rgba(127, 127, 127, 0.05);
  border: 1px solid var(--mor-border);
  border-radius: var(--mor-radius);
  box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.2);
}
.mr-wf {
  position: relative;
  display: flex; flex-direction: column; align-items: center; gap: 3px;
  min-width: 76px; padding: 5px 12px;
  background: transparent; border: 1px solid transparent; border-radius: 7px;
  color: var(--mor-text-muted); cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease, border-color 0.15s ease;
}
.mr-wf:hover:not(:disabled) { background: rgba(127, 127, 127, 0.09); color: var(--mor-text); }
.mr-wf:disabled { opacity: 0.4; cursor: not-allowed; }
.mr-wf.active {
  color: var(--mor-text);
  border-color: color-mix(in srgb, var(--mor-accent) 50%, transparent);
  background: color-mix(in srgb, var(--mor-accent) 15%, transparent);
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 10%, transparent);
}
.mr-wf-export {
  color: color-mix(in srgb, var(--mor-accent) 85%, white);
  border-color: color-mix(in srgb, var(--mor-accent) 40%, transparent);
}
.mr-wf-icon { font-size: 16px; line-height: 1; }
.mr-wf-label { font-size: 11px; letter-spacing: 0.03em; }
.mr-wf-tick {
  position: absolute; top: 2px; right: 7px;
  font-size: 9px; color: var(--mor-success);
  text-shadow: 0 0 6px color-mix(in srgb, var(--mor-success) 50%, transparent);
}
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
.mr-lane-tag.title { background: linear-gradient(180deg, color-mix(in srgb, var(--mor-warning) 72%, white), var(--mor-warning)); }
.mr-lane-a1 .mr-lane-tag { background: linear-gradient(180deg, #5ee8dc, var(--mor-success)); }
/* Taller audio lanes so the waveform envelope has room to read and is an easier
   drag/trim target. */
.mr-lane-a1 { height: 76px; }
.mr-audio-fx {
  position: absolute; top: 2px; right: 4px; z-index: 2;
  font-size: 9px; color: var(--mor-accent-hover);
  text-shadow: 0 0 6px color-mix(in srgb, var(--mor-accent) 50%, transparent);
  pointer-events: none;
}

/* On-lane audio handles: fade wedges, corner grips, a volume line. */
.mr-fade-in, .mr-fade-out {
  position: absolute; top: 0; bottom: 0; z-index: 2; pointer-events: none;
}
.mr-fade-in { left: 0; background: linear-gradient(to right, rgba(0,0,0,0.6), transparent); }
.mr-fade-out { right: 0; background: linear-gradient(to left, rgba(0,0,0,0.6), transparent); }
.mr-fade-grip {
  position: absolute; top: 0; bottom: 0; width: 9px; z-index: 4;
  cursor: ew-resize; opacity: 0; transition: opacity 0.15s ease;
  background: color-mix(in srgb, var(--mor-success) 70%, white);
  box-shadow: 0 0 6px color-mix(in srgb, var(--mor-success) 60%, transparent);
}
.mr-fade-grip.in { left: 0; border-radius: 5px 0 0 5px; }
.mr-fade-grip.out { right: 0; border-radius: 0 5px 5px 0; }
.mr-lane-item.audio:hover .mr-fade-grip { opacity: 0.55; }
.mr-fade-grip:hover { opacity: 0.95 !important; }
.mr-len-grip {
  position: absolute; top: 0; bottom: 0; right: 0; width: 9px; z-index: 4;
  cursor: ew-resize; opacity: 0; transition: opacity 0.15s ease;
  border-radius: 0 5px 5px 0;
  background: color-mix(in srgb, var(--mor-title, gold) 75%, white);
  box-shadow: 0 0 6px color-mix(in srgb, var(--mor-title, gold) 60%, transparent);
}
.mr-lane-item.title:hover .mr-len-grip { opacity: 0.6; }
.mr-len-grip:hover { opacity: 0.95 !important; }
.mr-vol-line {
  position: absolute; left: 0; right: 0; height: 3px; z-index: 3;
  margin-bottom: -1.5px; cursor: ns-resize;
  background: color-mix(in srgb, var(--mor-warning) 85%, white);
  box-shadow: 0 0 5px color-mix(in srgb, var(--mor-warning) 70%, transparent);
  opacity: 0.35; transition: opacity 0.15s ease;
}
.mr-lane-item.audio:hover .mr-vol-line { opacity: 0.85; }
.mr-vol-line:hover { opacity: 1; height: 4px; }

/* Track mixer strip in the Audio inspector. */
.mr-mixer { display: flex; flex-direction: column; gap: 8px; margin-bottom: 6px; }
.mr-mixer-row { display: flex; align-items: center; gap: 8px; }
.mr-mixer-tag {
  flex: none; width: 26px; font-size: 10px; font-weight: 600; letter-spacing: 0.5px;
  color: var(--mor-text-muted); text-align: center;
}
.mr-mixer-fader { flex: 1; min-width: 0; }
.mr-mixer-btn {
  flex: none; width: 22px; height: 20px; border-radius: 4px; font-size: 10px;
  font-weight: 700; cursor: pointer; line-height: 1;
  border: 1px solid var(--mor-border-light);
  background: color-mix(in srgb, var(--mor-panel) 88%, white);
  color: var(--mor-text-muted);
}
.mr-mixer-btn.on.m {
  background: var(--mor-danger); border-color: var(--mor-danger); color: #140b0b;
}
.mr-mixer-btn.on.s {
  background: var(--mor-warning); border-color: var(--mor-warning); color: #141002;
}
.mr-mixer-val { flex: none; width: 34px; font-size: 10px; text-align: right; color: var(--mor-text-muted); }

/* Inspector waveform: whole source, kept window bright, trims/fades shaded. */
.mr-insp-wave {
  position: relative; height: 46px; width: 100%; margin: 6px 0 2px;
  border-radius: 5px; background-repeat: no-repeat; background-size: 100% 100%;
  background-color: color-mix(in srgb, #061412 75%, var(--mor-success));
  box-shadow:
    inset 0 0 0 1px color-mix(in srgb, var(--mor-success) 35%, transparent),
    inset 0 -6px 10px color-mix(in srgb, var(--mor-success) 18%, transparent);
}
.mr-insp-trim { position: absolute; top: 0; bottom: 0; background: rgba(0,0,0,0.58); }
.mr-insp-fadein { position: absolute; top: 0; bottom: 0; background: linear-gradient(to right, rgba(0,0,0,0.5), transparent); }
.mr-insp-fadeout { position: absolute; top: 0; bottom: 0; background: linear-gradient(to left, rgba(0,0,0,0.5), transparent); }

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
  border-color: color-mix(in srgb, var(--mor-success) 55%, transparent);
  /* Dark bed so the teal wave stands out; image sits on top via wave_css. */
  background-color: color-mix(in srgb, #061412 88%, var(--mor-success));
  background-image: linear-gradient(180deg, color-mix(in srgb, var(--mor-success) 22%, transparent), color-mix(in srgb, var(--mor-success) 8%, transparent));
  line-height: 74px;
  padding-left: 32px;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.85);
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, white 10%, transparent),
    inset 0 -10px 18px color-mix(in srgb, var(--mor-success) 12%, transparent),
    0 1px 2px rgba(0, 0, 0, 0.3);
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
  height: 28px; flex: none; border-radius: 4px;
  background-color: color-mix(in srgb, #061412 75%, var(--mor-success));
  box-shadow:
    inset 0 0 0 1px color-mix(in srgb, var(--mor-success) 35%, transparent),
    inset 0 -6px 10px color-mix(in srgb, var(--mor-success) 18%, transparent);
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
  background: var(--mor-glass) !important;
  backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.3);
  -webkit-backdrop-filter: blur(var(--mor-glass-blur)) saturate(1.3);
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5), var(--mor-highlight), 0 0 0 1px rgba(0, 0, 0, 0.3) !important;
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
.mr-ctx-tag.title { background: linear-gradient(180deg, color-mix(in srgb, var(--mor-warning) 72%, white), var(--mor-warning)); }
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
.mr-settings-dialog { display: flex; flex-direction: column; gap: 12px; min-width: 360px; }
.mr-settings-note { margin: -4px 0 2px; font-size: 12px; }
.mr-settings-dialog .mr-toolbar { justify-content: flex-end; margin-top: 4px; }
.mr-keys-dialog { display: flex; flex-direction: column; gap: 8px; min-width: 340px; }
.mr-keys-opt { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.mr-keys-name { font-weight: 600; }
.mr-keys-dialog .mr-toolbar { justify-content: flex-end; margin-top: 6px; }

.mr-shortcut-table { border-collapse: collapse; width: 100%; font-size: 13px; }
.mr-shortcut-table td { padding: 4px 10px 4px 0; }
.mr-key { color: var(--mor-accent-hover); white-space: nowrap; }

@media (max-width: 700px) {
  .mr-work { flex-direction: column; }
  .mr-phone { flex: none; width: auto; height: 45vh; }
  .mr-inspector { min-width: 0; }
  .mr-inspector.mr-float-panel {
    max-width: calc(100vw - 16px);
    max-height: min(60vh, 520px);
  }
  .mr-add-grid { grid-template-columns: 1fr; }
  .mr-insp-reopen {
    writing-mode: horizontal-tb; align-self: stretch;
    padding: 8px 12px;
  }
}

.mr-root button:focus-visible,
.mr-root input:focus-visible,
.mr-fx-tile:focus-visible,
.mr-tab:focus-visible,
.mr-panel-btn:focus-visible,
.mr-add-card:focus-visible,
.mr-insp-reopen:focus-visible {
  outline: 1px solid color-mix(in srgb, var(--mor-accent-hover) 70%, transparent);
  outline-offset: 2px;
  box-shadow: var(--mor-focus-glow);
}

@media (prefers-reduced-motion: reduce) {
  .mr-clip, .mr-lane-item, .mr-fx-tile, .mr-root .mor-btn, .mr-add-card,
  .mr-progress > div, .mr-deck, .mr-tab, .mr-phone, .mr-xf-h, .mr-panel-btn,
  .mr-float-grip-ne, .mr-float-grip-nw, .mr-float-grip-se, .mr-float-grip-sw {
    transition: none !important; animation: none !important;
  }
  .mr-drop, .mr-deck.playing, .mr-progress > div, .mr-float-panel { animation: none !important; }
  .mr-text-row { transition: none !important; }
  .mr-root .mor-btn:hover:not(:disabled),
  .mr-fx-tile:hover, .mr-add-card:hover:not(:disabled),
  .mr-text-row:hover { transform: none; }
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
            transform: engine::AnimatedTransform::default(),
            grade: engine::Grade::default(),
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
    fn builtin_styles_use_real_palette_values_and_keep_the_words() {
        let colors: Vec<&str> = TITLE_COLORS.iter().map(|(n, _)| *n).collect();
        let bevels: Vec<&str> = BEVELS.iter().map(|(v, _)| *v).collect();
        let positions: Vec<&str> = TITLE_POS.iter().map(|(n, _)| *n).collect();
        let builtins = builtin_title_styles();
        assert!(builtins.len() >= 4, "expected a real gallery");
        for p in &builtins {
            // A typo here would silently fall back to white / no bevel / mid.
            assert!(colors.contains(&p.style.color.as_str()), "{}: bad colour {}", p.name, p.style.color);
            assert!(colors.contains(&p.style.outline_color.as_str()), "{}: bad outline colour", p.name);
            assert!(bevels.contains(&p.style.bevel.as_str()), "{}: bad bevel {}", p.name, p.style.bevel);
            assert!(positions.contains(&p.style.pos.as_str()), "{}: bad position {}", p.name, p.style.pos);
        }
        // Applying a style keeps the card's own words and timing, takes the look.
        let card = TitleItem { text: "Keep me".into(), at: 4.0, dur: 9.0, ..base_title() };
        let styled = restyle(&card, &builtins[0].style);
        assert_eq!(styled.text, "Keep me");
        assert_eq!((styled.at, styled.dur), (4.0, 9.0));
        assert_eq!(styled.outline, builtins[0].style.outline);
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
        assert_eq!(route_drop(Kind::Audio, Lane::A2), Ok((Lane::A2, None)));

        // Sound aimed at a video lane still goes to A1, and says so.
        assert_eq!(route_drop(Kind::Audio, Lane::V1), Ok((Lane::A1, Some("audio goes to A1"))));
        assert_eq!(route_drop(Kind::Audio, Lane::V2), Ok((Lane::A1, Some("audio goes to A1"))));

        // A video on an audio lane contributes its soundtrack rather than being refused.
        assert_eq!(
            route_drop(Kind::Video, Lane::A1),
            Ok((Lane::A1, Some("using its soundtrack")))
        );
        assert_eq!(
            route_drop(Kind::Video, Lane::A2),
            Ok((Lane::A2, Some("using its soundtrack")))
        );
        // A photo genuinely has nothing to give an audio track.
        assert!(route_drop(Kind::Still, Lane::A1).is_err());
        assert!(route_drop(Kind::Still, Lane::A2).is_err());
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
        let all = "All platforms";
        assert_eq!(over_limits(30.0, all), None);
        assert_eq!(over_limits(60.0, all), None); // exactly at the cap still fits
        let w = over_limits(75.0, all).unwrap();
        assert!(w.contains("Shorts") && !w.contains("Reels"), "{w}");
        let w = over_limits(120.0, all).unwrap();
        assert!(w.contains("Shorts") && w.contains("Reels") && !w.contains("TikTok"), "{w}");
        assert!(over_limits(1200.0, all).unwrap().contains("TikTok"));
    }

    #[test]
    fn a_chosen_platform_only_warns_against_its_own_cap() {
        // 75s is over Shorts (60) but under Reels (90): targeting Reels stays quiet.
        assert_eq!(over_limits(75.0, "Reels"), None);
        assert!(over_limits(75.0, "Shorts").unwrap().contains("Shorts"));
        // A single target never names another platform.
        let w = over_limits(1200.0, "Shorts").unwrap();
        assert!(w.contains("Shorts") && !w.contains("TikTok"), "{w}");
    }

    #[test]
    fn phase_follows_selection() {
        use Phase::*;
        // Selecting an item jumps to its phase.
        assert_eq!(phase_for_selection(Some(Sel::Main(0)), Cut), Cut);
        assert_eq!(phase_for_selection(Some(Sel::Over(0)), Cut), Cut);
        assert_eq!(phase_for_selection(Some(Sel::Title(0)), Cut), Text);
        assert_eq!(phase_for_selection(Some(Sel::Aud(0)), Cut), Audio);
        // Browsing clips while grading stays in Style, not kicked back to Cut.
        assert_eq!(phase_for_selection(Some(Sel::Main(1)), Style), Style);
        // An empty selection keeps the current phase.
        assert_eq!(phase_for_selection(None, Export), Export);
    }

    #[test]
    fn dirty_tracks_disk_state_not_runtime_fields() {
        let empty = Snapshot { clips: vec![], overlays: vec![], audios: vec![], titles: vec![], markers: vec![], mixer: Mixer::default() };
        // Never saved: empty is clean, any content is unsaved.
        assert!(!timeline_dirty(&empty, None));
        let mut one = empty.clone();
        one.clips.push(clip(0.0, 2.0));
        assert!(timeline_dirty(&one, None));

        // Saved baseline equal to current → clean.
        let base = serde_json::to_string(&one).unwrap();
        assert!(!timeline_dirty(&one, Some(&base)));

        // A background proxy/waveform landing (skip fields) is NOT an edit.
        let mut hydrated = one.clone();
        hydrated.clips[0].proxy = "/cache/x.mp4".to_string();
        hydrated.clips[0].wave = "data:image/png;base64,AAAA".to_string();
        assert!(!timeline_dirty(&hydrated, Some(&base)), "runtime-only fields must not mark dirty");

        // A real edit (a trim) is dirty.
        let mut edited = one.clone();
        edited.clips[0].out_s = 1.0;
        assert!(timeline_dirty(&edited, Some(&base)));
    }

    #[test]
    fn key_schemes_round_trip_and_remap_only_the_blade() {
        // Persistence id round-trips; an unknown id falls back to the default.
        for k in KeyScheme::ALL {
            assert_eq!(KeyScheme::from_id(k.id()), k);
        }
        assert_eq!(KeyScheme::from_id("nonsense"), KeyScheme::MorReel);

        // Each editor spells the blade differently — that's the whole point.
        let splits: Vec<&str> = KeyScheme::ALL.iter().map(|k| k.split()).collect();
        assert_eq!(splits, vec!["S", "Ctrl+\\", "Ctrl+K", "Ctrl+B"]);

        // The help table only swaps the blade row; everything else is scheme-blind.
        assert_eq!(help_key("S", "Split at playhead", KeyScheme::Premiere), "Ctrl+K");
        assert_eq!(help_key("Space", "Play / pause", KeyScheme::Premiere), "Space");
    }

    #[test]
    fn otio_export_is_valid_and_carries_clips_and_captions() {
        let mut c = clip(0.0, 2.0);
        c.path = "/media/shot.mp4".to_string();
        c.name = "shot".to_string();
        let mut title: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        title.text = "hello world".to_string();
        title.at = 1.0;
        title.dur = 2.0;
        let snap = Snapshot {
            clips: vec![c],
            overlays: vec![],
            audios: vec![],
            titles: vec![title],
            markers: vec![],
            mixer: Mixer::default(),
        };

        let doc: serde_json::Value = serde_json::from_str(&snapshot_to_otio(&snap, "reel", 30.0)).unwrap();
        assert_eq!(doc["OTIO_SCHEMA"], "Timeline.1");
        let tracks = &doc["tracks"]["children"];
        assert_eq!(tracks.as_array().unwrap().len(), 5); // V1 V2 A1 A2 Captions

        // V1's clip references the media by a file:// URL at its source span.
        let v1_clip = &tracks[0]["children"][0];
        assert_eq!(v1_clip["OTIO_SCHEMA"], "Clip.1");
        assert_eq!(v1_clip["media_reference"]["target_url"], "file:///media/shot.mp4");
        assert_eq!(v1_clip["source_range"]["duration"]["value"], 60.0); // 2s @ 30fps

        // The caption lands on the Captions track, gapped to its start at 1s.
        let caps = &tracks[4]["children"];
        assert_eq!(caps[0]["OTIO_SCHEMA"], "Gap.1"); // 0..1s
        assert_eq!(caps[1]["name"], "hello world");
        assert_eq!(caps[1]["media_reference"]["OTIO_SCHEMA"], "MissingReference.1");
    }

    #[test]
    fn layouts_round_trip_and_presets_are_distinct_arrangements() {
        let all = preset_layouts().to_vec();
        let back: Vec<Layout> = serde_json::from_str(&serde_json::to_string(&all).unwrap()).unwrap();
        assert_eq!(back, all); // a layout is just a (de)serialized blob
        // The presets are real arrangement changes, not relabels: Focus hides the
        // inspector, Editing docks it, Floating floats it.
        let get = |n: &str| all.iter().find(|l| l.name == n).unwrap().clone();
        assert!(!get("Focus").inspector_open);
        assert!(get("Editing").inspector_open && !get("Editing").inspector_float);
        assert!(get("Floating").inspector_float);
    }

    #[test]
    fn a_pre_settings_project_still_loads() {
        // A bare Snapshot (no "settings" key) must deserialize into Project via
        // flatten, falling back to default settings — old files keep opening.
        let bare = r#"{"clips":[],"overlays":[],"audios":[],"titles":[]}"#;
        let p: Project = serde_json::from_str(bare).unwrap();
        assert_eq!(p.settings, ProjectSettings::default());
        // And a round-trip preserves settings.
        let json = serde_json::to_string(&Project {
            snap: p.snap,
            settings: ProjectSettings { platform: "Reels".into(), ..Default::default() },
        })
        .unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.settings.platform, "Reels");
    }

    #[test]
    fn project_round_trips_without_derived_data() {
        let mut c = clip(1.0, 3.0);
        c.speed = 1.5;
        c.volume = 0.25;
        c.thumb = "data:image/jpeg;base64,AAAA".to_string(); // derived, must not persist
        c.proxy = "/cache/proxy.mp4".to_string();
        let snap = Snapshot { clips: vec![c], overlays: vec![], audios: vec![], titles: vec![], markers: vec![2.5], mixer: Mixer::default() };

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
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (335.0, 240.0), RECT, false);
        assert!((t.scale - 2.0).abs() < 1e-9, "scale = {}", t.scale);
        // Half as far is half the size.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (185.0, 240.0), RECT, false);
        assert!((t.scale - 0.5).abs() < 1e-9, "scale = {}", t.scale);
        // Clamped to the slider's range rather than collapsing to nothing.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (135.5, 240.0), RECT, false);
        assert!(t.scale >= 0.1, "scale should not collapse: {}", t.scale);
        // A grab that starts on the centre has no distance to work from.
        assert_eq!(xf_apply(XfGrab::Scale, start, (135.0, 240.0), (200.0, 240.0), RECT, false).scale, 1.0);
    }

    #[test]
    fn dragging_the_knob_rotates_and_stays_in_slider_range() {
        let start = engine::Transform::default();
        // From straight up to straight right is a quarter turn clockwise.
        let t = xf_apply(XfGrab::Rotate, start, (135.0, 40.0), (235.0, 240.0), RECT, false);
        assert!((t.rotation - 90.0).abs() < 1e-6, "rotation = {}", t.rotation);
        // Rotation always lands inside what the slider can show.
        for (to_x, to_y) in [(35.0, 240.0), (135.0, 440.0), (200.0, 100.0), (60.0, 300.0)] {
            let r = xf_apply(XfGrab::Rotate, start, (135.0, 40.0), (to_x, to_y), RECT, false).rotation;
            assert!((-180.0..=180.0).contains(&r), "rotation {r} is outside the slider");
        }
    }

    #[test]
    fn a_side_handle_stretches_one_axis_only() {
        let start = engine::Transform::default();
        // Dragging the right edge twice as far from the centre doubles the
        // width and leaves the height exactly alone.
        let t = xf_apply(XfGrab::StretchX, start, (235.0, 240.0), (335.0, 240.0), RECT, false);
        assert!((t.scale_x - 2.0).abs() < 1e-9, "scale_x = {}", t.scale_x);
        assert_eq!(t.scale_y, 1.0, "a sideways drag must not change the height");
        assert_eq!(t.scale, 1.0, "the master scale stays put");

        // Vertical drags measure along their own axis, so moving sideways
        // while dragging a top handle does nothing.
        let t = xf_apply(XfGrab::StretchY, start, (135.0, 340.0), (135.0, 290.0), RECT, false);
        assert!((t.scale_y - 0.5).abs() < 1e-9, "scale_y = {}", t.scale_y);
        assert_eq!(t.scale_x, 1.0);
        let sideways = xf_apply(XfGrab::StretchY, start, (135.0, 340.0), (300.0, 340.0), RECT, false);
        assert_eq!(sideways.scale_y, 1.0, "a horizontal drag should not stretch downward");

        // A corner still keeps the shape: it moves the master scale, not an axis.
        let t = xf_apply(XfGrab::Scale, start, (235.0, 240.0), (335.0, 240.0), RECT, false);
        assert!((t.scale - 2.0).abs() < 1e-9);
        assert_eq!((t.scale_x, t.scale_y), (1.0, 1.0));
    }

    #[test]
    fn shift_snaps_rotation_to_fifteen_degrees() {
        let start = engine::Transform::default();
        // A drag that lands near 90° goes to exactly 90° when snapping.
        let from = (135.0, 40.0);
        let to = (233.0, 245.0); // a few degrees shy of a quarter turn
        let free = xf_apply(XfGrab::Rotate, start, from, to, RECT, false).rotation;
        let snapped = xf_apply(XfGrab::Rotate, start, from, to, RECT, true).rotation;
        assert!((free - 90.0).abs() > 0.5, "the test drag should not already be square: {free}");
        assert!((snapped % 15.0).abs() < 1e-6, "snapped to {snapped}, not a multiple of 15");
        assert!((snapped - 90.0).abs() < 1e-6, "should land on the nearest quarter turn: {snapped}");
        // Snapping still respects the slider's range.
        for to in [(35.0, 240.0), (135.0, 440.0), (60.0, 300.0)] {
            let r = xf_apply(XfGrab::Rotate, start, from, to, RECT, true).rotation;
            assert!((-180.0..=180.0).contains(&r), "{r} is outside the slider");
        }
    }

    #[test]
    fn stretching_moves_the_handles_with_the_box() {
        // Wide and short: the corners spread across and pull in vertically.
        let t = engine::Transform { scale_x: 2.0, scale_y: 0.5, ..Default::default() };
        let c = xf_corners(&t);
        assert_eq!(c[0], (-0.5, 0.25), "top-left of a box twice as wide, half as tall");
        assert_eq!(c[2], (1.5, 0.75));

        // Side handles sit at the midpoints of that same box.
        let e = xf_edges(&t);
        assert_eq!(e[0], (-0.5, 0.5), "left edge");
        assert_eq!(e[1], (1.5, 0.5), "right edge");
        assert_eq!(e[2], (0.5, 0.25), "top edge");
        assert_eq!(e[3], (0.5, 0.75), "bottom edge");

        // Rotated, every handle stays glued to the box rather than drifting.
        let spun = engine::Transform { rotation: 90.0, ..t };
        let (ec, ee) = (xf_corners(&spun), xf_edges(&spun));
        assert!(ec.iter().all(|(x, y)| x.is_finite() && y.is_finite()));
        assert!((ee[0].0 - 0.5).abs() < 1e-6, "a quarter turn puts the left edge above centre");
    }

    #[test]
    fn mirroring_is_not_the_identity() {
        let mut t = engine::Transform::default();
        assert!(t.is_identity());
        t.flip_h = true;
        assert!(!t.is_identity(), "a mirrored clip must still emit a filter");
        assert!(!engine::transform_chain(&t, engine::W, engine::H, false).is_empty());
        let stretched = engine::Transform { scale_x: 1.5, ..Default::default() };
        assert!(!stretched.is_identity(), "a stretched clip must still emit a filter");
    }

    #[test]
    fn a_zero_sized_monitor_never_produces_nonsense() {
        // The rect is measured asynchronously; a drag must degrade to a no-op
        // rather than divide by zero if it somehow arrives empty.
        let start = engine::Transform { scale: 1.5, x: 0.2, ..Default::default() };
        for grab in [XfGrab::Move, XfGrab::Scale, XfGrab::StretchX, XfGrab::StretchY, XfGrab::Rotate] {
            let t = xf_apply(grab, start, (10.0, 10.0), (99.0, 99.0), (0.0, 0.0, 0.0, 0.0), false);
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
        assert_eq!(
            (t.scale, t.scale_x, t.scale_y, t.x, t.y, t.rotation, t.opacity),
            (1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0)
        );
        // V1 has nothing underneath it, so opacity is not offered there.
        assert_eq!(transform_knobs(&t, false).len(), 6);
        assert_eq!(transform_knobs(&t, true).len(), 7);
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
        c.transform.set_pose(engine::Transform { scale: 0.5, ..Default::default() });
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
