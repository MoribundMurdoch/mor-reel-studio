// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — portrait-only (9:16) video editor.
// V1: main clip track (trim/reorder/split, ripple by construction).
// V2: full-frame cutaway overlays. A1: audio mixed under. Effects per video item.

mod bevel;
mod coords;
mod engine;
mod hub;
mod keyframe;
mod plugin;

use dioxus::desktop::tao::window::Icon;
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use engine::{AudioSpec, ClipSpec, OverlaySpec, TitleSpec};
use futures_util::StreamExt; // rx.next() in the live-control coroutine
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
        // Swallow keystrokes that originate in a text field before Dioxus's
        // delegated (bubbling) listener on the app root can turn them into
        // shortcuts — so typing a title never plays, deletes, or switches
        // workspaces. A head <script> is guaranteed to run at load, unlike a
        // use_effect eval; the capture-phase window listener fires before the
        // root's bubble listener, and stopPropagation leaves typing intact.
        .with_custom_head(
            r#"<script>
            if (!window.__morShortcutGuard) {
              window.__morShortcutGuard = true;
              function morEditable(n) {
                if (!n) return false;
                var tag = n.tagName;
                return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || n.isContentEditable;
              }
              window.addEventListener('keydown', function (e) {
                // Check the focused element too, not just the event target: as
                // long as a text field owns focus, no keystroke is a shortcut.
                if (morEditable(document.activeElement) || morEditable(e.target)) {
                  e.stopPropagation();
                }
              }, true);
            }
            </script>"#
                .to_string(),
        )
        .with_window(
            WindowBuilder::new()
                .with_title("MorReel Studio")
                .with_inner_size(LogicalSize::new(1100.0, 860.0))
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
    ("Color", "Noir", "hue=s=0,eq=contrast=1.5:brightness=-0.02"),
    ("Color", "Negative", "negate"),
    ("Color", "X-Ray", "negate,hue=s=0,eq=contrast=1.3"),
    ("Color", "Matrix", "colorchannelmixer=0.3:0.6:0.1:0:0.25:0.7:0.05:0:0.2:0.5:0.1,eq=contrast=1.2"),
    ("Look", "Dreamy", "gblur=sigma=2,eq=brightness=0.04:saturation=1.15"),
    ("Look", "Vignette", "vignette"),
    ("Look", "Vintage", "curves=preset=vintage"),
    ("Look", "Cross process", "curves=preset=cross_process"),
    ("Look", "Faded", "curves=preset=lighter,eq=saturation=0.82"),
    ("Look", "Golden hour", "colortemperature=3800,eq=saturation=1.2:brightness=0.02"),
    ("Look", "Blockbuster", "colorbalance=rs=.12:gs=.02:bs=-.12:rh=-.06:bh=.12,eq=saturation=1.15"),
    ("Look", "Bleach bypass", "eq=saturation=0.35:contrast=1.35:brightness=0.02"),
    ("Look", "Film grain", "noise=alls=18:allf=t"),
    ("Look", "Ink", "edgedetect=mode=colormix:high=0"),
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
    // Keyers — produce alpha, so on a V2 overlay V1 shows through the keyed area,
    // and on a V1 clip the frame background does. chromakey works in YUV; colorkey/
    // lumakey need rgba first. The Effect-strength slider drives the key tolerance
    // (see effect_filter_amt), moranima's background-removal sensitivity analog.
    ("Key", "Green screen", "chromakey=0x00d400:0.13:0.07"),
    ("Key", "Blue screen", "chromakey=0x2b6fdf:0.13:0.07"),
    // Drop black/white suit particle & light-leak plates shot on a flat backdrop.
    ("Key", "Drop black", "format=rgba,colorkey=0x000000:0.16:0.10"),
    ("Key", "Drop white", "format=rgba,colorkey=0xffffff:0.16:0.10"),
    ("Key", "Luma key", "format=rgba,lumakey=threshold=0.18:tolerance=0.12"),
];

/// Compositing blend modes for a V2 image/particle layer — the moranima look for
/// light leaks and glowing particles, which brighten V1 rather than cover it.
/// (label, ffmpeg `blend=all_mode` value); "Normal" is the default alpha-over.
const BLEND_MODES: &[(&str, &str)] = &[
    ("Normal", ""),
    ("Screen", "screen"),
    ("Add", "addition"),
    ("Lighten", "lighten"),
    ("Multiply", "multiply"),
    ("Overlay", "overlay"),
];

/// Effects contributed by installed+enabled `bundle` plugins from the hub, held in
/// a process-global the free effect functions can read (they aren't in the reactive
/// scope). Set once at startup and re-set when the Plugin Hub changes a bundle.
/// ponytail: a global `RwLock<Vec>`, not a signal — the lookup is a free fn; the
/// UI bumps a `hub_gen` signal separately to re-render the picker.
static HUB_EFFECTS: std::sync::OnceLock<std::sync::RwLock<Vec<(String, String, String)>>> = std::sync::OnceLock::new();

fn hub_effects() -> &'static std::sync::RwLock<Vec<(String, String, String)>> {
    HUB_EFFECTS.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}

/// Replace the active hub bundle effects (called after a hub install/enable).
fn set_hub_effects(effects: Vec<(String, String, String)>) {
    *hub_effects().write().unwrap() = effects;
}

/// Built-in effects plus every active hub bundle effect — what the picker lists.
/// A bundle name that collides with a built-in loses (built-ins win the lookup).
fn all_effects() -> Vec<(String, String, String)> {
    let mut v: Vec<_> = EFFECTS.iter().map(|(c, n, f)| (c.to_string(), n.to_string(), f.to_string())).collect();
    for e in hub_effects().read().unwrap().iter() {
        if !v.iter().any(|(_, n, _)| n == &e.1) {
            v.push(e.clone());
        }
    }
    v
}

/// Whether `name` is a keyer (the "Key" family) — used for the Effects tab's
/// done-tick and to keep keyers out of the Style look palette.
fn is_keyer(name: &str) -> bool {
    EFFECTS.iter().any(|(cat, n, _)| *cat == "Key" && *n == name)
}

/// The ffmpeg snippet for a named effect: built-ins first, then hub bundle effects.
/// Returns `String` (not `&'static str`) because a hub effect is owned at runtime.
fn effect_filter(name: &str) -> String {
    if let Some((_, _, f)) = EFFECTS.iter().find(|(_, n, _)| *n == name) {
        return f.to_string();
    }
    hub_effects().read().unwrap().iter().find(|(_, n, _)| n == name).map(|(_, _, f)| f.clone()).unwrap_or_default()
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
        "Noir" => format!("hue=s={:.3},eq=contrast={:.3}:brightness={:.3}", 1.0 - a, 1.0 + 0.5 * a, -0.02 * a),
        "Golden hour" => format!("colortemperature={:.0},eq=saturation={:.3}:brightness={:.3}", 6500.0 - 2700.0 * a, 1.0 + 0.2 * a, 0.02 * a),
        "Blockbuster" => format!("colorbalance=rs={:.3}:gs={:.3}:bs={:.3}:rh={:.3}:bh={:.3},eq=saturation={:.3}", 0.12 * a, 0.02 * a, -0.12 * a, -0.06 * a, 0.12 * a, 1.0 + 0.15 * a),
        "Bleach bypass" => format!("eq=saturation={:.3}:contrast={:.3}:brightness={:.3}", 1.0 - 0.65 * a, 1.0 + 0.35 * a, 0.02 * a),
        "Film grain" => format!("noise=alls={:.0}:allf=t", 18.0 * a),
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
        // Keyers: strength = how much colour spread the key eats. similarity spans
        // a tight 0.03 to a loose 0.30; blend trails it so edges stay feathered.
        "Green screen" => format!("chromakey=0x00d400:{:.3}:{:.3}", 0.03 + 0.27 * a, (0.03 + 0.27 * a) * 0.5),
        "Blue screen" => format!("chromakey=0x2b6fdf:{:.3}:{:.3}", 0.03 + 0.27 * a, (0.03 + 0.27 * a) * 0.5),
        "Drop black" => format!("format=rgba,colorkey=0x000000:{:.3}:{:.3}", 0.04 + 0.30 * a, (0.04 + 0.30 * a) * 0.6),
        "Drop white" => format!("format=rgba,colorkey=0xffffff:{:.3}:{:.3}", 0.04 + 0.30 * a, (0.04 + 0.30 * a) * 0.6),
        "Luma key" => format!("format=rgba,lumakey=threshold={:.3}:tolerance={:.3}", 0.36 * a, 0.08 + 0.12 * a),
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

/// How close a free seat must be to a named preset for the format bar to light
/// that preset up (and for Shift-drag to snap onto it).
const TITLE_SEAT_SNAP: f64 = 0.05;

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
    Effects,
    Background,
    Text,
    Audio,
    Export,
}

/// The timeline's phase-emphasis class: the CSS rules keyed on it dim the lanes
/// the active phase doesn't touch, spotlighting the ones it does. Add/Export
/// touch everything, so they emphasize nothing (empty string).
fn phase_lane_class(p: Phase) -> &'static str {
    match p {
        Phase::Cut => "mr-phase-cut",
        Phase::Style => "mr-phase-style",
        // Keying/compositing is all about the picture layers → spotlight video.
        Phase::Effects => "mr-phase-cut",
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
            // Style and Effects both edit the picture layers — browsing clips there
            // shouldn't kick you back to Cut.
            if current == Phase::Style || current == Phase::Effects {
                current
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

/// Vertical seat for a card: free `y_frac` when the user dragged it, otherwise
/// the named Top/Middle/Lower-third preset. Matches drawtext's
/// `y=(h-text_h)*y_frac` (0 = top of free space).
fn seat_y(t: &TitleItem) -> f64 {
    t.y_frac.unwrap_or_else(|| title_y(&t.pos)).clamp(0.0, 1.0)
}

/// Named preset whose seat is within snap distance of `y`, if any.
fn nearest_title_pos(y: f64) -> Option<&'static str> {
    TITLE_POS
        .iter()
        .filter(|(_, py)| (y - *py).abs() <= TITLE_SEAT_SNAP)
        .min_by(|a, b| (y - a.1).abs().partial_cmp(&(y - b.1).abs()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(n, _)| *n)
}

/// Write a free vertical seat. Snaps onto a named preset when close enough so
/// the format-bar seat buttons still light up after a careful drag.
fn set_seat_y(t: &mut TitleItem, y: f64) {
    let y = y.clamp(0.02, 0.95);
    if let Some(name) = nearest_title_pos(y) {
        t.pos = name.to_string();
        t.y_frac = None; // named seat is enough; keep projects tidy
    } else {
        t.y_frac = Some(y);
        // Leave `pos` as the last named seat for gallery/CSS fallbacks.
    }
}

/// Seat the card on a named preset (clears any free drag offset).
fn set_seat_named(t: &mut TitleItem, name: &str) {
    t.pos = name.to_string();
    t.y_frac = None;
}

/// Whether the format-bar seat button for `name` should show as active.
fn seat_matches(t: &TitleItem, name: &str) -> bool {
    (seat_y(t) - title_y(name)).abs() <= TITLE_SEAT_SNAP
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

/// Split a Whisper segment into short caption chunks of at most `max_words`
/// words each, sharing the segment's time in proportion to each chunk's word
/// count. A whole spoken sentence becomes several punchy captions instead of one
/// wall of text held for five seconds — the phone-editor default.
fn chunk_caption(start: f64, end: f64, text: &str, max_words: usize) -> Vec<(f64, f64, String)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_words {
        return vec![(start, end, text.trim().to_string())];
    }
    let span = (end - start).max(0.01);
    let n = words.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let j = (i + max_words).min(n);
        // Time boundaries fall on word fractions, so longer chunks hold longer.
        let cs = start + span * i as f64 / n as f64;
        let ce = start + span * j as f64 / n as f64;
        out.push((cs, ce, words[i..j].join(" ")));
        i = j;
    }
    out
}

#[test]
fn chunk_caption_splits_and_conserves_time() {
    let c = chunk_caption(0.0, 6.0, "one two three four five six", 2);
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].2, "one two");
    assert!((c[0].0 - 0.0).abs() < 1e-9 && (c[2].1 - 6.0).abs() < 1e-9);
    // Short segments pass through untouched.
    assert_eq!(chunk_caption(0.0, 2.0, "hi there", 5).len(), 1);
}

/// Rasterize every card a title is made of — one normally, one per word when
/// the words come in individually. Content-addressed, so re-rendering after an
/// edit only pays for the steps that actually changed.
async fn render_one(t: &TitleItem) -> Result<Vec<String>, String> {
    let mut cards = Vec::new();
    for (text, _, _, active) in t.segments() {
        let png = match active {
            // Karaoke card: full line, one word highlighted — rendered via libass.
            Some(w) => engine::render_karaoke(&t.style_of(&text), w, title_color(&t.karaoke_color)).await?,
            None => engine::render_title(&t.style_of(&text)).await?,
        };
        cards.push(png);
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
    /// Named vertical seat: "Top" | "Middle" | "Lower third". Used when
    /// `y_frac` is unset; still updated to the nearest named seat after a drag
    /// so style galleries and old UI paths keep making sense.
    pos: String,
    /// Free vertical seat as a fraction of free space (0 = top), set by
    /// dragging the title on the monitor. `None` = use the named `pos` preset.
    /// Older projects load with None and look exactly as before.
    #[serde(default)]
    y_frac: Option<f64>,
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
    /// Karaoke: keep the whole line on screen and recolour each word as it is
    /// "spoken" (its even slice of the card's duration). Its own kinetic mode,
    /// distinct from `reveal`'s one-word-at-a-time build; renders via libass.
    #[serde(default)]
    karaoke: bool,
    /// The colour the active word takes in karaoke mode — the rest of the line
    /// stays `color`.
    #[serde(default = "karaoke_hi")]
    karaoke_color: String,
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
    /// Backdrop-box opacity, 0..1. The punchy caption plate wants ~0.85; the
    /// old fixed value was 0.45, which stays the default so nothing shifts.
    #[serde(default = "box_opacity_default")]
    box_opacity: f64,
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
    /// When false: not composited (FCP-style disable for text cards).
    #[serde(default = "enabled_true")]
    enabled: bool,
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

/// The historical fixed caption-box opacity — the default so old projects and
/// untouched cards look exactly as before.
fn box_opacity_default() -> f64 {
    0.45
}

/// Default highlight colour for the active word in karaoke mode.
fn karaoke_hi() -> String {
    "Gold".to_string()
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
    // Vertical order under Mirror: place → size → spin → anchor → opacity.
    let set_scale: fn(&mut engine::Transform, f64) = |x, v| x.scale = v;
    let mut knobs: Vec<XformKnob> = vec![
        ("Position X", t.x, -1.0, 1.0, 0.005, |x, v| x.x = v),
        ("Position Y", t.y, -1.0, 1.0, 0.005, |x, v| x.y = v),
        ("Scale", t.scale, 0.1, 4.0, 0.01, set_scale),
        ("Stretch across", t.scale_x, 0.1, 4.0, 0.01, |x, v| x.scale_x = v),
        ("Stretch down", t.scale_y, 0.1, 4.0, 0.01, |x, v| x.scale_y = v),
        ("Rotation", t.rotation, -180.0, 180.0, 1.0, |x, v| x.rotation = v),
        // The pivot rotation turns around; moving it shifts the picture (Position
        // places the anchor), so a spin can swing/orbit instead of turning in place.
        ("Anchor X", t.anchor_x, -1.0, 1.0, 0.005, |x, v| x.anchor_x = v),
        ("Anchor Y", t.anchor_y, -1.0, 1.0, 0.005, |x, v| x.anchor_y = v),
    ];
    if with_opacity {
        knobs.push(("Opacity", t.opacity, 0.0, 1.0, 0.01, |x, v| x.opacity = v));
    }
    knobs
}

/// Which transform rows carry a keyframe diamond. `scale` animates through the
/// proven zoompan (Ken Burns) path; `opacity` (offered only on composited V2
/// layers) drives the alpha plane; `rotation` animates via a time-varying
/// `rotate=a='…'` angle (see `AnimatedTransform::chain`). Position stays static
/// until the geometry pipeline moves onto zoompan wholesale.
fn xf_keyable(label: &str) -> bool {
    matches!(label, "Scale" | "Opacity" | "Rotation")
}

/// The `Animated` field a transform-row label edits. Every knob maps to one, so
/// a slider drag can key an animated field in place rather than flattening it.
fn xf_field<'a>(
    at: &'a mut engine::AnimatedTransform,
    label: &str,
) -> Option<&'a mut keyframe::Animated<f64>> {
    Some(match label {
        "Scale" => &mut at.scale,
        "Stretch across" => &mut at.scale_x,
        "Stretch down" => &mut at.scale_y,
        "Position X" => &mut at.x,
        "Position Y" => &mut at.y,
        "Rotation" => &mut at.rotation,
        "Opacity" => &mut at.opacity,
        _ => return None,
    })
}

/// Write a slider value: re-key an animated field at the playhead (`t`, clip-
/// local seconds), or set a plain constant when it isn't animated. Preserving
/// the curve is the whole point — dragging one row must never wipe a sibling's
/// keyframes, which `set_pose` would.
fn xf_write(at: &mut engine::AnimatedTransform, label: &str, v: f64, t: f64) {
    if let Some(f) = xf_field(at, label) {
        if f.is_animated() {
            f.set_key(t, v, keyframe::Interp::Smooth);
        } else {
            *f = keyframe::Animated::Const(v);
        }
    }
}

/// Diamond click: drop a key at the playhead holding the value shown there, or
/// pull the key already sitting there back out.
fn xf_toggle_key(at: &mut engine::AnimatedTransform, label: &str, t: f64) {
    if let Some(f) = xf_field(at, label) {
        if f.has_key(t) {
            f.remove_key(t);
        } else {
            let v = f.sample(t);
            f.set_key(t, v, keyframe::Interp::Smooth);
        }
    }
}

/// Is this row's field currently a curve? Fills its diamond.
fn xf_field_animated(at: &engine::AnimatedTransform, label: &str) -> bool {
    match label {
        "Scale" => at.scale.is_animated(),
        "Opacity" => at.opacity.is_animated(),
        "Rotation" => at.rotation.is_animated(),
        _ => false,
    }
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
    /// The cards this title is actually made of: (text, start, length, active).
    /// One card normally; one per word for a reveal (growing prefix) or for
    /// karaoke (full line, `active` = the highlighted word index).
    fn segments(&self) -> Vec<(String, f64, f64, Option<usize>)> {
        // Karaoke: the whole line every card, one card per word, spread evenly
        // across the full duration — the highlight jumps word to word.
        if self.kind == "Text" && self.karaoke {
            let n = self.text.split_whitespace().count();
            if n >= 2 {
                let step = self.dur / n as f64;
                return (0..n)
                    .map(|k| {
                        let at = self.at + k as f64 * step;
                        let end = if k + 1 == n {
                            self.at + self.dur
                        } else {
                            self.at + (k + 1) as f64 * step
                        };
                        (self.text.clone(), at, (end - at).max(0.01), Some(k))
                    })
                    .collect();
            }
        }
        let steps = word_prefixes(&self.text);
        if !self.reveal || self.kind != "Text" || steps.len() < 2 {
            return vec![(self.text.clone(), self.at, self.dur, None)];
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
                (text, at, (end - at).max(0.01), None)
            })
            .collect()
    }

    /// Which card is on screen at `t`, if any.
    fn card_at(&self, t: f64) -> Option<usize> {
        self.segments().iter().position(|(_, at, dur, _)| t >= *at && t < at + dur)
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
            y_frac: seat_y(self),
            font: self.font.clone(),
            align: self.align.clone(),
            outline: self.outline,
            outline_color: title_color(&self.outline_color).to_string(),
            boxed: self.boxed,
            box_opacity: self.box_opacity,
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
    /// Play the trimmed span backwards. Off by default so old projects load.
    #[serde(default)]
    reverse: bool,
    /// Gain on this clip's own audio; 0.0 mutes it.
    #[serde(default = "unity")]
    volume: f64,
    /// "Reduce background noise" strength 0..=1 for this clip's own audio.
    #[serde(default)]
    denoise: f64,
    /// EQ / voice treatment (engine::AUDIO_TREATS); "None" = flat.
    #[serde(default = "none_label")]
    treat: String,
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
    /// When false: invisible and silent in preview/export, still on the
    /// timeline (FCP Clip › Disable). Default true for older projects.
    #[serde(default = "enabled_true")]
    enabled: bool,
    /// When true and any item is soloed, only soloed items contribute audio;
    /// non-soloed clips render B&W (FCP Clip › Solo).
    #[serde(default)]
    solo: bool,
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
            reverse: self.reverse,
            volume: self.volume,
            denoise: self.denoise,
            treat: self.treat.clone(),
            transition: self.transition.clone(),
            trans_dur: self.trans_dur,
            enabled: self.enabled,
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

    /// Source-file time for a point `off` seconds of *source* into the trimmed
    /// span — mirrored when reversed, so preview and export read the same frame.
    fn src_at(&self, off: f64) -> f64 {
        if self.reverse { self.out_s - off } else { self.in_s + off }
    }

    /// What preview/scrub extraction should read: the proxy once built.
    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

impl Default for Clip {
    fn default() -> Self {
        Clip {
            path: String::new(),
            name: String::new(),
            duration: 0.0,
            in_s: 0.0,
            out_s: 0.0,
            has_audio: false,
            effect: "None".into(),
            effect_amount: 1.0,
            framing: "Crop".into(),
            transform: engine::AnimatedTransform::default(),
            grade: engine::Grade::default(),
            speed: 1.0,
            reverse: false,
            volume: 1.0,
            denoise: 0.0,
            treat: "None".into(),
            transition: "None".into(),
            trans_dur: 0.5,
            thumb: String::new(),
            wave: String::new(),
            proxy: String::new(),
            group: 0,
            enabled: true,
            solo: false,
        }
    }
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
    /// Play the trimmed span backwards.
    #[serde(default)]
    reverse: bool,
    /// ffmpeg `blend=all_mode` value for compositing this layer (empty = alpha-over).
    /// Screen/Add turn a black-backed light-leak or particle plate into a glow over V1.
    #[serde(default)]
    blend: String,
    #[serde(skip)]
    proxy: String,
    /// Drag-together group id; 0 = ungrouped.
    group: usize,
    /// When false: skip compositing (invisible). Still sits on V2.
    #[serde(default = "enabled_true")]
    enabled: bool,
    /// Solo isolate — see [`Clip::solo`].
    #[serde(default)]
    solo: bool,
}

impl OverlayItem {
    /// Seconds this cutaway covers V1 for — its source span, retimed.
    fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// Source-file time for a point `off` seconds into the cutaway, mirrored
    /// when reversed. (Preview maps timeline seconds 1:1, matching the old code.)
    fn src_at(&self, off: f64) -> f64 {
        if self.reverse { self.out_s - off } else { self.in_s + off }
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
    /// When false: silent and out of the mix (still on the lane).
    #[serde(default = "enabled_true")]
    enabled: bool,
    /// Solo isolate — see [`Clip::solo`].
    #[serde(default)]
    solo: bool,
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

impl Default for AudioItem {
    fn default() -> Self {
        AudioItem {
            path: String::new(),
            name: String::new(),
            duration: 0.0,
            in_s: 0.0,
            out_s: 0.0,
            at: 0.0,
            volume: 1.0,
            vol_end: -1.0,
            duck: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            denoise: 0.0,
            noise_floor: -25.0,
            track_noise: false,
            compress: 0.0,
            gate: 0.0,
            declick: 0.0,
            treat: "None".into(),
            lane: 1,
            wave: String::new(),
            group: 0,
            enabled: true,
            solo: false,
        }
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
/// Clip/overlay/audio/title `enabled` default — older projects load as on.
fn enabled_true() -> bool {
    true
}
/// Solid black monitor frame for a disabled V1 clip under the playhead.
const BLACK_PREVIEW: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='540' height='960'%3E%3Crect fill='black' width='100%25' height='100%25'/%3E%3C/svg%3E";
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
        y_frac: None,
        // Flat by default: bevel is an opt-in effect, not the default look. An
        // outline still carries legibility on its own.
        bevel: "Off".to_string(),
        bevel_size: 21.0,
        font: "Sans".to_string(),
        align: "Centre".to_string(),
        anim: "None".to_string(),
        reveal: false,
        karaoke: false,
        karaoke_color: karaoke_hi(),
        kind: "Text".to_string(),
        shape_w: 0.6,
        shape_h: 0.12,
        shape_x: 0.0,
        // Transparent + outline: the video shows through with no opaque plate.
        boxed: false,
        box_opacity: box_opacity_default(),
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
        enabled: true,
    }
}

/// Ready-made looks offered above the user's saved presets — the starter
/// gallery OpenShot/kdenlive ship, but built from MorReel's own knobs so every
/// one renders through the real title engine. `restyle` keeps each card's own
/// words and timing; only the look is copied.
///
/// Layout and entrance names lean on the iMovie/FCP vocabulary (Slide, Lower
/// third, Expand) so a creator scanning the grid already knows what they'll get.
fn builtin_title_styles() -> Vec<TitlePreset> {
    let named = |name: &str, style: TitleItem| TitlePreset { name: name.to_string(), style };
    vec![
        // Full-frame centered title — the classic open card.
        named("Centered title", TitleItem { font_size: 140.0, outline: 8.0, pos: "Middle".into(), ..base_title() }),
        // The punchy phone caption: big, heavy outline, sat in the lower third.
        named("Bold caption", TitleItem { font_size: 150.0, outline: 12.0, pos: "Lower third".into(), ..base_title() }),
        // News lower-third: opaque plate instead of an outline.
        named("Lower-third box", TitleItem { boxed: true, outline: 0.0, font_size: 90.0, pos: "Lower third".into(), ..base_title() }),
        // The social-caption plate: white words on a punchy near-solid slab,
        // words arriving one at a time — the FCP-style caption over busy video.
        named("Caption plate", TitleItem { boxed: true, box_opacity: 0.85, outline: 0.0, font_size: 100.0, reveal: true, pos: "Lower third".into(), ..base_title() }),
        // Slide-on entrances — iMovie's "Slide" family, driven by TITLE_ANIMS.
        named("Slide up", TitleItem { anim: "Slide up".into(), font_size: 120.0, pos: "Middle".into(), outline: 6.0, ..base_title() }),
        named("Slide lower third", TitleItem {
            anim: "Slide up".into(), font_size: 100.0, pos: "Lower third".into(), outline: 6.0, ..base_title()
        }),
        named("Slide in", TitleItem { anim: "Slide in left".into(), font_size: 130.0, pos: "Middle".into(), outline: 6.0, ..base_title() }),
        // Expanding plate that rides up into the frame.
        named("Expand plate", TitleItem {
            boxed: true, box_opacity: 0.9, outline: 0.0, font_size: 110.0,
            pos: "Middle".into(), anim: "Slide up".into(), ..base_title()
        }),
        named("Top title", TitleItem { font_size: 90.0, pos: "Top".into(), outline: 5.0, ..base_title() }),
        // The embossed look — MorReel's signature bevel, in gold.
        named("Gold chisel", TitleItem { color: "Gold".into(), bevel: "Cameo".into(), outline: 0.0, font_size: 130.0, ..base_title() }),
        named("Neon pop", TitleItem { color: "Cyan".into(), outline: 8.0, font_size: 130.0, ..base_title() }),
        named("Red alert", TitleItem { color: "Red".into(), outline_color: "White".into(), outline: 6.0, font_size: 140.0, ..base_title() }),
        // Carved into the video rather than standing off it.
        named("Carved", TitleItem { bevel: "Intaglio".into(), outline: 0.0, font_size: 130.0, ..base_title() }),
        // The CapCut/TikTok karaoke: whole line up, each word lights gold in turn.
        named("Karaoke", TitleItem { karaoke: true, karaoke_color: "Gold".into(), outline: 8.0, font_size: 110.0, pos: "Lower third".into(), ..base_title() }),
        // Karaoke on a plate, for busy footage.
        named("Karaoke plate", TitleItem { karaoke: true, karaoke_color: "Cyan".into(), boxed: true, box_opacity: 0.85, outline: 0.0, font_size: 96.0, pos: "Lower third".into(), ..base_title() }),
        // The small, unobtrusive movie subtitle.
        named("Subtitle", TitleItem { font_size: 70.0, outline: 5.0, pos: "Lower third".into(), ..base_title() }),
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
        enabled: dst.enabled,
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

/// The noun for whatever's selected, shown in the inspector title so the panel
/// header names what you're editing instead of a redundant in-body label.
fn sel_noun(sel: Option<Sel>, titles: &[TitleItem]) -> Option<&'static str> {
    Some(match sel? {
        Sel::Main(_) => "Clip",
        Sel::Over(_) => "Cutaway",
        Sel::Aud(_) => "Audio",
        Sel::Title(k) => {
            let t = titles.get(k)?;
            if t.caption { "Caption" } else if t.kind != "Text" { "Shape" } else { "Text" }
        }
    })
}

/// The popped inspector window's identity: what you're editing, in that kind's
/// own colour, so the window reads as a purpose-built tool rather than a
/// stretched panel. A live selection wins; with nothing selected it names the
/// current workspace instead. Returns (accent hex, glyph, title, eyebrow).
fn solo_identity(
    sel: Option<Sel>,
    phase: Phase,
    titles: &[TitleItem],
) -> (&'static str, &'static str, String, &'static str) {
    if let Some(noun) = sel_noun(sel, titles) {
        // Each element kind carries its own accent — instant "what is this window
        // for". Text/Caption ride the app violet; the rest borrow palette hues.
        let (color, glyph) = match noun {
            "Caption" => ("#8f7bf0", "T"),
            "Text" => ("#8f7bf0", "T"),
            "Shape" => ("#a48ff5", "◆"),
            "Clip" => ("#5b8def", "▦"),
            "Cutaway" => ("#e86aa6", "⧉"),
            "Audio" => ("#3dd6c8", "♪"),
            _ => ("#8f7bf0", "◆"),
        };
        (color, glyph, noun.to_string(), "Now editing")
    } else {
        let (color, glyph, name) = match phase {
            Phase::Add => ("#8f7bf0", "＋", "Add"),
            Phase::Cut => ("#8f7bf0", "✂", "Cut"),
            Phase::Style => ("#8f7bf0", "✦", "Style"),
            Phase::Effects => ("#f0a04f", "◧", "Effects"),
            Phase::Background => ("#8f7bf0", "▧", "Background"),
            Phase::Text => ("#8f7bf0", "T", "Text"),
            Phase::Audio => ("#3dd6c8", "♪", "Audio"),
            Phase::Export => ("#8f7bf0", "⇪", "Export"),
        };
        (color, glyph, name.to_string(), "Workspace")
    }
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
            return Some((i + 1, progress, next.src_at(into)));
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
            return Some((i, c.src_at((t - acc).clamp(0.0, d) * c.speed.max(0.01))));
        }
        acc += d;
    }
    None
}

fn fmt_t(s: f64) -> String {
    let s = s.max(0.0); // squash negatives and -0.0 → "0:00.0"
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// Short clip length for the filmstrip badge — iMovie-style "4.0s" under a
/// minute, "1.5h" at an hour and up, full deck readout in between.
fn fmt_clip_dur(s: f64) -> String {
    let s = s.max(0.0);
    if s < 60.0 {
        format!("{s:.1}s")
    } else if s >= 3600.0 {
        format!("{:.1}h", s / 3600.0)
    } else {
        fmt_t(s)
    }
}

/// Resize a title from either edge. Left keeps the right edge fixed on the
/// timeline; right only changes duration. `dt` is timeline seconds (drag right
/// is positive). Minimum hold is 0.3s so a card can't vanish under the grip.
fn title_edge_resize(at0: f64, dur0: f64, left: bool, dt: f64) -> (f64, f64) {
    const MIN: f64 = 0.3;
    if left {
        let end = at0 + dur0;
        let at = (at0 + dt).clamp(0.0, (end - MIN).max(0.0));
        (at, (end - at).max(MIN))
    } else {
        (at0, (dur0 + dt).max(MIN))
    }
}

/// Resize media from either edge. `free_at` is true for free-positioned items
/// (V2 overlays): the left edge moves `at` so the right edge stays put. For V1
/// (`free_at` false) only in/out change — timeline position is owned by extents.
/// `dt` is timeline seconds; source in/out move by `dt * speed`.
fn media_edge_resize(
    at0: f64,
    in0: f64,
    out0: f64,
    src_dur: f64,
    speed: f64,
    left: bool,
    dt: f64,
    free_at: bool,
) -> (f64, f64, f64) {
    const MIN_T: f64 = 0.1;
    let speed = speed.max(0.01);
    let min_src = MIN_T * speed;
    if left {
        let mut new_in = (in0 + dt * speed).clamp(0.0, (out0 - min_src).max(0.0));
        if free_at {
            let mut actual_dt = (new_in - in0) / speed;
            // Don't let the card walk off the left of the timeline.
            if at0 + actual_dt < 0.0 {
                actual_dt = -at0;
                new_in = (in0 + actual_dt * speed).clamp(0.0, (out0 - min_src).max(0.0));
                actual_dt = (new_in - in0) / speed;
            }
            (at0 + actual_dt, new_in, out0)
        } else {
            (at0, new_in, out0)
        }
    } else {
        let new_out = (out0 + dt * speed).clamp(in0 + min_src, src_dur.max(in0 + min_src));
        (at0, in0, new_out)
    }
}

/// CSS custom props that paint a cheap on-tile mock of a title look — colour,
/// plate, outline, and vertical seat — so the style gallery reads without
/// waiting on ffmpeg rasterize for every preset.
fn title_preview_css(t: &TitleItem) -> String {
    let color = title_color(&t.color);
    let outline = title_color(&t.outline_color);
    let bg = if t.boxed {
        format!("rgba(0,0,0,{:.2})", t.box_opacity.clamp(0.35, 0.95))
    } else {
        "transparent".into()
    };
    let shadow = if t.outline > 0.0 {
        let w = (t.outline / 3.5).clamp(1.0, 4.0);
        format!("{w:.0}px {w:.0}px 0 {outline}, -{w:.0}px -{w:.0}px 0 {outline}, {w:.0}px -{w:.0}px 0 {outline}, -{w:.0}px {w:.0}px 0 {outline}")
    } else {
        "none".into()
    };
    let y = seat_y(t);
    let seat = if y <= 0.22 {
        "flex-start"
    } else if y >= 0.62 {
        "flex-end"
    } else {
        "center"
    };
    let size = (t.font_size / 9.0).clamp(9.0, 18.0);
    format!(
        "--mr-ts-color:{color};--mr-ts-bg:{bg};--mr-ts-shadow:{shadow};--mr-ts-seat:{seat};--mr-ts-size:{size:.0}px"
    )
}

/// Timeline item class string: base + selection + group-mark + disable/solo.
fn item_class(base: &str, sel: bool, mark: bool, disabled: bool, solo: bool) -> String {
    format!(
        "{base}{}{}{}{}",
        if sel { " selected" } else { "" },
        if mark { " marked" } else { "" },
        if disabled { " disabled" } else { "" },
        if solo { " solo" } else { "" },
    )
}

/// How V1 clips paint filmstrip vs waveform — FCP Clip Appearance modes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClipAppear {
    /// Large waveforms only.
    Wave,
    /// Large wave + small film.
    WaveFilm,
    /// Film and wave the same height.
    Equal,
    /// Large film + small wave (the default iMovie-style look).
    FilmWave,
    /// Filmstrips only.
    Film,
    /// Clip labels only — compact name bars.
    Labels,
}

impl ClipAppear {
    const ALL: [ClipAppear; 6] = [
        Self::Wave,
        Self::WaveFilm,
        Self::Equal,
        Self::FilmWave,
        Self::Film,
        Self::Labels,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Wave => "Waveforms only",
            Self::WaveFilm => "Large wave + small film",
            Self::Equal => "Equal film and wave",
            Self::FilmWave => "Large film + small wave",
            Self::Film => "Filmstrips only",
            Self::Labels => "Labels only",
        }
    }

    /// Icon glyph for the mode button (Unicode stand-ins for FCP's icons).
    fn glyph(self) -> &'static str {
        match self {
            Self::Wave => "▁▃▅",
            Self::WaveFilm => "▬▁▃",
            Self::Equal => "▬▃",
            Self::FilmWave => "▬▁",
            Self::Film => "▬",
            Self::Labels => "▭",
        }
    }

    /// Base film / wave heights in px before the clip-height multiplier.
    fn base_heights(self) -> (f64, f64) {
        match self {
            Self::Wave => (0.0, 72.0),
            Self::WaveFilm => (22.0, 56.0),
            Self::Equal => (48.0, 48.0),
            Self::FilmWave => (72.0, 22.0),
            Self::Film => (72.0, 0.0),
            Self::Labels => (28.0, 0.0),
        }
    }

    /// `(film_px, wave_px)` after applying the height slider (0.5..=2.0).
    fn heights(self, height: f64) -> (f64, f64) {
        let m = height.clamp(0.5, 2.0);
        let (f, w) = self.base_heights();
        (f * m, w * m)
    }

    fn show_film(self) -> bool {
        !matches!(self, Self::Wave)
    }

    fn show_wave(self) -> bool {
        !matches!(self, Self::Film | Self::Labels)
    }

    fn labels_only(self) -> bool {
        matches!(self, Self::Labels)
    }
}

/// A cut at source-time `local` is valid only if both halves keep at least
/// `min` seconds; returns the cut point when it is.
fn cut_local(in_s: f64, out_s: f64, local: f64, min: f64) -> Option<f64> {
    (local >= in_s + min && local <= out_s - min).then_some(local)
}

/// How long a freeze frame holds by default. Edge-resize stretches it after.
const FREEZE_HOLD: f64 = 2.0;

/// Instant replay: source seconds rewound from the playhead, played back at
/// half speed so the beat reads on a short-form reel.
const REPLAY_SRC: f64 = 1.5;
const REPLAY_SPEED: f64 = 0.5;

/// Two V1 clips can be joined when they are the same source file, same rate and
/// direction, and the right clip's in-point continues the left's out-point —
/// i.e. they were a split (or a freeze was removed) and the cut is still clean.
fn can_join_clips(a: &Clip, b: &Clip) -> bool {
    if a.path != b.path || a.reverse != b.reverse {
        return false;
    }
    if (a.speed - b.speed).abs() > 1e-4 {
        return false;
    }
    // Source continuity: forward clips abut out→in; reversed clips run the
    // other way (right's out meets left's in on the file timeline).
    if a.reverse {
        (b.out_s - a.in_s).abs() < 1e-3
    } else {
        (a.out_s - b.in_s).abs() < 1e-3
    }
}

/// Source in/out for an instant-replay segment ending at `local` (source time).
/// Returns `None` if less than a tenth of a second of source is available.
fn replay_span(in_s: f64, out_s: f64, local: f64, src_len: f64) -> Option<(f64, f64)> {
    let local = local.clamp(in_s, out_s);
    let start = (local - src_len).max(in_s);
    if local - start < 0.1 {
        return None;
    }
    Some((start, local))
}

/// Where a freeze frame lands relative to the V1 clip under the playhead.
#[derive(Clone, Copy, PartialEq, Debug)]
enum FreezePlace {
    /// Split the clip at source-time `local`; freeze sits between the halves.
    Split { local: f64 },
    /// Playhead is near the head — freeze goes *before* this clip.
    Before,
    /// Playhead is near the tail (or the clip is tiny) — freeze goes *after*.
    After,
}

/// Decide freeze placement from the clip's source range and the source time
/// under the playhead. `None` if the local time is outside the kept span.
fn freeze_place(in_s: f64, out_s: f64, local: f64, min: f64) -> Option<FreezePlace> {
    if local < in_s - 1e-6 || local > out_s + 1e-6 {
        return None;
    }
    let head = local - in_s >= min;
    let tail = out_s - local >= min;
    Some(match (head, tail) {
        (true, true) => FreezePlace::Split { local },
        (false, true) => FreezePlace::Before,
        _ => FreezePlace::After,
    })
}

/// In-memory clipboard for timeline items — not part of a project snapshot.
#[derive(Clone, PartialEq)]
enum ClipboardItem {
    Main(Clip),
    Over(OverlayItem),
    Aud(AudioItem),
    Title(TitleItem),
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

/// Localhost port the running editor listens on for live coordinate commands from
/// the MCP server. ponytail: fixed port; `MORREEL_LIVE_PORT` overrides on a clash.
const LIVE_PORT: u16 = 8177;

fn live_port() -> u16 {
    std::env::var("MORREEL_LIVE_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(LIVE_PORT)
}

/// One live command in flight: a plugin call and the channel to answer it on.
struct LiveCmd {
    plugin: String,
    tool: String,
    params: serde_json::Value,
    reply: tokio::sync::oneshot::Sender<Result<String, String>>,
}

/// Accept newline-delimited JSON commands on localhost and hand each to the UI
/// coroutine, writing its result back on the same connection. A request is
/// `{"plugin","tool","params"}`; a reply is `{"ok": msg}` or `{"error": msg}`.
///
/// One connection at a time — a single editor driven by a single model needs no
/// concurrency, and serial handling keeps every edit ordered on the undo stack.
/// ponytail: loopback only, no auth — any local process can drive the editor,
/// which is fine for a personal tool. Add a shared token the day it isn't.
async fn live_server(live: Coroutine<LiveCmd>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let port = live_port();
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("MorReel live control off — can't bind 127.0.0.1:{port}: {e}");
            return;
        }
    };
    eprintln!("MorReel live control listening on 127.0.0.1:{port}");
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (rd, mut wr) = stream.into_split();
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(req) => {
                    let plugin = req.get("plugin").and_then(|v| v.as_str()).unwrap_or("coords").to_string();
                    let tool = req.get("tool").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let params = req.get("params").cloned().unwrap_or(serde_json::json!({}));
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    live.send(LiveCmd { plugin, tool, params, reply: tx });
                    match rx.await {
                        Ok(Ok(msg)) => serde_json::json!({ "ok": msg }),
                        Ok(Err(e)) => serde_json::json!({ "error": e }),
                        Err(_) => serde_json::json!({ "error": "editor dropped the command" }),
                    }
                }
                Err(e) => serde_json::json!({ "error": format!("bad request: {e}") }),
            };
            let _ = wr.write_all(format!("{resp}\n").as_bytes()).await;
        }
    }
}

/// What the user asked to do to a hub plugin, carried up to the one handler that
/// mutates install state and runs the per-kind side effects.
#[derive(Clone, Copy, PartialEq)]
enum HubAction {
    Install,
    Uninstall,
    Enable,
    Disable,
}

/// One plugin's row in the Plugin Hub panel. A child component so its `id` can be
/// cloned freely for each button's handler — the parent's rsx `for` loop can't
/// hold per-row `let` bindings.
#[component]
fn HubRow(
    manifest: hub::Manifest,
    installed: bool,
    enabled: bool,
    on_action: EventHandler<(String, HubAction)>,
) -> Element {
    let id = manifest.id.clone();
    let (id_toggle, id_remove) = (id.clone(), id.clone());
    let kind_label = match manifest.kind {
        hub::Kind::Agent => "agent",
        hub::Kind::Mcp => "mcp",
        hub::Kind::Bundle => "bundle",
    };
    // Kind-specific one-liner: what "install" will actually do.
    let detail = match manifest.kind {
        hub::Kind::Agent => manifest.install.clone().unwrap_or_default(),
        hub::Kind::Mcp => manifest
            .run
            .as_ref()
            .map(|r| format!("Runs `{} {}` — added to mcp-servers.json on install.", r.command, r.args.join(" ")))
            .unwrap_or_default(),
        hub::Kind::Bundle => {
            let n = manifest.bundle.as_ref().map(|b| b.effects.len()).unwrap_or(0);
            format!("Adds {n} effect look(s) to the editor.")
        }
    };
    let repo = manifest.repository.clone().unwrap_or_default();

    rsx! {
        div { class: "mr-hub-row",
            div { class: "mr-hub-head",
                span { class: "mr-hub-name", "{manifest.display_name}" }
                span { class: "mr-hub-kind mr-hub-kind-{kind_label}", "{kind_label}" }
                span { class: "mor-statusbar-muted", "by {manifest.author} · {manifest.license}" }
            }
            p { class: "mor-statusbar-muted mr-hub-desc", "{manifest.description}" }
            if !detail.is_empty() {
                p { class: "mor-statusbar-muted mr-hub-detail", "{detail}" }
            }
            if !repo.is_empty() {
                p { class: "mor-statusbar-muted mr-hub-repo", "{repo}" }
            }
            div { class: "mr-hub-actions",
                if !installed {
                    button {
                        class: "mor-btn primary",
                        onclick: move |_| on_action.call((id.clone(), HubAction::Install)),
                        "Install"
                    }
                } else {
                    button {
                        class: if enabled { "mor-btn primary" } else { "mor-btn" },
                        onclick: move |_| on_action.call((
                            id_toggle.clone(),
                            if enabled { HubAction::Disable } else { HubAction::Enable },
                        )),
                        if enabled { "Enabled" } else { "Disabled" }
                    }
                    button {
                        class: "mor-btn",
                        onclick: move |_| on_action.call((id_remove.clone(), HubAction::Uninstall)),
                        "Remove"
                    }
                }
            }
        }
    }
}

/// Which face of the editor a given window shows. The main window is `Full`
/// (everything); a popped-out inspector renders `Inspector` only. Same `Editor`
/// component, same shared state — so an edit in the popped window and the main
/// window are one edit, not two copies kept in sync.
#[derive(Clone, Copy, PartialEq)]
enum EditorView {
    Full,
    Inspector,
}

/// Every signal the editor owns, in one Copy bundle. It exists so a second
/// window (the popped-out inspector) can run the *same* `Editor` component over
/// the *same* signals: pass the bundle to a new VirtualDom and both windows read
/// and write one shared model. (Signals cross the VirtualDom boundary fine — the
/// pop-out monitor already shares one this way.) The macro keeps the field list
/// and its initializers in a single place instead of three.
macro_rules! editor_state {
    ( $( $name:ident : $ty:ty = $init:expr ),* $(,)? ) => {
        #[derive(Clone, Copy, PartialEq)]
        struct EditorState { $( $name: Signal<$ty>, )* }
        impl EditorState {
            /// Create every signal. Runs once, in the main window's `App`.
            fn new() -> Self {
                Self { $( $name: use_signal($init), )* }
            }
        }
    };
}

editor_state! {
    clips: Vec<Clip> = Vec::<Clip>::new,
    overlays: Vec<OverlayItem> = Vec::<OverlayItem>::new,
    audios: Vec<AudioItem> = Vec::<AudioItem>::new,
    titles: Vec<TitleItem> = Vec::<TitleItem>::new,
    selected: Option<Sel> = || None,
    playhead: f64 = || 0.0f64,
    show_overlays: bool = || true,
    show_titles: bool = || true,
    preview: String = String::new,
    status: String = || "Ready — add clips to start.".to_string(),
    export_progress: Option<f64> = || None,
    importing: bool = || false,
    ctx_menu: Option<(f64, f64, Ctx)> = || None,
    pending: Option<(String, f64, String, String, engine::Over)> = || None,
    preview_busy: bool = || false,
    proxy_queue: Vec<String> = Vec::<String>::new,
    proxy_busy: bool = || false,
    active_phase: Phase = || Phase::Cut,
    title_render_gen: u64 = || 0u64,
    magnet: bool = || true,
    marked: Vec<Sel> = Vec::<Sel>::new,
    next_group: usize = || 1usize,
    undo_stack: Vec<Snapshot> = Vec::<Snapshot>::new,
    redo_stack: Vec<Snapshot> = Vec::<Snapshot>::new,
    undo_tag: String = String::new,
    markers: Vec<f64> = Vec::<f64>::new,
    mixer: Mixer = Mixer::default,
    saved_json: Option<String> = || None,
    hub_gen: u64 = || 0u64,
    show_hub: bool = || false,
    show_autocut: bool = || false,
    autocut_busy: bool = || false,
    autocut_noise: f64 = || 32.0_f64,
    autocut_min_sil: f64 = || 0.35_f64,
    autocut_pad: f64 = || 0.08_f64,
    autocut_min_keep: f64 = || 0.15_f64,
    autocut_sel_only: bool = || true,
    playing: bool = || false,
    // Bumps every time a play session starts so a stale async loop exits instead
    // of racing a new one (loop / play-from-start / Space while already playing).
    play_gen: u64 = || 0u64,
    loop_playback: bool = || false,
    settings: ProjectSettings = ProjectSettings::default,
    export_opts: engine::ExportOpts = engine::ExportOpts::default,
    safe_area: bool = || false,
    transcribing: bool = || false,
    show_export: bool = || false,
    show_handles: bool = || true,
    xf_drag: Option<(XfGrab, (f64, f64), engine::Transform, (f64, f64, f64, f64))> = || None,
    // Title seat drag: (title index, start y_frac, from client-y, phone rect y/h).
    title_drag: Option<(usize, f64, f64, f64, f64)> = || None,
    presets: Vec<TitlePreset> = load_presets,
    show_save_preset: bool = || false,
    preset_name: String = String::new,
    preferred_mode: UiMode = || UiMode::load_preference().unwrap_or(UiMode::active()),
    show_about: bool = || false,
    show_shortcuts: bool = || false,
    show_settings: bool = || false,
    settings_tab: String = || "Format".to_string(),
    key_scheme: KeyScheme = load_keyscheme,
    show_keys: bool = || false,
    style_tab: String = || "Look".to_string(),
    monitor_out: bool = || false,
    zoom: f64 = || 1.0f64,
    clip_appear: ClipAppear = || ClipAppear::FilmWave,
    clip_height: f64 = || 1.0f64,
    show_clip_names: bool = || true,
    show_appear: bool = || false,
    pan: Option<(f64, f64)> = || None,
    drop_hover: Option<Lane> = || None,
    drag: Option<(Sel, f64, f64, f64)> = || None,
    drag_moved: bool = || false,
    fade_drag: Option<(usize, bool, f64, f64)> = || None,
    // Volume drag: Sel::Main or Sel::Aud, grab y, gain at grab. Vertical drag
    // on a clip's own wave strip or an A-lane item (iMovie "Adjust volume").
    vol_drag: Option<(Sel, f64, f64)> = || None,
    // Edge-resize: (target, is_left, grab_x, at0, in0_or_dur0, out0, speed, src_dur).
    // Titles use in0_or_dur0 as duration and ignore out0/speed/src_dur. Main/Over
    // store real in/out so a drag recomputes from the grab snapshot without drift.
    len_drag: Option<(Sel, bool, f64, f64, f64, f64, f64, f64)> = || None,
    scrubbing: bool = || false,
    show_effects: bool = || false,
    show_add: bool = || false,
    // Copy/paste buffer for timeline items (clips, cutaways, audio, titles).
    clipboard: Option<ClipboardItem> = || None,
    // Voiceover capture: Some((wav path, timeline start)) while the mic is live.
    // `vo_stop` is flipped true by Stop / V / Escape; the capture task watches it.
    vo_session: Option<(String, f64)> = || None,
    vo_stop: bool = || false,
    // insp_open/insp_float/float_* are per-window inspector chrome, NOT shared —
    // localized in Editor so a popped window always shows the inspector even when
    // the main window has it docked or hidden.
    layouts: Vec<Layout> = load_layouts,
    show_save_layout: bool = || false,
    layout_name: String = String::new,
    is_fullscreen: bool = || false,
    fx_thumbs: std::collections::HashMap<String, String> = std::collections::HashMap::<String, String>::new,
    fx_key: String = String::new,
}

#[component]
fn App() -> Element {
    let state = EditorState::new();
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        MorShortcutRoot { Editor { state, view: EditorView::Full } }
    }
}

/// The popped-out inspector's own window: the same `Editor` in `Inspector` view
/// over the shared `state`. Dropping this VirtualDom (closing the window) flips
/// `out` back so the main window's pop-out button re-enables.
#[component]
fn PoppedInspector(state: EditorState, out: Signal<bool>) -> Element {
    use_drop(move || out.set(false));
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        MorShortcutRoot { Editor { state, view: EditorView::Inspector } }
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
fn Editor(state: EditorState, view: EditorView) -> Element {
    // The main window owns the side effects (live-control server, autosave,
    // keyboard shortcuts, preview pump). A popped-out inspector shares the same
    // signals but must not run a second copy of any of them — it only renders.
    let is_main = view == EditorView::Full;
    let mut clips = state.clips;
    let mut overlays = state.overlays;
    let mut audios = state.audios;
    let mut titles = state.titles;
    let mut selected = state.selected;
    let mut playhead = state.playhead; // global timeline seconds
    // Declutter the monitor while editing: hide the overlay (V2) or title (T)
    // lane from the preview. A view toggle only — export always renders every
    // lane, so a hidden layer can never silently drop out of the finished reel.
    let mut show_overlays = state.show_overlays;
    let mut show_titles = state.show_titles;
    let mut preview = state.preview;
    let mut status = state.status;
    let mut export_progress = state.export_progress;
    let mut importing = state.importing;

    let total_of = move || extents(&clips.read()).iter().sum::<f64>();

    // Right-click menu: (viewport x, y, what was clicked). One menu, many targets.
    let mut ctx_menu = state.ctx_menu;
    let mut open_ctx = move |evt: Event<MouseData>, target: Ctx| {
        evt.prevent_default(); // replaces the webview's Reload/Inspect menu
        evt.stop_propagation();
        let p = evt.client_coordinates();
        ctx_menu.set(Some((p.x, p.y, target)));
    };

    // Preview extraction: latest-wins queue so slider drags don't stack ffmpeg runs.
    // Whatever is composited on top — a title's fade, the incoming half of a
    // transition — rides along so the scrub shows what the export will.
    let mut pending = state.pending;
    let mut preview_busy = state.preview_busy;
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
    let mut proxy_queue = state.proxy_queue;
    let mut proxy_busy = state.proxy_busy;
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

    // Declared here (not with the other phase state below) so `seek_to` can
    // read it: scrub-follows-clip is a picture-phase convenience and must not
    // fire in the Text/Audio workspaces, where a preview refresh would otherwise
    // steal the selection and kick you out of what you're editing.
    let mut active_phase = state.active_phase;

    // Seek: playhead moves, selection follows the V1 clip underneath (only in the
    // picture phases), preview shows whatever is on top (a V2 overlay covers V1
    // while it runs).
    let mut seek_to = move |t: f64| {
        playhead.set(t);
        let any_solo = clips.read().iter().any(|c| c.enabled && c.solo)
            || overlays.read().iter().any(|o| o.enabled && o.solo)
            || audios.read().iter().any(|a| a.enabled && a.solo);
        // Topmost title active at t, composited onto the preview frame.
        let title_png = show_titles()
            .then(|| {
                titles
                    .read()
                    .iter()
                    .rev()
                    .find(|ti| ti.enabled && t >= ti.at && t < ti.at + ti.dur && !ti.pngs.is_empty())
                    .and_then(|ti| {
                        // A revealed title is a run of cards; show whichever is up
                        // now, faded against the whole title rather than its step.
                        let k = ti.card_at(t).unwrap_or(0).min(ti.pngs.len().saturating_sub(1));
                        ti.pngs.get(k).map(|p| (p.clone(), title_alpha(t, ti.at, ti.dur)))
                    })
            })
            .flatten();
        let over = show_overlays()
            .then(|| {
                overlays.read().iter().find(|o| {
                    o.enabled && t >= o.at && t < o.at + o.trimmed()
                }).map(|o| {
                    let mut look = o.look();
                    if any_solo && !o.solo {
                        look = join_chain(look, "hue=s=0".into());
                    }
                    (o.scrub_path(), o.src_at(t - o.at), o.framing.clone(), look)
                })
            })
            .flatten();
        let loc = locate(&clips.read(), t);
        if let Some((i, _)) = loc {
            // Only follow the clip under the playhead in the picture-editing
            // phases. In Text/Audio/Export the inspector is task-organized, and a
            // re-render's preview refresh calling seek_to must not yank the
            // selection over to a clip (which would jump the workspace away).
            let picture_phase = matches!(
                active_phase(),
                Phase::Add | Phase::Cut | Phase::Style | Phase::Background
            );
            if picture_phase && selected() != Some(Sel::Main(i)) {
                selected.set(Some(Sel::Main(i)));
            }
        }
        let mut layers = engine::Over { title: title_png, ..Default::default() };
        if let Some((path, local, fr, eff)) = over {
            request_preview(path, local, fr, eff, layers);
        } else if let Some((i, local)) = loc {
            let (path, fr, eff) = {
                let cl = clips.read();
                let c = &cl[i];
                if !c.enabled {
                    // Disabled: black monitor, matching export's color=black.
                    preview.set(BLACK_PREVIEW.to_string());
                    return;
                }
                let mut look = c.look();
                if any_solo && !c.solo {
                    look = join_chain(look, "hue=s=0".into());
                }
                (c.scrub_path(), c.framing.clone(), look)
            };
            // Inside a transition the export is showing both clips at once, so
            // the monitor has to as well: the outgoing clip is the base and the
            // incoming one fades up over it by however far the blend has run.
            // Without this, scrubbing a dissolve would show a hard cut.
            if let Some((next, alpha, ntime)) = transition_at(&clips.read(), t) {
                let cl = clips.read();
                if cl[next].enabled {
                    let mut nlook = cl[next].look();
                    if any_solo && !cl[next].solo {
                        nlook = join_chain(nlook, "hue=s=0".into());
                    }
                    layers.blend = Some((
                        cl[next].scrub_path(),
                        ntime,
                        cl[next].framing.clone(),
                        nlook,
                        alpha,
                    ));
                }
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
    let mut title_render_gen = state.title_render_gen;
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
        // An explicit "select this clip" (keyboard nav, open, delete) — set it
        // outright rather than leaning on seek_to's follow, which now only fires
        // in the picture phases.
        selected.set(Some(Sel::Main(i)));
        seek_to(start_of(i));
    };

    // Magnetic timeline: V2/A1/T items anchor to the V1 clip under their start
    // point, so trims, moves and ripple deletes carry them along. ~ toggles it
    // off to edit V1 while attached items hold position (this timeline has no
    // dragging, so "hold ~ while dragging" becomes a toggle).
    // ponytail: anchors are positional, not content ids — an item re-anchors if
    // an edit puts a different clip under it.
    let mut magnet = state.magnet;
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
    let mut marked = state.marked;
    let mut next_group = state.next_group;
    // Undo/redo. `push_undo` records the state *before* an edit; a non-empty
    // tag collapses a run of edits that share it, so one slider drag is one
    // undo step instead of forty. Discrete actions pass "" and never collapse.
    let mut undo_stack = state.undo_stack;
    let mut redo_stack = state.redo_stack;
    let mut undo_tag = state.undo_tag;
    // Beat markers: tap M along to the music while it plays and you get the
    // grid you actually want to cut on. They are snap targets, so a dragged
    // item lands on the beat instead of near it.
    let mut markers = state.markers;
    let mut mixer = state.mixer;
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
    let mut saved_json = state.saved_json;
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

    // Live control: an external process (the MCP server) drives the timeline in
    // real time. Each command runs through the very same undo → mutate → restore
    // path a UI edit uses, so it lands on the undo stack and refreshes the monitor
    // (restore reseeks, which re-renders the preview). The whole thing runs on
    // Dioxus's single-threaded executor, so the !Send coroutine handle and signals
    // are used directly — no cross-thread plumbing.
    let live = use_coroutine(move |mut rx: UnboundedReceiver<LiveCmd>| async move {
        if !is_main {
            return; // a popped inspector shares state; it doesn't serve live control
        }
        while let Some(cmd) = rx.next().await {
            // Read-only discovery: what plugins/tools exist, for `morreel tools`.
            if cmd.tool == "list_tools" {
                let m = serde_json::to_string_pretty(&plugin::manifest()).unwrap_or_default();
                let _ = cmd.reply.send(Ok(m));
                continue;
            }
            // Read-only listing so the model can see what indices exist, without
            // going through the mutating registry.
            if cmd.tool == "list_items" {
                let snap = snapshot();
                let lane = |label: &str, names: Vec<String>| {
                    let body = names.iter().enumerate().map(|(i, n)| format!("  [{i}] {n}")).collect::<Vec<_>>();
                    format!("{label} ({}):\n{}", names.len(), body.join("\n"))
                };
                let v1 = lane("V1 clips", snap.clips.iter().map(|c| c.name.clone()).collect());
                let v2 = lane("V2 overlays", snap.overlays.iter().map(|o| o.name.clone()).collect());
                let _ = cmd.reply.send(Ok(format!("{v1}\n{v2}")));
                continue;
            }
            // Render a clip's source frame to a PNG and hand back its path, so a
            // vision model can *see* the shot and then reframe it with the coords
            // tools (place_box / place_point / track_point) — the MorReel form of
            // Smart Conform. The whole uncropped source is returned on purpose:
            // the model needs to see what the current crop leaves out.
            if cmd.tool == "get_frame" {
                let tgt = cmd.params.get("target").unwrap_or(&cmd.params);
                let lane = tgt.get("lane").and_then(|v| v.as_str()).unwrap_or("V1");
                let index = tgt.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let snap = snapshot();
                let src = match lane {
                    "V2" => snap.overlays.get(index).map(|o| (o.path.clone(), o.in_s, o.out_s)),
                    _ => snap.clips.get(index).map(|c| (c.path.clone(), c.in_s, c.out_s)),
                };
                let reply = match src {
                    None => Err(format!("no item at {lane}[{index}]")),
                    Some((path, in_s, out_s)) => {
                        // `at` = seconds into the source from the clip's in-point;
                        // default to the midpoint, a representative frame.
                        let t = match cmd.params.get("at").and_then(|v| v.as_f64()) {
                            Some(a) => (in_s + a).clamp(in_s, out_s.max(in_s)),
                            None => (in_s + out_s) / 2.0,
                        };
                        engine::extract_still(&path, t).await.map_err(|e| format!("get_frame: {e}"))
                    }
                };
                let _ = cmd.reply.send(reply);
                continue;
            }
            let mut snap = snapshot();
            let result = plugin::dispatch(&mut snap, &cmd.plugin, &cmd.tool, &cmd.params);
            if let Ok(ref msg) = result {
                push_undo("");
                restore(snap);
                status.set(format!("Live · {msg}"));
            }
            let _ = cmd.reply.send(result);
        }
    });
    use_future(move || async move {
        if is_main {
            live_server(live).await;
        }
    });

    // Plugin Hub state. Load once: bundle effects from installed+enabled plugins go
    // into the effect list before the first render builds the picker. `hub_gen`
    // bumps to re-render when the Plugin Hub panel installs/toggles something.
    let mut hub_gen = state.hub_gen;
    let mut show_hub = state.show_hub;
    use_hook(|| {
        if let Some(dir) = hub::hub_dir() {
            let manifests = hub::load_manifests(&dir);
            set_hub_effects(hub::active_bundle_effects(&manifests, &hub::InstallState::load()));
        }
    });
    // One handler for every hub row action: mutate install state, then run the
    // per-kind side effects — an mcp plugin re-syncs mcp-servers.json, a bundle
    // reloads the effect list. `hub_gen` bumps so the panel and effect picker
    // re-render. Manifests are re-read here so a `git pull` is picked up live.
    let mut handle_hub = move |(id, action): (String, HubAction)| {
        let Some(dir) = hub::hub_dir() else { return };
        let manifests = hub::load_manifests(&dir);
        let mut state = hub::InstallState::load();
        let res = match action {
            HubAction::Install => state.set_installed(&id, true),
            HubAction::Uninstall => state.set_installed(&id, false),
            HubAction::Enable => state.set_enabled(&id, true),
            HubAction::Disable => state.set_enabled(&id, false),
        };
        if let Err(e) = res {
            status.set(format!("Plugin Hub: {e}"));
            return;
        }
        set_hub_effects(hub::active_bundle_effects(&manifests, &state));
        let word = match action {
            HubAction::Install => "installed",
            HubAction::Uninstall => "removed",
            HubAction::Enable => "enabled",
            HubAction::Disable => "disabled",
        };
        match hub::sync_mcp_servers(&manifests, &state) {
            Ok(p) => status.set(format!("Plugin '{id}' {word}. MCP servers → {}", p.display())),
            Err(e) => status.set(format!("Plugin '{id}' {word} (mcp sync failed: {e})")),
        }
        hub_gen += 1;
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
    let mut show_autocut = state.show_autocut;
    let mut autocut_busy = state.autocut_busy;
    // Silence threshold as positive dB magnitude (UI shows −N dB).
    let mut autocut_noise = state.autocut_noise;
    let mut autocut_min_sil = state.autocut_min_sil;
    let mut autocut_pad = state.autocut_pad;
    let mut autocut_min_keep = state.autocut_min_keep;
    // true = only the selected V1 clip; false = every V1 clip with audio.
    let mut autocut_sel_only = state.autocut_sel_only;

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

    // Ripple-trim the selected V1 clip's edge by whole frames — the keyboard
    // precision editor. `edge_out` picks the clip's end (out) vs start (in);
    // `frames` is signed. The rest of the timeline ripples for free because V1
    // position is derived from clip order, not stored.
    let mut ripple_trim = move |edge_out: bool, frames: f64| {
        let Some(Sel::Main(i)) = selected() else {
            status.set("Select a clip first to ripple-trim.".into());
            return;
        };
        if clips.read().get(i).is_none() {
            return;
        }
        let dt = frames / engine::FPS as f64;
        push_undo("ripple-trim");
        let old = spans();
        {
            let mut cl = clips.write();
            let c = &mut cl[i];
            if edge_out {
                c.out_s = (c.out_s + dt).clamp(c.in_s + 0.1, c.duration);
            } else {
                c.in_s = (c.in_s + dt).clamp(0.0, c.out_s - 0.1);
            }
        }
        ride(old, &|k| Some(start_of(k)));
        let (a, b) = { let c = &clips.read()[i]; (c.in_s, c.out_s) };
        status.set(format!(
            "Trim {} {:+}f  →  {a:.2}..{b:.2}s",
            if edge_out { "out" } else { "in" },
            frames as i64
        ));
    };

    // Render one waveform strip in the background and fan it out to every item
    // sharing that source path — one render per source, splits inherit by clone.
    let fill_clip_waves = move |path: String| {
        spawn(async move {
            if let Ok(uri) = engine::waveform_data_uri(&path).await {
                for c in clips.write().iter_mut().filter(|c| c.path == path) {
                    c.wave = uri.clone();
                }
            }
        });
    };
    let fill_audio_waves = move |path: String| {
        spawn(async move {
            if let Ok(uri) = engine::waveform_data_uri(&path).await {
                for a in audios.write().iter_mut().filter(|a| a.path == path) {
                    a.wave = uri.clone();
                }
            }
        });
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
            out_s: initial_out(&path, duration),
            has_audio,
            thumb,
            ..Default::default()
        };
        {
            let mut cl = clips.write();
            let i = insert_at.unwrap_or(cl.len()).min(cl.len());
            cl.insert(i, clip);
        }
        queue_proxy(path.clone());
        // A clip's own audio gets the same waveform strip A1 items have, so you
        // can see where the sound is without scrubbing for it.
        if has_audio {
            fill_clip_waves(path);
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
                        reverse: false,
                        blend: String::new(),
                        proxy: String::new(),
                        group: 0,
                        enabled: true,
                        solo: false,
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
                        out_s: duration,
                        at: at.max(0.0),
                        lane: bus,
                        ..Default::default()
                    });
                    selected.set(Some(Sel::Aud(audios.read().len() - 1)));
                    let tag = if bus >= 2 { "A2" } else { "A1" };
                    status.set(format!(
                        "Audio on {tag} at {} — mixed under the main track.",
                        fmt_t(at)
                    ));
                    // Waveform renders in the background; splits share it by path.
                    fill_audio_waves(path);
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

    // Apply a title look: restyle the selected card, or drop a new one at the
    // playhead when nothing is selected — the iMovie "pick a style from the
    // browser" path, not a dropdown of names.
    let mut apply_title_style = move |preset: TitlePreset| {
        if clips.read().is_empty() {
            status.set("Add a clip before adding text.".to_string());
            return;
        }
        push_undo("");
        let name = preset.name.clone();
        if let Some(Sel::Title(k)) = selected() {
            if let Some(item) = titles.write().get_mut(k) {
                *item = restyle(item, &preset.style);
            }
            rerender_title(k);
            status.set(format!("Applied \"{name}\"."));
            return;
        }
        let mut t = preset.style;
        t.at = playhead();
        t.text = "Text".into();
        t.pngs.clear();
        titles.write().push(t);
        let k = titles.read().len() - 1;
        selected.set(Some(Sel::Title(k)));
        active_phase.set(Phase::Text);
        rerender_title(k);
        status.set(format!("Added \"{name}\" at the playhead — type over it."));
    };

    let gather_specs = move || -> (Vec<ClipSpec>, Vec<OverlaySpec>, Vec<TitleSpec>, Vec<AudioSpec>) {
        // The mixer folds in once, here: V1 gain scales every clip's own audio;
        // A1/A2 gain scales (or, when muted/soloed-out, drops) each bed. Both
        // preview and export come through this function, so they stay in step.
        // Per-item Disable and Solo (FCP-style) also land here so preview = export.
        let m = mixer();
        let gv1 = m.gain_of(MIX_V1);
        let cl = clips.read();
        let ov = overlays.read();
        let au = audios.read();
        let any_solo = cl.iter().any(|c| c.enabled && c.solo)
            || ov.iter().any(|o| o.enabled && o.solo)
            || au.iter().any(|a| a.enabled && a.solo);
        let specs = cl
            .iter()
            .map(|c| {
                let mut s = c.spec();
                s.volume *= gv1;
                // Solo isolates audio: non-soloed clips go silent. Picture stays
                // (desaturated) so you can still see the rest of the cut.
                if c.enabled && any_solo && !c.solo {
                    s.volume = 0.0;
                    s.effect = join_chain(s.effect, "hue=s=0".into());
                }
                s
            })
            .collect();
        let ospecs = ov
            .iter()
            .filter(|o| o.enabled)
            .map(|o| {
                let mut effect = o.look();
                if any_solo && !o.solo {
                    effect = join_chain(effect, "hue=s=0".into());
                }
                OverlaySpec {
                    path: o.path.clone(),
                    in_s: o.in_s,
                    out_s: o.out_s,
                    at: o.at,
                    speed: o.speed,
                    reverse: o.reverse,
                    effect,
                    framing: o.framing.clone(),
                    blend: o.blend.clone(),
                }
            })
            .collect();
        let tspecs = titles
            .read()
            .iter()
            .filter(|t| t.enabled && !t.pngs.is_empty())
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
        let aspecs = au
            .iter()
            .filter_map(|a| {
                if !a.enabled || (any_solo && !a.solo) {
                    return None;
                }
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
    // `play_gen` invalidates a loop that lost a race with Space / loop / play-from-start.
    let mut playing = state.playing;
    let mut play_gen = state.play_gen;
    let mut loop_playback = state.loop_playback;
    let mut start_play = move || {
        if clips.read().is_empty() {
            return;
        }
        let g = play_gen() + 1;
        play_gen.set(g);
        playing.set(true);
        spawn(async move {
            let wav = std::env::temp_dir().join("morreel-playmix.wav");
            let (specs, _, _, aspecs) = gather_specs();
            let mut audio = match engine::render_audio_mix(&specs, &aspecs, &wav).await {
                // Guard: paused or superseded while the mix was rendering.
                Ok(()) if playing() && play_gen() == g => match engine::launch_audio(&wav, playhead()) {
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
            while playing() && play_gen() == g {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !playing() || play_gen() != g {
                    break;
                }
                let dt = last.elapsed().as_secs_f64().min(0.5);
                last = std::time::Instant::now();
                let t = playhead() + dt;
                if t >= total_of() {
                    if loop_playback() {
                        seek_to(0.0);
                        // Restart the mix from the top so sound loops with picture.
                        if let Some(child) = audio.as_mut() {
                            let _ = child.start_kill();
                        }
                        audio = match engine::launch_audio(&wav, 0.0) {
                            Ok(child) => Some(child),
                            Err(_) => None,
                        };
                        last = std::time::Instant::now();
                        continue;
                    }
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
        start_play();
    };
    let mut play_from_start = move |_: ()| {
        if clips.read().is_empty() {
            return;
        }
        playing.set(false);
        seek_to(0.0);
        start_play();
    };
    let mut toggle_loop = move |_: ()| {
        loop_playback.toggle();
        status.set(if loop_playback() {
            "Loop playback on — the reel restarts at the end.".to_string()
        } else {
            "Loop playback off.".to_string()
        });
    };

    // Record voiceover onto A2 at the playhead (iMovie "Record Voiceover").
    // First press starts the mic; second press (or V) stops and lands the take.
    // Capture runs in a background task so the UI stays responsive.
    let mut vo_session = state.vo_session;
    let mut vo_stop = state.vo_stop;
    let mut stop_voiceover = move |_: ()| {
        if vo_session().is_none() {
            return;
        }
        vo_stop.set(true);
    };
    let mut start_voiceover = move |_: ()| {
        if vo_session().is_some() {
            vo_stop.set(true);
            return;
        }
        if export_progress().is_some() {
            status.set("Can't record during export.".to_string());
            return;
        }
        let path = engine::voiceover_out_path();
        let path_s = path.display().to_string();
        let at = playhead().max(0.0);
        vo_stop.set(false);
        vo_session.set(Some((path_s.clone(), at)));
        status.set(format!(
            "● Recording voiceover from {} — press V or Stop when done.",
            fmt_t(at)
        ));
        // Roll picture with the take when there's a reel to watch.
        if !clips.read().is_empty() && !playing() {
            start_play();
        }
        spawn(async move {
            let mut child = match engine::start_mic_record(&path).await {
                Ok(c) => c,
                Err(e) => {
                    vo_session.set(None);
                    vo_stop.set(false);
                    status.set(e);
                    return;
                }
            };
            loop {
                if vo_stop() {
                    break;
                }
                match child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(_)) => {
                        vo_session.set(None);
                        vo_stop.set(false);
                        let _ = std::fs::remove_file(&path);
                        status.set("Microphone capture ended unexpectedly.".to_string());
                        return;
                    }
                    Err(e) => {
                        vo_session.set(None);
                        vo_stop.set(false);
                        status.set(format!("Mic capture error: {e}"));
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
            vo_stop.set(false);
            if let Err(e) = engine::stop_mic_record(child).await {
                vo_session.set(None);
                let _ = std::fs::remove_file(&path);
                status.set(format!("Could not finish recording: {e}"));
                return;
            }
            vo_session.set(None);
            playing.set(false);
            match engine::probe(&path_s).await {
                Ok((duration, has_audio)) if has_audio && duration >= 0.2 => {
                    push_undo("");
                    let n = audios
                        .read()
                        .iter()
                        .filter(|a| a.name.starts_with("Voiceover"))
                        .count()
                        + 1;
                    audios.write().push(AudioItem {
                        path: path_s.clone(),
                        name: format!("Voiceover {n}"),
                        duration,
                        out_s: duration,
                        at,
                        fade_in: 0.05,
                        fade_out: 0.1,
                        lane: 2, // A2 — VO / second bed
                        ..Default::default()
                    });
                    let k = audios.read().len() - 1;
                    selected.set(Some(Sel::Aud(k)));
                    active_phase.set(Phase::Audio);
                    status.set(format!(
                        "Voiceover on A2 at {} ({}) — duck music under it if needed.",
                        fmt_t(at),
                        fmt_clip_dur(duration)
                    ));
                    fill_audio_waves(path_s);
                }
                Ok((duration, _)) => {
                    let _ = std::fs::remove_file(&path);
                    status.set(if duration < 0.2 {
                        "Recording too short — hold a little longer.".to_string()
                    } else {
                        "Recording had no audio — check the microphone.".to_string()
                    });
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    status.set(format!("Could not read voiceover: {e}"));
                }
            }
        });
    };
    let mut toggle_voiceover = move |_: ()| {
        if vo_session().is_some() {
            stop_voiceover(());
        } else {
            start_voiceover(());
        }
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
            fill_audio_waves(path);
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
            fill_clip_waves(path);
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
    let mut settings = state.settings;
    // Declared up here (not by their dialogs below) so open_project can seed
    // them from a loaded project's settings.
    let mut export_opts = state.export_opts;
    let mut safe_area = state.safe_area;

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
    let mut transcribing = state.transcribing;
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
                    // Break long segments into short, fast caption chunks (≤5
                    // words) before laying them down — punchy phone captions
                    // rather than one held sentence.
                    let segs: Vec<(f64, f64, String)> = segs
                        .iter()
                        .flat_map(|(s, e, t)| chunk_caption(*s, *e, t, 5))
                        .collect();
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
                                pos: "Lower third".to_string(),
                                boxed: true, // backdrop keeps captions readable over busy video
                                box_opacity: 0.85, // a punchy plate, not the faint old default
                                outline: 0.0,
                                caption: true,
                                ..base_title()
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
    let mut show_export = state.show_export;

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
            wave: wave.clone(),
            group: gid,
            ..Default::default()
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

    // Freeze frame at the playhead: grab the source frame, hold it for a couple
    // of seconds, and (when there's room) split the clip around it — Final Cut /
    // iMovie "Add Freeze Frame". Edge-resize stretches the hold afterwards.
    let mut add_freeze_frame = move |_: ()| {
        const MIN: f64 = 0.1;
        let t = playhead();
        let Some((i, local)) = locate(&clips.read(), t) else {
            status.set("Park the playhead on a V1 clip to freeze a frame.".to_string());
            return;
        };
        let c = clips.read()[i].clone();
        let place = match freeze_place(c.in_s, c.out_s, local, MIN) {
            Some(p) => p,
            None => {
                status.set("Playhead is outside the clip's kept range.".to_string());
                return;
            }
        };
        let path = c.path.clone();
        let name = c.name.clone();
        let framing = c.framing.clone();
        let effect = c.effect.clone();
        let effect_amount = c.effect_amount;
        let transform = c.transform.clone();
        let grade = c.grade.clone();
        status.set(format!("Capturing freeze frame from {}…", name));
        spawn(async move {
            let png = match engine::extract_still(&path, local).await {
                Ok(p) => p,
                Err(e) => {
                    status.set(format!("Freeze frame failed: {e}"));
                    return;
                }
            };
            let thumb = engine::frame_data_uri(&png, 0.0, 108, 192, &framing, "", engine::Over::default())
                .await
                .unwrap_or_default();
            // Bail if the reel changed under us (clip removed / reordered).
            let still_here = clips.read().get(i).is_some_and(|cur| cur.path == path);
            if !still_here {
                status.set("Clip moved while capturing — try freeze frame again.".to_string());
                return;
            }
            // Timeline second where the freeze begins — free-position items at
            // or after this point ride forward by the hold (clip-level magnet
            // can't express a mid-clip insert cleanly).
            let freeze_at = match place {
                FreezePlace::Before => start_of(i),
                FreezePlace::Split { .. } => t,
                FreezePlace::After => start_of(i) + extents(&clips.read()).get(i).copied().unwrap_or(0.0),
            };
            push_undo("");
            let freeze = Clip {
                path: png,
                name: format!("{name} (freeze)"),
                duration: engine::STILL_SOURCE,
                out_s: FREEZE_HOLD,
                has_audio: false,
                effect,
                effect_amount,
                framing,
                transform,
                grade,
                volume: 0.0,
                thumb,
                ..Default::default()
            };
            let fi = {
                let mut cl = clips.write();
                match place {
                    FreezePlace::Split { local } => {
                        let mut right = cl[i].clone();
                        cl[i].out_s = local;
                        right.in_s = local;
                        // Right half follows the freeze, not the original head.
                        right.transition = "None".into();
                        cl.insert(i + 1, freeze);
                        cl.insert(i + 2, right);
                        i + 1
                    }
                    FreezePlace::Before => {
                        cl.insert(i, freeze);
                        i
                    }
                    FreezePlace::After => {
                        cl.insert(i + 1, freeze);
                        i + 1
                    }
                }
            };
            if magnet() {
                let bump = FREEZE_HOLD;
                for o in overlays.write().iter_mut() {
                    if o.at >= freeze_at - 1e-6 {
                        o.at += bump;
                    }
                }
                for a in audios.write().iter_mut() {
                    if a.at >= freeze_at - 1e-6 {
                        a.at += bump;
                    }
                }
                for ti in titles.write().iter_mut() {
                    if ti.at >= freeze_at - 1e-6 {
                        ti.at += bump;
                    }
                }
            }
            selected.set(Some(Sel::Main(fi)));
            seek_to(start_of(fi));
            status.set(format!(
                "Freeze frame held for {} — drag an edge to change the hold.",
                fmt_clip_dur(FREEZE_HOLD)
            ));
        });
    };

    // Mute (or unmute) the selected clip's own audio / A-lane bed.
    let mut mute_sel = move |_: ()| {
        match selected() {
            Some(Sel::Main(i)) => {
                push_undo("");
                let mut cl = clips.write();
                let Some(c) = cl.get_mut(i) else { return };
                if !c.has_audio {
                    status.set(format!("{} has no audio to mute.", c.name));
                    return;
                }
                if c.volume <= 0.001 {
                    c.volume = 1.0;
                    status.set(format!("{} unmuted.", c.name));
                } else {
                    c.volume = 0.0;
                    status.set(format!("{} muted.", c.name));
                }
            }
            Some(Sel::Aud(k)) => {
                push_undo("");
                let mut au = audios.write();
                let Some(a) = au.get_mut(k) else { return };
                if a.volume <= 0.001 {
                    a.volume = 1.0;
                    a.vol_end = -1.0;
                    status.set(format!("{} unmuted.", a.name));
                } else {
                    a.volume = 0.0;
                    a.vol_end = -1.0;
                    status.set(format!("{} muted.", a.name));
                }
            }
            _ => status.set("Select a V1 clip or A-lane bed to mute.".to_string()),
        }
    };

    // FCP-style Disable: item stays on the timeline but is invisible + silent
    // in preview and export. (V is already voiceover — Shift+D here.)
    let mut toggle_disable_sel = move |_: ()| {
        match selected() {
            Some(Sel::Main(i)) => {
                push_undo("");
                let mut cl = clips.write();
                let Some(c) = cl.get_mut(i) else { return };
                c.enabled = !c.enabled;
                let name = c.name.clone();
                let on = c.enabled;
                drop(cl);
                status.set(if on {
                    format!("{name} enabled — back in preview and export.")
                } else {
                    format!("{name} disabled — dimmed on the timeline, invisible and silent.")
                });
                seek_to(playhead());
            }
            Some(Sel::Over(j)) => {
                push_undo("");
                let mut ov = overlays.write();
                let Some(o) = ov.get_mut(j) else { return };
                o.enabled = !o.enabled;
                let name = o.name.clone();
                let on = o.enabled;
                drop(ov);
                status.set(if on {
                    format!("{name} enabled.")
                } else {
                    format!("{name} disabled — cutaway hidden.")
                });
                seek_to(playhead());
            }
            Some(Sel::Aud(k)) => {
                push_undo("");
                let mut au = audios.write();
                let Some(a) = au.get_mut(k) else { return };
                a.enabled = !a.enabled;
                let name = a.name.clone();
                let on = a.enabled;
                drop(au);
                status.set(if on {
                    format!("{name} enabled.")
                } else {
                    format!("{name} disabled — silent.")
                });
            }
            Some(Sel::Title(k)) => {
                push_undo("");
                let mut ts = titles.write();
                let Some(t) = ts.get_mut(k) else { return };
                t.enabled = !t.enabled;
                let on = t.enabled;
                drop(ts);
                status.set(if on {
                    "Title enabled.".into()
                } else {
                    "Title disabled — not composited.".into()
                });
                seek_to(playhead());
            }
            None => status.set("Select a clip, cutaway, bed or title to disable.".into()),
        }
    };

    // FCP-style Solo: isolate selected item's audio; non-soloed picture goes
    // B&W. Toggle off when already soloed alone, or clear all solos with a
    // second press on the same item.
    let mut toggle_solo_sel = move |_: ()| {
        match selected() {
            Some(Sel::Main(i)) => {
                push_undo("");
                let mut cl = clips.write();
                let Some(c) = cl.get_mut(i) else { return };
                let was = c.solo;
                c.solo = !was;
                let name = c.name.clone();
                drop(cl);
                status.set(if was {
                    format!("{name} unsoloed.")
                } else {
                    format!("{name} soloed — other audio silent; non-soloed clips in B&W.")
                });
                seek_to(playhead());
            }
            Some(Sel::Over(j)) => {
                push_undo("");
                let mut ov = overlays.write();
                let Some(o) = ov.get_mut(j) else { return };
                o.solo = !o.solo;
                let name = o.name.clone();
                let on = o.solo;
                drop(ov);
                status.set(if on {
                    format!("{name} soloed.")
                } else {
                    format!("{name} unsoloed.")
                });
                seek_to(playhead());
            }
            Some(Sel::Aud(k)) => {
                push_undo("");
                let mut au = audios.write();
                let Some(a) = au.get_mut(k) else { return };
                a.solo = !a.solo;
                let name = a.name.clone();
                let on = a.solo;
                drop(au);
                status.set(if on {
                    format!("{name} soloed — only soloed beds play.")
                } else {
                    format!("{name} unsoloed.")
                });
            }
            _ => status.set("Select a clip, cutaway or bed to solo.".into()),
        }
    };

    // Clear every item solo flag (timeline Solo button off).
    let mut clear_all_solos = move |_: ()| {
        let any = clips.read().iter().any(|c| c.solo)
            || overlays.read().iter().any(|o| o.solo)
            || audios.read().iter().any(|a| a.solo);
        if !any {
            return;
        }
        push_undo("");
        for c in clips.write().iter_mut() {
            c.solo = false;
        }
        for o in overlays.write().iter_mut() {
            o.solo = false;
        }
        for a in audios.write().iter_mut() {
            a.solo = false;
        }
        status.set("Solo off — full mix restored.".into());
        seek_to(playhead());
    };

    // Join two adjacent V1 clips that came from the same split (same source,
    // continuous in/out). Selection can be either half; we prefer the left of
    // the pair when both neighbors could join.
    let mut join_clips = move |_: ()| {
        let i = match selected() {
            Some(Sel::Main(i)) => i,
            _ => {
                status.set("Select a V1 clip to join with its neighbour.".to_string());
                return;
            }
        };
        let cl = clips.read();
        let join_left = i > 0 && can_join_clips(&cl[i - 1], &cl[i]);
        let join_right = i + 1 < cl.len() && can_join_clips(&cl[i], &cl[i + 1]);
        drop(cl);
        let (left, right) = if join_left {
            (i - 1, i)
        } else if join_right {
            (i, i + 1)
        } else {
            status.set(
                "Nothing to join — need an adjacent clip of the same source that still abuts in time."
                    .to_string(),
            );
            return;
        };
        push_undo("");
        let old = spans();
        let name = {
            let mut cl = clips.write();
            let right_out = cl[right].out_s;
            let right_in = cl[right].in_s;
            // Keep the wider of the two source ranges so reverse joins work.
            if cl[left].reverse {
                cl[left].in_s = cl[left].in_s.min(right_in);
                cl[left].out_s = cl[left].out_s.max(right_out);
            } else {
                cl[left].out_s = right_out;
            }
            // Transition into the right half disappears with the cut.
            let name = cl[left].name.clone();
            cl.remove(right);
            name
        };
        ride(old, &|k| {
            if k == right {
                Some(start_of(left))
            } else if k > right {
                Some(start_of(k - 1))
            } else {
                Some(start_of(k))
            }
        });
        selected.set(Some(Sel::Main(left)));
        status.set(format!("Joined halves of {}.", name));
    };

    // Instant replay: re-play the last ~1.5s of source under the playhead at
    // half speed (iMovie-style beat for short-form), then continue the clip.
    let mut instant_replay = move |_: ()| {
        const MIN: f64 = 0.1;
        let t = playhead();
        let Some((i, local)) = locate(&clips.read(), t) else {
            status.set("Park the playhead on a V1 clip for instant replay.".to_string());
            return;
        };
        let c = clips.read()[i].clone();
        let Some((rin, rout)) = replay_span(c.in_s, c.out_s, local, REPLAY_SRC * c.speed.max(0.01))
        else {
            status.set("Need a bit more clip before the playhead to replay.".to_string());
            return;
        };
        // Need room to split (or we're at the tail — still insert the replay after).
        let place = freeze_place(c.in_s, c.out_s, local, MIN);
        let insert_at = match place {
            Some(FreezePlace::Before) => start_of(i),
            Some(FreezePlace::Split { .. }) | None => t,
            Some(FreezePlace::After) => {
                start_of(i) + extents(&clips.read()).get(i).copied().unwrap_or(0.0)
            }
        };
        let bump = (rout - rin) / REPLAY_SPEED;
        push_undo("");
        let mut replay = c.clone();
        replay.in_s = rin;
        replay.out_s = rout;
        replay.speed = REPLAY_SPEED;
        replay.reverse = false;
        replay.transition = "None".into();
        replay.group = 0;
        replay.name = format!("{} (replay)", c.name);
        // Mute the replay's own audio — the double-hit of sound is usually worse
        // than silence under a slow-mo beat.
        replay.volume = 0.0;
        let ri = {
            let mut cl = clips.write();
            match place {
                Some(FreezePlace::Split { local }) => {
                    let mut right = cl[i].clone();
                    cl[i].out_s = local;
                    right.in_s = local;
                    right.transition = "None".into();
                    cl.insert(i + 1, replay);
                    cl.insert(i + 2, right);
                    i + 1
                }
                Some(FreezePlace::After) | Some(FreezePlace::Before) | None => {
                    cl.insert(i + 1, replay);
                    i + 1
                }
            }
        };
        if magnet() {
            for o in overlays.write().iter_mut() {
                if o.at >= insert_at - 1e-6 {
                    o.at += bump;
                }
            }
            for a in audios.write().iter_mut() {
                if a.at >= insert_at - 1e-6 {
                    a.at += bump;
                }
            }
            for ti in titles.write().iter_mut() {
                if ti.at >= insert_at - 1e-6 {
                    ti.at += bump;
                }
            }
        }
        selected.set(Some(Sel::Main(ri)));
        seek_to(start_of(ri));
        status.set(format!(
            "Instant replay at {}× — {} of source, drag edges to taste.",
            REPLAY_SPEED,
            fmt_clip_dur(rout - rin)
        ));
    };

    // One-click cross dissolve into the selected (or playhead) V1 clip.
    let mut add_cross_dissolve = move |_: ()| {
        let i = match selected() {
            Some(Sel::Main(i)) if i > 0 => Some(i),
            _ => locate(&clips.read(), playhead()).map(|(i, _)| i).filter(|&i| i > 0),
        };
        let Some(i) = i else {
            status.set("Select a V1 clip after the first to add a cross dissolve.".to_string());
            return;
        };
        push_undo("");
        let old = spans();
        {
            let mut cl = clips.write();
            cl[i].transition = "Cross dissolve".into();
            if cl[i].trans_dur < 0.1 {
                cl[i].trans_dur = 0.5;
            }
        }
        ride(old, &|k| Some(start_of(k)));
        selected.set(Some(Sel::Main(i)));
        seek_to(playhead().min(total_of()));
        status.set(format!(
            "Cross dissolve into {} ({}).",
            clips.read()[i].name,
            fmt_clip_dur(clips.read()[i].trans_dur)
        ));
    };

    // Copy/paste the selection across lanes. Paste drops free-position items at
    // the playhead; V1 clips insert at the cut under the playhead.
    let mut clipboard = state.clipboard;
    let mut copy_sel = move |_: ()| {
        let item = match selected() {
            Some(Sel::Main(i)) => clips.read().get(i).cloned().map(ClipboardItem::Main),
            Some(Sel::Over(j)) => overlays.read().get(j).cloned().map(ClipboardItem::Over),
            Some(Sel::Aud(k)) => audios.read().get(k).cloned().map(ClipboardItem::Aud),
            Some(Sel::Title(k)) => titles.read().get(k).cloned().map(ClipboardItem::Title),
            None => None,
        };
        match item {
            Some(it) => {
                let noun = match &it {
                    ClipboardItem::Main(_) => "Clip",
                    ClipboardItem::Over(_) => "Cutaway",
                    ClipboardItem::Aud(_) => "Audio",
                    ClipboardItem::Title(_) => "Title",
                };
                clipboard.set(Some(it));
                status.set(format!("{noun} copied — Ctrl+V pastes at the playhead."));
            }
            None => status.set("Select something on the timeline to copy.".to_string()),
        }
    };
    let mut paste_sel = move |_: ()| {
        let Some(item) = clipboard() else {
            status.set("Clipboard is empty — copy a timeline item first.".to_string());
            return;
        };
        push_undo("");
        let t = playhead().max(0.0);
        match item {
            ClipboardItem::Main(mut c) => {
                c.group = 0;
                c.transition = "None".into();
                let i = insert_index(&clips.read(), t);
                clips.write().insert(i, c);
                selected.set(Some(Sel::Main(i)));
                status.set(format!("Pasted clip at {}.", fmt_t(start_of(i))));
            }
            ClipboardItem::Over(mut o) => {
                o.group = 0;
                o.at = t;
                overlays.write().push(o);
                let j = overlays.read().len() - 1;
                selected.set(Some(Sel::Over(j)));
                status.set(format!("Pasted cutaway at {}.", fmt_t(t)));
            }
            ClipboardItem::Aud(mut a) => {
                a.group = 0;
                a.at = t;
                audios.write().push(a);
                let k = audios.read().len() - 1;
                selected.set(Some(Sel::Aud(k)));
                status.set(format!("Pasted audio at {}.", fmt_t(t)));
            }
            ClipboardItem::Title(mut ti) => {
                ti.group = 0;
                ti.at = t;
                ti.pngs.clear();
                titles.write().push(ti);
                let k = titles.read().len() - 1;
                selected.set(Some(Sel::Title(k)));
                rerender_title(k);
                status.set(format!("Pasted title at {}.", fmt_t(t)));
            }
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

    // Jump the playhead to the previous/next edit point — a V1 cut, i.e. the
    // cumulative clip boundaries (0, len[0], len[0]+len[1], …). The classic
    // Up/Down transport move; complements nudge (coarse) and , / . (one frame).
    let mut jump_edit = move |dir: i64| {
        let mut acc = 0.0;
        let mut bounds = vec![0.0];
        for e in extents(&clips.read()) {
            acc += e;
            bounds.push(acc);
        }
        const EPS: f64 = 1e-3; // don't get stuck on the boundary we're sitting on
        let now = playhead();
        let target = if dir < 0 {
            bounds.iter().rev().find(|&&b| b < now - EPS).copied().unwrap_or(0.0)
        } else {
            bounds.iter().find(|&&b| b > now + EPS).copied().unwrap_or(acc)
        };
        seek_to(target);
    };

    // On-screen transform handles over the monitor. The sliders in the
    // inspector stay the precise way in; this is the direct way.
    let mut show_handles = state.show_handles;
    // The monitor's box on screen, measured when a drag starts rather than
    // tracked, so a resized window can never leave stale geometry behind.
    let mut xf_drag = state.xf_drag;
    let mut title_drag = state.title_drag;
    // DOM element handles are per-window, never shared: window B can't focus
    // window A's node.
    let mut phone_el = use_signal(|| Option::<std::rc::Rc<MountedData>>::None);

    // Start a title seat drag — measure the phone box once, one undo step for
    // the whole drag (same contract as transform handles).
    let mut begin_title_drag = move |k: usize, from_y: f64| {
        let Some(start_y) = titles.read().get(k).map(seat_y) else { return };
        let Some(el) = phone_el() else { return };
        push_undo("title-seat");
        spawn(async move {
            if let Ok(r) = el.get_client_rect().await {
                title_drag.set(Some((k, start_y, from_y, r.origin.y, r.size.height.max(1.0))));
            }
        });
    };
    let mut end_title_drag = move || {
        let Some((k, _, _, _, _)) = title_drag() else { return };
        title_drag.set(None);
        // Re-rasterize at the final seat — during the drag only the handle moved.
        if let Some(item) = titles.write().get_mut(k) {
            item.pngs.clear();
        }
        rerender_title(k);
    };

    // Handle to the title text editor. Selecting a text title focuses it so the
    // cursor is ready to type — the way double-clicking a title in Premiere/FCP
    // drops you into text entry. Without this, keystrokes land on the app root
    // (autofocused) and fire single-key shortcuts (G/T/M/B/Space…) instead of
    // typing; the capture-phase guard only silences shortcuts *while* the field
    // itself holds focus.
    let mut title_input_el = use_signal(|| Option::<std::rc::Rc<MountedData>>::None);
    use_effect(move || {
        let sel = selected();
        let el = title_input_el();
        if let (Some(Sel::Title(k)), Some(el)) = (sel, el) {
            // .peek() so typing (which mutates `titles`) never re-runs this and
            // yanks the caret — only a selection change should refocus.
            if titles.peek().get(k).is_some_and(|t| t.kind == "Text") {
                spawn(async move { let _ = el.set_focus(true).await; });
            }
        }
    });

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

    // The selected element's timeline start, playback speed and source in-point
    // — the frame of reference a keyframe at the playhead is measured against.
    let sel_anchor = move || -> Option<(f64, f64, f64, f64)> {
        match selected() {
            Some(Sel::Main(i)) => {
                let cl = clips.read();
                let c = cl.get(i)?;
                let start: f64 = extents(&cl).iter().take(i).sum();
                Some((start, c.speed.max(0.01), c.in_s, (c.out_s - c.in_s).max(0.05)))
            }
            Some(Sel::Over(j)) => {
                let ov = overlays.read();
                let o = ov.get(j)?;
                Some((o.at, o.speed.max(0.01), o.in_s, (o.out_s - o.in_s).max(0.05)))
            }
            _ => None,
        }
    };

    // Apply an edit to the selected transform at the current clip-local playhead
    // time, then refresh the monitor *at the playhead* (not the clip start) so a
    // keyed value shows where it was set. Unlike `seek_to` this never steals the
    // selection, so an overlay's opacity can be keyed while it stays selected.
    let mut edit_sel_at = move |edit: &dyn Fn(&mut engine::AnimatedTransform, f64)| {
        let Some((start, speed, in_s, dur)) = sel_anchor() else { return };
        let t = ((playhead() - start) * speed).clamp(0.0, dur);
        let target = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                edit(&mut cl[i].transform, t);
                Some((cl[i].scrub_path(), cl[i].framing.clone(), cl[i].look()))
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                edit(&mut ov[j].transform, t);
                Some((ov[j].scrub_path(), ov[j].framing.clone(), ov[j].look()))
            }
            _ => None,
        };
        if let Some((path, fr, look)) = target {
            // Reuse the clamped clip-local time so the scrub can't seek past the
            // clip's own source range.
            request_preview(path, in_s + t, fr, look, engine::Over::default());
        }
    };

    // Whether the selected element's row-field is a curve — fills its diamond.
    let sel_field_animated = move |label: &str| -> bool {
        match selected() {
            Some(Sel::Main(i)) => {
                clips.read().get(i).map(|c| xf_field_animated(&c.transform, label)).unwrap_or(false)
            }
            Some(Sel::Over(j)) => {
                overlays.read().get(j).map(|o| xf_field_animated(&o.transform, label)).unwrap_or(false)
            }
            _ => false,
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
        // Order: orient (mirror) → place → size → spin → opacity.
        // Mirror sits first so the discrete flip is one glance from the top;
        // sliders follow in the usual place/scale/rotate stack.
        rsx! {
            h4 { class: "mr-fx-cat", "Transform" }
            if with_opacity {
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Scale below 1 makes this a picture-in-picture — V1 shows through around it."
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
            for (label, value, min, max, step, _set) in transform_knobs(&xf, with_opacity) {
                div { key: "{label}", class: "mr-xf-row",
                    Slider {
                        label: Some(label),
                        min, max, step,
                        precision: if step < 0.1 { 3 } else { 0 },
                        value,
                        oninput: Some(EventHandler::new(move |v: f64| {
                            push_undo(&format!("xf-{label}"));
                            // Keys the field in place when it's animated, sets a
                            // constant otherwise — a sibling's curve is never lost.
                            edit_sel_at(&|at, t| xf_write(at, label, v, t));
                        })),
                    }
                    // A diamond only where the engine actually animates: scale and,
                    // on a composited layer, opacity. Click drops or pulls a key at
                    // the playhead; move the playhead, change the value, it tweens.
                    if xf_keyable(label) {
                        button {
                            r#type: "button",
                            class: if sel_field_animated(label) { "mr-kf-diamond on" } else { "mr-kf-diamond" },
                            title: "Keyframe at the playhead",
                            onclick: move |_| {
                                push_undo("keyframe");
                                edit_sel_at(&|at, t| xf_toggle_key(at, label, t));
                                status.set(
                                    "Keyframe toggled at the playhead — set a second one elsewhere to animate."
                                        .to_string(),
                                );
                            },
                            if sel_field_animated(label) { "◆" } else { "◇" }
                        }
                    }
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
    let mut presets = state.presets;
    let mut show_save_preset = state.show_save_preset;
    let mut preset_name = state.preset_name;

    // The iMovie-style visual style browser — the built-in looks plus the user's
    // saved presets as clickable tiles. Shared by the Title inspector and the
    // Text phase; `at_playhead` only changes the tooltip, `disabled` greys the
    // tiles when there is no clip to drop a title onto.
    let title_gallery = move |disabled: bool, at_playhead: bool| {
        rsx! {
            div { class: "mr-title-gallery",
                for p in builtin_title_styles().into_iter().chain(presets.read().iter().cloned()) {
                    {
                        let pname = p.name.clone();
                        let css = title_preview_css(&p.style);
                        let sample = if p.style.karaoke {
                            "♪ TITLE"
                        } else if p.style.reveal {
                            "TITLE…"
                        } else {
                            "TITLE TEXT"
                        };
                        let tip = if at_playhead {
                            format!("Add \"{pname}\" at the playhead")
                        } else {
                            format!("Apply \"{pname}\"")
                        };
                        rsx! {
                            button {
                                key: "{pname}",
                                class: "mr-title-tile",
                                title: "{tip}",
                                disabled,
                                onclick: move |_| apply_title_style(p.clone()),
                                div { class: "mr-title-preview", style: "{css}",
                                    span { "{sample}" }
                                }
                                span { class: "mr-title-tile-name", "{pname}" }
                            }
                        }
                    }
                }
            }
        }
    };
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
    use_shortcut(Some(" ".into()), is_main.then(|| EventHandler::new(move |()| toggle_play(()))));
    use_shortcut(Some("BACKSPACE".into()), is_main.then(|| EventHandler::new(move |()| delete_sel(()))));
    use_shortcut(Some("ARROWLEFT".into()), is_main.then(|| EventHandler::new(move |()| nudge(-0.1))));
    use_shortcut(Some("ARROWRIGHT".into()), is_main.then(|| EventHandler::new(move |()| nudge(0.1))));
    use_shortcut(Some("SHIFT+ARROWLEFT".into()), is_main.then(|| EventHandler::new(move |()| nudge(-1.0))));
    use_shortcut(Some("SHIFT+ARROWRIGHT".into()), is_main.then(|| EventHandler::new(move |()| nudge(1.0))));
    use_shortcut(Some("HOME".into()), is_main.then(|| EventHandler::new(move |()| seek_to(0.0))));
    use_shortcut(Some("END".into()), is_main.then(|| EventHandler::new(move |()| seek_to(total_of()))));
    // Frame-step the playhead (1/30 s — engine::FPS). Finer than the arrows'
    // 0.1s; `,`/`.` is the editor-standard single-frame scrub.
    let frame = 1.0 / engine::FPS as f64;
    use_shortcut(Some(",".into()), is_main.then(|| EventHandler::new(move |()| seek_to((playhead() - frame).max(0.0)))));
    use_shortcut(Some(".".into()), is_main.then(|| EventHandler::new(move |()| seek_to((playhead() + frame).min(total_of())))));
    // Ripple-trim the selected clip a frame at a time — keyboard precision edit.
    // Alt+,/. nudge the clip's OUT (end); add Shift for the IN (start). Modifier
    // order must match the registry's CTRL+SHIFT+ALT build order.
    use_shortcut(Some("ALT+,".into()), is_main.then(|| EventHandler::new(move |()| ripple_trim(true, -1.0))));
    use_shortcut(Some("ALT+.".into()), is_main.then(|| EventHandler::new(move |()| ripple_trim(true, 1.0))));
    use_shortcut(Some("SHIFT+ALT+,".into()), is_main.then(|| EventHandler::new(move |()| ripple_trim(false, -1.0))));
    use_shortcut(Some("SHIFT+ALT+.".into()), is_main.then(|| EventHandler::new(move |()| ripple_trim(false, 1.0))));
    // Prev/next edit point (V1 cut).
    use_shortcut(Some("ARROWUP".into()), is_main.then(|| EventHandler::new(move |()| jump_edit(-1))));
    use_shortcut(Some("ARROWDOWN".into()), is_main.then(|| EventHandler::new(move |()| jump_edit(1))));
    use_shortcut(Some("[".into()), is_main.then(|| EventHandler::new(move |()| step_sel(-1))));
    use_shortcut(Some("]".into()), is_main.then(|| EventHandler::new(move |()| step_sel(1))));
    use_shortcut(Some("ESCAPE".into()), is_main.then(|| EventHandler::new(move |()| {
        if vo_session().is_some() {
            stop_voiceover(());
        } else {
            ctx_menu.set(None);
        }
    })));
    use_shortcut(Some("V".into()), is_main.then(|| EventHandler::new(move |()| toggle_voiceover(()))));
    use_shortcut(Some("G".into()), is_main.then(|| EventHandler::new(move |()| toggle_safe(()))));
    use_shortcut(Some("T".into()), is_main.then(|| EventHandler::new(move |()| toggle_handles(()))));
    use_shortcut(Some("M".into()), is_main.then(|| EventHandler::new(move |()| drop_marker(()))));
    use_shortcut(Some("SHIFT+M".into()), is_main.then(|| EventHandler::new(move |()| clear_markers(()))));
    use_shortcut(Some("B".into()), is_main.then(|| EventHandler::new(move |()| analyze_beats(()))));
    // Disable/Solo: FCP uses V / Option-S; V is voiceover here, so Shift+D / Alt+S.
    use_shortcut(Some("SHIFT+D".into()), is_main.then(|| EventHandler::new(move |()| toggle_disable_sel(()))));
    use_shortcut(Some("ALT+S".into()), is_main.then(|| EventHandler::new(move |()| toggle_solo_sel(()))));
    // The menu item binds "~"; this covers layouts where ~ is Shift+` and the
    // combo therefore arrives as SHIFT+~.
    use_shortcut(Some("SHIFT+~".into()), is_main.then(|| EventHandler::new(move |()| toggle_magnet(()))));

    // Window chrome preference (frameless / native / tiling), persisted like
    // the blogger theme editor; takes effect on next launch.
    let active_mode = UiMode::active();
    let mut preferred_mode = state.preferred_mode;
    let mut show_about = state.show_about;
    let mut show_shortcuts = state.show_shortcuts;
    let mut show_settings = state.show_settings;
    let mut settings_tab = state.settings_tab;
    // Editing-key scheme (persisted app-wide) + its picker. Reactive: the Split
    // menu item below reads key_scheme().split(), and MorMenuItem rebinds the key
    // when that prop changes, so switching schemes remaps the blade live.
    let mut key_scheme = state.key_scheme;
    let mut show_keys = state.show_keys;
    // The active workflow phase drives the inspector and the bottom bar; it is
    // declared up by `seek_to` (which needs to read it). Selecting a timeline
    // item jumps to its phase; `.peek()` keeps that from re-firing when the phase
    // itself changes (a bar click), so the two never fight in a loop.
    use_effect(move || {
        if !is_main {
            return; // popped inspector follows the shared phase, doesn't drive it
        }
        let target = phase_for_selection(selected(), *active_phase.peek());
        if *active_phase.peek() != target {
            active_phase.set(target);
        }
    });
    // Sub-tabs inside the Style phase — the dense one — split into Look (grade +
    // framing) and Transform (position/scale/rotate + Ken Burns) so neither view
    // is a long scroll.
    let mut style_tab = state.style_tab;
    // Ctrl+, — the platform-conventional "preferences" shortcut.
    use_shortcut(Some("Ctrl+,".into()), is_main.then(|| EventHandler::new(move |()| show_settings.set(true))));
    let mut set_mode = move |m: UiMode| {
        preferred_mode.set(m);
        let _ = m.save_preference();
        status.set(format!("Window mode → {m} (applies on next launch)"));
    };
    let radio = move |m: UiMode| if preferred_mode() == m { "●" } else { "○" };

    // Pop-out program monitor: the monitor MOVES to its own OS window — the
    // embedded phone hides while it's out, and closing the window docks it back.
    let mut monitor_out = state.monitor_out;
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

    // Pop the inspector into its own OS window. Same trick as the monitor, but
    // the whole `Editor` runs there in `Inspector` view over the *shared* state
    // bundle — so it's one inspector on the same project, on a second screen if
    // you like, not a copy. Closing the window re-enables the button (use_drop).
    let mut inspector_out = use_signal(|| false);
    let mut open_inspector_win = move || {
        if inspector_out() {
            return;
        }
        use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
        let dom = VirtualDom::new_with_props(
            PoppedInspector,
            PoppedInspectorProps { state, out: inspector_out },
        );
        let cfg = Config::new()
            .with_menu(None::<dioxus::desktop::muda::Menu>)
            .with_window(
                WindowBuilder::new()
                    .with_title("MorReel Inspector")
                    .with_inner_size(LogicalSize::new(430.0, 900.0)),
            );
        let _ = dioxus::desktop::window().new_window(dom, cfg);
        inspector_out.set(true);
        status.set("Inspector popped out — close its window to dock it back.".to_string());
    };

    // Timeline zoom (status-bar + appearance popover) and middle-mouse panning.
    let mut zoom = state.zoom;
    let mut clip_appear = state.clip_appear;
    let mut clip_height = state.clip_height;
    let mut show_clip_names = state.show_clip_names;
    let mut show_appear = state.show_appear;
    let mut pan = state.pan;

    // Timeline scale (px per second), shared by the rsx and the drag handlers.
    // ponytail: keyed to the shortest clip (min 48px wide) — no per-clip
    // min-width, so ruler/playhead geometry stays exact.
    let calc_scale = move || {
        let min_dur = clips.read().iter().map(Clip::trimmed).fold(f64::MAX, f64::min);
        ((48.0 / min_dur).clamp(14.0, 240.0) * zoom()).clamp(2.0, 960.0)
    };
    let mut zoom_by = move |factor: f64| {
        zoom.set((zoom() * factor).clamp(0.25, 6.0));
    };
    // Timeline zoom — FCP ⌘+/⌘− (Ctrl on Linux/Windows).
    use_shortcut(Some("CTRL+=".into()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.25))));
    use_shortcut(Some("CTRL+-".into()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.0 / 1.25))));
    use_shortcut(Some("CTRL++".into()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.25))));
    // Clip appearance modes — FCP Control-Option-1…6.
    use_shortcut(Some("CTRL+ALT+1".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Wave))));
    use_shortcut(Some("CTRL+ALT+2".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::WaveFilm))));
    use_shortcut(Some("CTRL+ALT+3".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Equal))));
    use_shortcut(Some("CTRL+ALT+4".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::FilmWave))));
    use_shortcut(Some("CTRL+ALT+5".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Film))));
    use_shortcut(Some("CTRL+ALT+6".into()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Labels))));
    // Clip height — FCP Control-Option-Up/Down for waveform size.
    use_shortcut(Some("CTRL+ALT+ARROWUP".into()), is_main.then(|| EventHandler::new(move |()| {
        clip_height.set((clip_height() + 0.15).min(2.0));
    })));
    use_shortcut(Some("CTRL+ALT+ARROWDOWN".into()), is_main.then(|| EventHandler::new(move |()| {
        clip_height.set((clip_height() - 0.15).max(0.5));
    })));

    // Files dragged in from the file manager. The lane under the cursor decides
    // what the file becomes; `route_drop` has the final say when the two
    // disagree. V1 collects its files into one batch so a multi-file drop is a
    // single undo step and lands in the order they were dropped.
    let mut drop_hover = state.drop_hover;
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
    let mut drag = state.drag;
    let mut drag_moved = state.drag_moved;
    // On-lane audio handles. Fade: (audio index, is_out, grab x, fade at grab).
    // Volume: (audio index, grab y, volume at grab). Both are separate from the
    // move-drag above so grabbing a corner shapes the clip instead of sliding it.
    let mut fade_drag = state.fade_drag;
    let mut vol_drag = state.vol_drag;
    // Edge-resize drag for titles / cutaways / V1 trims. Separate from the move
    // drag so grabbing an edge stretches or trims instead of sliding the card.
    let mut len_drag = state.len_drag;
    // Ruler scrub: mousedown on the ruler seeks and keeps seeking while held.
    let mut scrubbing = state.scrubbing;

    // Inspector chrome: docked in the work row, floated as an in-app panel, or
    // hidden so the monitor + timeline take the full width.
    // Per-window inspector chrome (see EditorState note): a popped inspector
    // always shows itself, docked, regardless of the main window's dock state.
    let mut insp_open = use_signal(|| true);
    let mut insp_float = use_signal(|| false);
    // Floated panel geometry (logical px). Set when the panel undocks so move
    // and resize share one coordinate system instead of fighting CSS defaults.
    let mut float_xy = use_signal(|| Option::<(f64, f64)>::None);
    let mut float_size = use_signal(|| Option::<(f64, f64)>::None);
    // Active float interaction: which grip, pointer origin, panel origin + size.
    let mut float_drag = use_signal(|| Option::<(FloatGrab, f64, f64, f64, f64, f64, f64)>::None);
    let mut show_effects = state.show_effects;
    let mut show_add = state.show_add;

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
    let mut layouts = state.layouts;
    let mut show_save_layout = state.show_save_layout;
    let mut layout_name = state.layout_name;
    let mut is_fullscreen = state.is_fullscreen;

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
    use_shortcut(Some("F11".into()), is_main.then(|| EventHandler::new(move |()| toggle_fullscreen(()))));

    // Effects browser thumbnails: the selected item's poster frame through
    // every effect, generated lazily and cached until the frame changes.
    let mut fx_thumbs = state.fx_thumbs;
    let mut fx_key = state.fx_key;
    use_effect(move || {
        if !is_main || !show_effects() {
            return; // thumbs are shared state; the main window builds them once
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
            for (_, name, filter) in all_effects() {
                if fx_key() != key {
                    return; // selection moved on — this batch is stale
                }
                if let Ok(uri) = engine::frame_data_uri(&path, t, 108, 192, &fr, &filter, engine::Over::default()).await {
                    if fx_key() == key {
                        fx_thumbs.write().insert(name, uri);
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
    let effect_names: Vec<String> = all_effects().into_iter().map(|(_, n, _)| n).collect();

    // Plugin Hub rows for the panel. Reading `hub_gen` subscribes the whole editor
    // so an install/toggle re-renders both this panel and the effect picker. Only
    // loaded while the panel is open — a few small JSON files, re-read live.
    let _ = hub_gen();
    let hub_configured = hub::hub_dir().is_some();
    let hub_rows: Vec<(hub::Manifest, bool, bool)> = match (show_hub(), hub::hub_dir()) {
        (true, Some(dir)) => {
            let state = hub::InstallState::load();
            hub::load_manifests(&dir)
                .into_iter()
                .map(|m| {
                    let (i, e) = (state.is_installed(&m.id), state.is_enabled(&m.id));
                    (m, i, e)
                })
                .collect()
        }
        _ => Vec::new(),
    };

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
            // A popped inspector is chrome-free: no menu bar, no status bar, no
            // title/header strip (the OS window decorations handle close/move),
            // and the phase bar below is gated too — just the inspector.
            hide_statusbar: !is_main,
            show_headerbar: if is_main { None } else { Some(false) },
            menu: is_main.then(|| rsx! {
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
                        label: "Project settings…".to_string(),
                        on_action: move |_| show_settings.set(true),
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
                // "Insert" — everything you place on the timeline by hand. Lifted
                // out of File so File is pure project I/O (OBS / NLE convention).
                MorMenuDropdown { label: "Insert".to_string(),
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
                    MenuSeparator {}
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
                        label: if vo_session().is_some() {
                            "Stop voiceover".to_string()
                        } else {
                            "Record voiceover (A2)".to_string()
                        },
                        shortcut: Some("V".to_string()),
                        disabled: exporting,
                        on_action: move |_| toggle_voiceover(()),
                    }
                    MenuItem {
                        label: "Add text (T)".to_string(),
                        shortcut: Some("Ctrl+T".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_title(()),
                    }
                }
                // "Tools" — automated / analysis passes and plugin surfaces, the
                // "smart" actions (matches OBS + the blogger editor's Tools menu).
                MorMenuDropdown { label: "Tools".to_string(),
                    MenuItem {
                        label: "Auto-cut silence…".to_string(),
                        disabled: no_clips || autocut_busy(),
                        on_action: move |_| show_autocut.set(true),
                    }
                    MenuSeparator {}
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
                        label: "Detect beats from music".to_string(),
                        shortcut: Some("B".to_string()),
                        disabled: audios.read().is_empty(),
                        on_action: move |_| analyze_beats(()),
                    }
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
                        label: "Effects palette…".to_string(),
                        on_action: move |_| show_effects.set(true),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Plugin Hub…".to_string(),
                        on_action: move |_| show_hub.set(true),
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
                        label: "Add freeze frame".to_string(),
                        shortcut: Some("F".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_freeze_frame(()),
                    }
                    MenuItem {
                        label: "Instant replay".to_string(),
                        shortcut: Some("Ctrl+R".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| instant_replay(()),
                    }
                    MenuItem {
                        label: "Add cross dissolve".to_string(),
                        shortcut: Some("Ctrl+D".to_string()),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_cross_dissolve(()),
                    }
                    MenuItem {
                        label: "Join clips".to_string(),
                        shortcut: Some("Ctrl+J".to_string()),
                        disabled: !matches!(selected(), Some(Sel::Main(_))) || exporting,
                        on_action: move |_| join_clips(()),
                    }
                    MenuItem {
                        label: "Ripple delete".to_string(),
                        shortcut: Some("Delete".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| delete_sel(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Copy".to_string(),
                        shortcut: Some("Ctrl+C".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| copy_sel(()),
                    }
                    MenuItem {
                        label: "Paste at playhead".to_string(),
                        shortcut: Some("Ctrl+V".to_string()),
                        disabled: clipboard().is_none() || exporting,
                        on_action: move |_| paste_sel(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Mute audio".to_string(),
                        shortcut: Some("Ctrl+Shift+M".to_string()),
                        disabled: !matches!(
                            selected(),
                            Some(Sel::Main(i)) if clips.read().get(i).is_some_and(|c| c.has_audio)
                        ) && !matches!(selected(), Some(Sel::Aud(_))),
                        on_action: move |_| mute_sel(()),
                    }
                    MenuItem {
                        label: {
                            let en = match selected() {
                                Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.enabled),
                                Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.enabled),
                                Some(Sel::Aud(k)) => audios.read().get(k).map(|a| a.enabled),
                                Some(Sel::Title(k)) => titles.read().get(k).map(|t| t.enabled),
                                None => None,
                            };
                            if en == Some(false) {
                                "Enable".to_string()
                            } else {
                                "Disable".to_string()
                            }
                        },
                        shortcut: Some("Shift+D".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| toggle_disable_sel(()),
                    }
                    MenuItem {
                        label: {
                            let sol = match selected() {
                                Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.solo),
                                Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.solo),
                                Some(Sel::Aud(k)) => audios.read().get(k).map(|a| a.solo),
                                _ => None,
                            };
                            if sol == Some(true) {
                                "Unsolo".to_string()
                            } else {
                                "Solo".to_string()
                            }
                        },
                        shortcut: Some("Alt+S".to_string()),
                        disabled: !matches!(
                            selected(),
                            Some(Sel::Main(_)) | Some(Sel::Over(_)) | Some(Sel::Aud(_))
                        ),
                        on_action: move |_| toggle_solo_sel(()),
                    }
                    MenuItem {
                        label: "Clear all solos".to_string(),
                        disabled: !(clips.read().iter().any(|c| c.solo)
                            || overlays.read().iter().any(|o| o.solo)
                            || audios.read().iter().any(|a| a.solo)),
                        on_action: move |_| clear_all_solos(()),
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
                }
                MorMenuDropdown { label: "Playback".to_string(),
                    MenuItem {
                        label: if playing() { "Pause".to_string() } else { "Play".to_string() },
                        shortcut: Some("Space".to_string()),
                        disabled: no_clips,
                        on_action: move |_| toggle_play(()),
                    }
                    MenuItem {
                        label: "Play from beginning".to_string(),
                        shortcut: Some("Ctrl+Home".to_string()),
                        disabled: no_clips,
                        on_action: move |_| play_from_start(()),
                    }
                    MenuItem {
                        label: format!("{} Loop playback", if loop_playback() { "●" } else { "○" }),
                        shortcut: Some("Ctrl+L".to_string()),
                        disabled: no_clips,
                        on_action: move |_| toggle_loop(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: if vo_session().is_some() {
                            "● Stop voiceover".to_string()
                        } else {
                            "Record voiceover…".to_string()
                        },
                        shortcut: Some("V".to_string()),
                        disabled: exporting,
                        on_action: move |_| toggle_voiceover(()),
                    }
                    MenuSeparator {}
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
                        label: "Zoom timeline in".to_string(),
                        shortcut: Some("Ctrl+=".to_string()),
                        on_action: move |_| zoom_by(1.25),
                    }
                    MenuItem {
                        label: "Zoom timeline out".to_string(),
                        shortcut: Some("Ctrl+-".to_string()),
                        on_action: move |_| zoom_by(1.0 / 1.25),
                    }
                    MenuItem {
                        label: "Clip appearance…".to_string(),
                        on_action: move |_| show_appear.set(!show_appear()),
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
                    MenuItem {
                        label: format!("{} Magnetic timeline", if magnet() { "●" } else { "○" }),
                        shortcut: Some("~".to_string()),
                        on_action: move |_| toggle_magnet(()),
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
                        title: "Zoom timeline out (Ctrl+-)",
                        onclick: move |_| zoom_by(1.0 / 1.25),
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
                        title: "Zoom timeline in (Ctrl+=)",
                        onclick: move |_| zoom_by(1.25),
                        "⊕"
                    }
                    button {
                        class: if show_appear() { "mr-appear-btn on" } else { "mr-appear-btn" },
                        title: "Clip appearance — zoom, filmstrip / waveform layout, clip height",
                        onclick: move |_| show_appear.set(!show_appear()),
                        "⧉"
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
                    if is_main {
                    div { class: "mr-preview-col",
                        if !monitor_out() {
                            div { class: "mr-stage",
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
                                // Title seat handle — drag the words up/down on the
                                // phone the way iMovie lets you pull a title around.
                                // Live: only the handle (and a ghost line) move; the
                                // rasterized card re-renders on mouseup.
                                if let Some(Sel::Title(tk)) = selected() {
                                    if let Some(tt) = titles.read().get(tk).cloned() {
                                        {
                                            let y = seat_y(&tt) * 100.0;
                                            // Rough text block height so the box sits
                                            // over the glyphs rather than above them.
                                            let block_h = ((tt.font_size / 1920.0) * 1.4
                                                * tt.text.lines().count().max(1) as f64
                                                * 100.0)
                                                .clamp(4.0, 28.0);
                                            let dragging = title_drag().is_some_and(|(k, ..)| k == tk);
                                            let ghost = if dragging {
                                                tt.text.replace('\n', " ")
                                            } else {
                                                String::new()
                                            };
                                            let color = title_color(&tt.color);
                                            rsx! {
                                                div {
                                                    class: "mr-title-handle-layer",
                                                    style: if dragging { "pointer-events:auto" } else { "pointer-events:none" },
                                                    onmousemove: move |evt| {
                                                        let Some((k, start_y, from_y, _top, h)) = title_drag() else { return };
                                                        let p = evt.client_coordinates();
                                                        let mut y = start_y + (p.y - from_y) / h;
                                                        if evt.modifiers().shift() {
                                                            // Snap to named seats while Shift is held.
                                                            if let Some(name) = nearest_title_pos(y) {
                                                                y = title_y(name);
                                                            }
                                                        }
                                                        if let Some(item) = titles.write().get_mut(k) {
                                                            set_seat_y(item, y);
                                                        }
                                                    },
                                                    onmouseup: move |_| end_title_drag(),
                                                    onmouseleave: move |_| {
                                                        if title_drag().is_some() {
                                                            end_title_drag();
                                                        }
                                                    },
                                                    div {
                                                        class: if dragging { "mr-title-box dragging" } else { "mr-title-box" },
                                                        style: "top:{y}%;height:{block_h}%",
                                                        title: "Drag to move text \u{2014} hold Shift to snap to Top / Middle / Lower third",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            let p = evt.client_coordinates();
                                                            begin_title_drag(tk, p.y);
                                                        },
                                                        if dragging && !ghost.is_empty() {
                                                            span {
                                                                class: "mr-title-ghost",
                                                                style: "color:{color}",
                                                                "{ghost}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            }
                        }
                        if !clips.read().is_empty() {
                            // iMovie-style format bar: the knobs you reach for while
                            // looking at the words on the picture, not buried under
                            // the inspector scroll. Only when a text card is selected.
                            if let Some(Sel::Title(tk)) = selected() {
                                if let Some(tt) = titles.read().get(tk).filter(|t| t.kind == "Text").cloned() {
                                    {
                                    let fonts: Vec<String> = engine::font_families().to_vec();
                                    rsx! {
                                        div { class: "mr-format-bar", title: "Format the selected text",
                                            select {
                                                class: "mr-format-select",
                                                title: "Font",
                                                value: "{tt.font}",
                                                onkeydown: move |evt| evt.stop_propagation(),
                                                onchange: move |evt| {
                                                    let v = evt.value();
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        item.font = v;
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(tk);
                                                },
                                                for f in fonts {
                                                    option { key: "{f}", value: "{f}", selected: f == tt.font, "{f}" }
                                                }
                                            }
                                            button {
                                                class: "mor-btn mr-format-step",
                                                title: "Smaller",
                                                onclick: move |_| {
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        item.font_size = (item.font_size - 8.0).max(40.0);
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(tk);
                                                },
                                                "−"
                                            }
                                            span { class: "mr-format-size", "{tt.font_size as u32}" }
                                            button {
                                                class: "mor-btn mr-format-step",
                                                title: "Larger",
                                                onclick: move |_| {
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        item.font_size = (item.font_size + 8.0).min(240.0);
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(tk);
                                                },
                                                "+"
                                            }
                                            div { class: "mr-format-sep" }
                                            for (label, al) in [("⫷", "Left"), ("☰", "Centre"), ("⫸", "Right")] {
                                                button {
                                                    key: "{al}",
                                                    class: if tt.align == al { "mor-btn active mr-format-step" } else { "mor-btn mr-format-step" },
                                                    title: "Align {al}",
                                                    onclick: move |_| {
                                                        if let Some(item) = titles.write().get_mut(tk) {
                                                            item.align = al.to_string();
                                                            item.pngs.clear();
                                                        }
                                                        rerender_title(tk);
                                                    },
                                                    "{label}"
                                                }
                                            }
                                            div { class: "mr-format-sep" }
                                            button {
                                                class: if tt.outline > 0.0 { "mor-btn active mr-format-step" } else { "mor-btn mr-format-step" },
                                                title: if tt.outline > 0.0 { "Outline on — click to clear" } else { "Add outline" },
                                                onclick: move |_| {
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        item.outline = if item.outline > 0.0 { 0.0 } else { 4.0 };
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(tk);
                                                },
                                                "O"
                                            }
                                            button {
                                                class: if tt.boxed { "mor-btn active mr-format-step" } else { "mor-btn mr-format-step" },
                                                title: "Caption plate behind the words",
                                                onclick: move |_| {
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        item.boxed = !item.boxed;
                                                        item.pngs.clear();
                                                    }
                                                    rerender_title(tk);
                                                },
                                                "◫"
                                            }
                                            div { class: "mr-format-sep" }
                                            for (name, hex) in TITLE_COLORS {
                                                button {
                                                    key: "{name}",
                                                    class: if tt.color == *name { "mr-format-swatch active" } else { "mr-format-swatch" },
                                                    title: "{name}",
                                                    style: "background:{hex}",
                                                    onclick: move |_| {
                                                        if let Some(item) = titles.write().get_mut(tk) {
                                                            item.color = name.to_string();
                                                            item.pngs.clear();
                                                        }
                                                        rerender_title(tk);
                                                    },
                                                }
                                            }
                                            div { class: "mr-format-sep" }
                                            for (name, _) in TITLE_POS {
                                                button {
                                                    key: "{name}",
                                                    class: if seat_matches(&tt, name) { "mor-btn active mr-format-pos" } else { "mor-btn mr-format-pos" },
                                                    title: "Seat at {name}",
                                                    onclick: move |_| {
                                                        if let Some(item) = titles.write().get_mut(tk) {
                                                            set_seat_named(item, name);
                                                            item.pngs.clear();
                                                        }
                                                        rerender_title(tk);
                                                    },
                                                    "{name}"
                                                }
                                            }
                                            button {
                                                class: "mor-btn mr-format-reset",
                                                title: "Reset look to the plain default (keeps your words and timing)",
                                                onclick: move |_| {
                                                    push_undo("");
                                                    if let Some(item) = titles.write().get_mut(tk) {
                                                        *item = restyle(item, &base_title());
                                                    }
                                                    rerender_title(tk);
                                                    status.set("Text look reset.".to_string());
                                                },
                                                "Reset"
                                            }
                                        }
                                    }
                                    }
                                }
                            }
                            div { class: "mr-scrub",
                                // Deck counter: the master timecode readout — amber at
                                // rest, record-red while the transport is rolling.
                                div { class: if playing() { "mr-deck playing" } else { "mr-deck" },
                                    span { "{fmt_t(playhead().min(total))}" }
                                    span { class: "mr-deck-total", "/ {fmt_t(total)}" }
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
                    } // if is_main (monitor column)

                    if is_main && !insp_open() {
                        button {
                            class: "mr-insp-reopen",
                            title: "Show the inspector panel",
                            onclick: move |_| insp_open.set(true),
                            "Inspector ›"
                        }
                    }

                    if insp_open() {
                    div {
                        class: if !is_main { "mr-inspector mr-inspector-solo" } else if insp_float() { "mr-inspector mr-float-panel" } else { "mr-inspector" },
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
                        if !is_main {
                            // The popped window announces what it edits, in that
                            // element's colour — its one bold, identifying mark.
                            {
                                let (kc, glyph, title, eyebrow) =
                                    solo_identity(selected(), active_phase(), &titles.read());
                                rsx! {
                                    div { class: "mr-solo-head", style: "--kind:{kc};",
                                        span { class: "mr-solo-eyebrow", "{eyebrow}" }
                                        div { class: "mr-solo-title-row",
                                            span { class: "mr-solo-badge", "{glyph}" }
                                            span { class: "mr-solo-title", "{title}" }
                                        }
                                    }
                                }
                            }
                        }
                        if is_main {
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
                                {
                                    let base = if insp_float() { "Inspector · floating" } else { "Inspector" };
                                    match sel_noun(selected(), &titles.read()) {
                                        Some(noun) => format!("{noun} · {base}"),
                                        None => base.to_string(),
                                    }
                                }
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
                                if is_main {
                                button {
                                    class: "mr-panel-btn",
                                    disabled: inspector_out(),
                                    title: "Open the inspector in its own window — edit on a second screen",
                                    onclick: move |e| {
                                        e.stop_propagation();
                                        open_inspector_win();
                                    },
                                    "⧉+"
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
                        }
                        } // if is_main (panel head)
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
                        // Workspace actions (Add / browse FX / Export) belong to the
                        // main window; the popped inspector stays a focused editor
                        // for the selected item.
                        if is_main {
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
                                    Phase::Effects => "◧ Key & FX",
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
                        }

                        if let Some(p) = export_progress() {
                            div { class: "mr-progress",
                                div { style: "width: {p * 100.0:.1}%" }
                            }
                        }

                        if active_phase() == Phase::Add {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "Main clips go on V1, cutaways on V2, and music or voiceover underneath."
                            }
                            div { class: "mr-phase-actions",
                                button { class: "mor-btn primary", disabled: exporting, onclick: move |_| import_clips(()), "＋ Add clips (V1)" }
                                button { class: "mor-btn", disabled: exporting, onclick: move |_| add_overlay(()), "⧉ Add b-roll (V2)" }
                                button { class: "mor-btn", onclick: move |_| add_audio(1), "♪ Add music (A1)" }
                                button { class: "mor-btn", onclick: move |_| add_audio(2), "🎙 Add audio (A2)" }
                                button {
                                    class: if vo_session().is_some() { "mor-btn primary" } else { "mor-btn" },
                                    disabled: exporting,
                                    title: "Record from the microphone onto A2 at the playhead (V)",
                                    onclick: move |_| toggle_voiceover(()),
                                    if vo_session().is_some() { "■ Stop VO" } else { "● Record VO" }
                                }
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
                        } else if active_phase() == Phase::Effects {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "Chroma-key a green/blue screen, colour- or luma-key a plate to transparency, or blend an image/particle layer over V1. Select a V1 clip or V2 overlay, then open the palette."
                            }
                            div { class: "mr-phase-actions",
                                button {
                                    class: "mor-btn primary",
                                    disabled: no_clips,
                                    onclick: move |_| show_effects.set(true),
                                    "◧ Key & image effects…"
                                }
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
                                    if active_phase() == Phase::Style {
                                    MorTabs {
                                        tabs: vec!["Look".to_string(), "Transform".to_string()],
                                        active: style_tab(),
                                        onchange: move |t: String| style_tab.set(t),
                                    }
                                    if style_tab() == "Look" {
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
                                    // Quick presets + reverse, mirroring iMovie's Speed menu.
                                    div { class: "mr-toolbar",
                                        for (lbl, sp) in [("Slow", 0.5f64), ("1×", 1.0), ("Fast", 2.0)] {
                                            button {
                                                key: "{lbl}",
                                                class: if (c.speed - sp).abs() < 0.001 { "mor-btn active" } else { "mor-btn" },
                                                onclick: move |_| {
                                                    push_undo(&format!("speed{i}"));
                                                    let old = spans();
                                                    clips.write()[i].speed = sp;
                                                    ride(old, &|k| Some(start_of(k)));
                                                },
                                                "{lbl}"
                                            }
                                        }
                                        button {
                                            class: if c.reverse { "mor-btn active" } else { "mor-btn" },
                                            onclick: move |_| {
                                                push_undo(&format!("rev{i}"));
                                                let r = !clips.read()[i].reverse;
                                                clips.write()[i].reverse = r;
                                                seek_to(playhead());
                                            },
                                            "⇄ Reverse"
                                        }
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
                                        // Clean up the clip's own audio in place, iMovie-style —
                                        // same denoise + EQ presets as the A1 lane.
                                        Slider {
                                            label: Some("Reduce background noise"),
                                            min: 0.0,
                                            max: 1.0,
                                            step: 0.05,
                                            precision: 2,
                                            value: c.denoise,
                                            oninput: Some(EventHandler::new(move |v: f64| {
                                                push_undo(&format!("cden{i}"));
                                                clips.write()[i].denoise = v;
                                            })),
                                        }
                                        MorSelect {
                                            label: "Equalizer".to_string(),
                                            value: c.treat.clone(),
                                            options: engine::AUDIO_TREATS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                            onchange: move |v: String| {
                                                push_undo("");
                                                clips.write()[i].treat = v;
                                            },
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
                                    div { class: "mr-toolbar",
                                        for (lbl, sp) in [("Slow", 0.5f64), ("1×", 1.0), ("Fast", 2.0)] {
                                            button {
                                                key: "{lbl}",
                                                class: if (o.speed - sp).abs() < 0.001 { "mor-btn active" } else { "mor-btn" },
                                                onclick: move |_| {
                                                    push_undo(&format!("ospeed{j}"));
                                                    overlays.write()[j].speed = sp;
                                                },
                                                "{lbl}"
                                            }
                                        }
                                        button {
                                            class: if o.reverse { "mor-btn active" } else { "mor-btn" },
                                            onclick: move |_| {
                                                push_undo(&format!("orev{j}"));
                                                let r = !overlays.read()[j].reverse;
                                                overlays.write()[j].reverse = r;
                                                seek_to(playhead());
                                            },
                                            "⇄ Reverse"
                                        }
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
                                    if t.kind == "Text" {
                                        // A real multi-line field: Enter makes a line break,
                                        // no magic "\n" to type. The render is debounced and
                                        // the old card is left on the monitor while typing, so
                                        // the caret never fights an async re-render. Kept at the
                                        // top of the inspector so the words you're editing lead.
                                        div { class: "mor-input-wrapper",
                                            div { class: "mor-input-label", "Text — Enter for a new line" }
                                            textarea {
                                                class: "mor-input mr-text-area",
                                                rows: "3",
                                                autofocus: true,
                                                value: "{t.text}",
                                                // Grab the caret the instant the editor appears so
                                                // keystrokes type instead of firing shortcuts. The
                                                // effect above re-focuses when you switch between
                                                // already-open text cards (the node isn't remounted
                                                // then, so onmounted wouldn't re-fire).
                                                onmounted: move |evt| {
                                                    let d = evt.data();
                                                    title_input_el.set(Some(d.clone()));
                                                    spawn(async move { let _ = d.set_focus(true).await; });
                                                },
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
                                        value: if t.karaoke { "Highlight each word".to_string() } else if t.reveal { "One at a time".to_string() } else { "All at once".to_string() },
                                        options: vec!["All at once".to_string(), "One at a time".to_string(), "Highlight each word".to_string()],
                                        onchange: move |v: String| {
                                            push_undo("");
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.reveal = v == "One at a time";
                                                item.karaoke = v == "Highlight each word";
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    if t.karaoke {
                                        MorSelect {
                                            label: "Highlight colour".to_string(),
                                            value: t.karaoke_color.clone(),
                                            options: TITLE_COLORS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>(),
                                            onchange: move |v: String| {
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    item.karaoke_color = v;
                                                    item.pngs.clear();
                                                }
                                                rerender_title(k);
                                            },
                                        }
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "{t.segments().len()} card(s) — the whole line stays up while each word lights up in turn. Bevel is off in this mode."
                                        }
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
                                    // How solid the plate is — the caption's punch over
                                    // busy video. Only meaningful with a Box backdrop.
                                    if t.boxed {
                                        Slider {
                                            label: Some("Plate opacity"),
                                            min: 0.0,
                                            max: 1.0,
                                            step: 0.05,
                                            precision: 2,
                                            value: t.box_opacity,
                                            oninput: Some(EventHandler::new(move |v: f64| {
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    item.box_opacity = v;
                                                    item.pngs.clear();
                                                }
                                                rerender_title(k);
                                            })),
                                        }
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
                                        value: {
                                            // Show the named seat when we're on one; otherwise
                                            // a free-drag label so the dropdown isn't lying.
                                            if let Some(n) = nearest_title_pos(seat_y(&t)) {
                                                n.to_string()
                                            } else {
                                                "Custom".to_string()
                                            }
                                        },
                                        options: TITLE_POS.iter().map(|(n, _)| n.to_string())
                                            .chain(std::iter::once("Custom".to_string()))
                                            .collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if v == "Custom" {
                                                return; // free seat comes from the slider / drag
                                            }
                                            if let Some(item) = titles.write().get_mut(k) {
                                                set_seat_named(item, &v);
                                                item.pngs.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    Slider {
                                        label: Some("Vertical seat"),
                                        min: 0.02,
                                        max: 0.95,
                                        step: 0.01,
                                        precision: 2,
                                        value: seat_y(&t),
                                        oninput: Some(EventHandler::new(move |v: f64| {
                                            push_undo(&format!("tseat{k}"));
                                            if let Some(item) = titles.write().get_mut(k) {
                                                set_seat_y(item, v);
                                                item.pngs.clear();
                                            }
                                            rerender_title_soon(k);
                                        })),
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Drag the box on the preview to place the text — hold Shift to snap to Top / Middle / Lower third."
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
                                    // Visual style browser (iMovie Titles panel): pick
                                    // a look from tiles, not a blank name list.
                                    h4 { class: "mr-fx-cat", "Title styles" }
                                    {title_gallery(false, false)}
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
                                // Snapshot the T lane so the transcript can be
                                // edited without holding a read borrow across the
                                // rsx. `is_text` cards get an inline editable line;
                                // shapes just show their kind.
                                let text_items: Vec<(usize, bool, String, f64)> = titles
                                    .read()
                                    .iter()
                                    .enumerate()
                                    .map(|(k, t)| {
                                        let is_text = t.kind == "Text";
                                        let label = if is_text {
                                            t.text.replace('\n', " ")
                                        } else {
                                            t.kind.clone()
                                        };
                                        (k, is_text, label, t.at)
                                    })
                                    .collect();
                                let is_empty = text_items.is_empty();
                                rsx! {
                                    p { class: "mor-statusbar-muted",
                                        "Pick a title style below, or type over an existing card. Click a row's ✎ for the full knobs."
                                    }
                                    h4 { class: "mr-fx-cat", "Title styles" }
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Click a look to drop it at the playhead — same idea as iMovie's Titles browser."
                                    }
                                    {title_gallery(no_clips, true)}
                                    div { class: "mr-phase-actions",
                                        button { class: "mor-btn primary", onclick: move |_| add_title(()), "T Plain text" }
                                        button { class: "mor-btn", disabled: no_clips || transcribing(), onclick: move |_| auto_captions(()), "✎ Auto-caption from audio" }
                                    }
                                    if is_empty {
                                        p { class: "mor-statusbar-muted mr-text-empty",
                                            "No text on the reel yet — pick a style above."
                                        }
                                    } else {
                                        div { class: "mr-text-list",
                                            for (k, is_text, label, at) in text_items {
                                                div {
                                                    key: "{k}",
                                                    class: "mr-text-row",
                                                    button {
                                                        class: "mr-ctx-tag title mr-text-jump",
                                                        title: "Open this card's full style",
                                                        onclick: move |_| { selected.set(Some(Sel::Title(k))); seek_to(at); },
                                                        "✎"
                                                    }
                                                    if is_text {
                                                        input {
                                                            class: "mor-input mr-text-row-edit",
                                                            value: "{label}",
                                                            placeholder: "(empty)",
                                                            // Keystrokes must not reach the shortcut root.
                                                            onkeydown: move |evt| evt.stop_propagation(),
                                                            onfocusin: move |_| { seek_to(at); },
                                                            oninput: move |evt| {
                                                                let v = evt.value();
                                                                if let Some(item) = titles.write().get_mut(k) {
                                                                    item.text = v;
                                                                }
                                                                rerender_title_soon(k);
                                                            },
                                                        }
                                                    } else {
                                                        span { class: "mr-text-row-label", "{label}" }
                                                    }
                                                    span {
                                                        class: "mr-text-row-time",
                                                        onclick: move |_| seek_to(at),
                                                        "{fmt_t(at)}"
                                                    }
                                                    button {
                                                        class: "mr-text-del",
                                                        title: "Remove this card",
                                                        onclick: move |_| { selected.set(Some(Sel::Title(k))); delete_sel(()); },
                                                        "✕"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if active_phase() == Phase::Audio {
                                rsx! {
                                    p { class: "mor-statusbar-muted",
                                        "Add music under the picture, or record a voiceover at the playhead (V). Pick a bed on A1/A2 to trim and mix."
                                    }
                                    div { class: "mr-phase-actions",
                                        button { class: "mor-btn primary", onclick: move |_| add_audio(1), "♪ Add music (A1)" }
                                        button { class: "mor-btn", onclick: move |_| add_audio(2), "🎙 Import audio (A2)" }
                                        button {
                                            class: if vo_session().is_some() { "mor-btn primary" } else { "mor-btn" },
                                            disabled: exporting,
                                            title: "Record from the microphone onto A2 (V)",
                                            onclick: move |_| toggle_voiceover(()),
                                            if vo_session().is_some() { "■ Stop VO" } else { "● Record VO" }
                                        }
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

                        // Timeline shortcuts — irrelevant (and gated off) in the
                        // popped window, so only the main inspector shows them.
                        if is_main {
                            p { class: "mor-statusbar-muted mr-keys",
                                "Space play · V voiceover · Shift+D disable · Alt+S solo · Ctrl+L loop · F freeze · S split · Ctrl+E"
                            }
                        }
                    }
                    }
                }

                if is_main {
                div {
                    class: {
                        let soloing = clips().iter().any(|c| c.enabled && c.solo)
                            || overlays().iter().any(|o| o.enabled && o.solo)
                            || audios().iter().any(|a| a.enabled && a.solo);
                        let drop = drop_hover() == Some(Lane::V1);
                        match (soloing, drop) {
                            (true, true) => "mr-timeline soloing mr-drop",
                            (true, false) => "mr-timeline soloing",
                            (false, true) => "mr-timeline mr-drop",
                            (false, false) => "mr-timeline",
                        }
                    },
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
                        // Volume line / clip wave: drag up = louder. ~180px
                        // sweeps the full 0..2 range (iMovie "Adjust volume").
                        if let Some((target, grab_y, at_grab)) = vol_drag() {
                            let g = (at_grab - (p.y - grab_y) / 90.0).clamp(0.0, 2.0);
                            match target {
                                Sel::Aud(k) => {
                                    let mut au = audios.write();
                                    if let Some(a) = au.get_mut(k) {
                                        a.volume = g;
                                        if a.vol_end >= 0.0 {
                                            a.vol_end = g;
                                        }
                                    }
                                }
                                Sel::Main(i) => {
                                    if let Some(c) = clips.write().get_mut(i) {
                                        c.volume = g;
                                    }
                                }
                                _ => {}
                            }
                            return;
                        }
                        // Dual-edge resize: left keeps the far edge put (titles /
                        // cutaways) or trims the in-point (V1); right grows/shrinks
                        // the end. Values recompute from the grab snapshot so a
                        // long drag never accumulates rounding error.
                        if let Some((target, left, grab_x, at0, a0, b0, speed0, src_dur0)) = len_drag() {
                            let dt = (p.x - grab_x) / calc_scale();
                            match target {
                                Sel::Title(k) => {
                                    if let Some(t) = titles.write().get_mut(k) {
                                        let (at, dur) = title_edge_resize(at0, a0, left, dt);
                                        t.at = at;
                                        t.dur = dur;
                                    }
                                }
                                Sel::Over(j) => {
                                    if let Some(o) = overlays.write().get_mut(j) {
                                        let (at, inn, out) = media_edge_resize(
                                            at0, a0, b0, src_dur0, speed0, left, dt, true,
                                        );
                                        o.at = at;
                                        o.in_s = inn;
                                        o.out_s = out;
                                    }
                                }
                                Sel::Main(i) => {
                                    let old = spans();
                                    if let Some(c) = clips.write().get_mut(i) {
                                        let (_, inn, out) = media_edge_resize(
                                            0.0, a0, b0, src_dur0, speed0, left, dt, false,
                                        );
                                        c.in_s = inn;
                                        c.out_s = out;
                                    }
                                    ride(old, &|k| Some(start_of(k)));
                                }
                                Sel::Aud(_) => {}
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
                    // FCP-style Clip Appearance popover (zoom, film/wave modes, height).
                    if show_appear() {
                        div {
                            class: "mr-appear-pop",
                            onmousedown: move |evt| evt.stop_propagation(),
                            oncontextmenu: move |evt| evt.stop_propagation(),
                            div { class: "mr-appear-row",
                                span { class: "mr-appear-label", "🔍" }
                                input {
                                    r#type: "range",
                                    class: "mr-appear-zoom",
                                    min: "0.25",
                                    max: "6",
                                    step: "0.05",
                                    value: "{zoom}",
                                    title: "Zoom timeline",
                                    onkeydown: move |evt| evt.stop_propagation(),
                                    oninput: move |evt| {
                                        if let Ok(v) = evt.value().parse::<f64>() {
                                            zoom.set(v);
                                        }
                                    },
                                }
                                button {
                                    class: "mr-appear-icon-btn",
                                    title: "Zoom in (Ctrl+=)",
                                    onclick: move |_| zoom_by(1.25),
                                    "⊕"
                                }
                            }
                            div { class: "mr-appear-modes", title: "Clip appearance",
                                for (n, mode) in ClipAppear::ALL.into_iter().enumerate() {
                                    {
                                    let tip = format!("{} — Ctrl+Alt+{}", mode.label(), n + 1);
                                    let g = mode.glyph();
                                    rsx! {
                                    button {
                                        key: "{n}",
                                        class: if clip_appear() == mode {
                                            "mr-appear-mode on"
                                        } else {
                                            "mr-appear-mode"
                                        },
                                        title: "{tip}",
                                        onclick: move |_| clip_appear.set(mode),
                                        "{g}"
                                    }
                                    }
                                    }
                                }
                            }
                            div { class: "mr-appear-row",
                                span { class: "mr-appear-label", title: "Clip height", "↕" }
                                input {
                                    r#type: "range",
                                    class: "mr-appear-zoom",
                                    min: "0.5",
                                    max: "2",
                                    step: "0.05",
                                    value: "{clip_height}",
                                    title: "Clip height (Ctrl+Alt+↑/↓)",
                                    onkeydown: move |evt| evt.stop_propagation(),
                                    oninput: move |evt| {
                                        if let Ok(v) = evt.value().parse::<f64>() {
                                            clip_height.set(v.clamp(0.5, 2.0));
                                        }
                                    },
                                }
                            }
                            label { class: "mr-appear-check",
                                input {
                                    r#type: "checkbox",
                                    checked: show_clip_names(),
                                    onchange: move |evt| show_clip_names.set(evt.checked()),
                                }
                                "Show clip names"
                            }
                            button {
                                class: "mr-appear-close",
                                onclick: move |_| show_appear.set(false),
                                "Done"
                            }
                        }
                    }
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
                                    div { class: if show_titles() { "mr-lane mr-lane-t" } else { "mr-lane mr-lane-t mr-lane-off" },
                                        span {
                                            class: if show_titles() { "mr-lane-tag title mr-lane-toggle" } else { "mr-lane-tag title mr-lane-toggle off" },
                                            title: if show_titles() { "Titles shown — click to hide in the monitor" } else { "Titles hidden in the monitor — click to show" },
                                            onclick: move |evt| {
                                                evt.stop_propagation();
                                                let on = !show_titles();
                                                show_titles.set(on);
                                                seek_to(playhead());
                                            },
                                            if show_titles() { "T" } else { "T⃠" }
                                        }
                                        for (k, t) in titles().into_iter().enumerate() {
                                            div {
                                                key: "title-{k}",
                                                class: item_class(
                                                    "mr-lane-item title",
                                                    selected() == Some(Sel::Title(k)),
                                                    marked().contains(&Sel::Title(k)),
                                                    !t.enabled,
                                                    false,
                                                ),
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
                                                // Dual-edge grips: left keeps the right edge fixed;
                                                // right only changes duration (Final Cut / iMovie style).
                                                div {
                                                    class: "mr-len-grip in",
                                                    title: "Drag to change when the title starts",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        push_undo(&format!("tlen{k}"));
                                                        let (at, d) = titles.read().get(k).map_or((0.0, 3.0), |t| (t.at, t.dur));
                                                        len_drag.set(Some((Sel::Title(k), true, evt.client_coordinates().x, at, d, 0.0, 1.0, 0.0)));
                                                    },
                                                }
                                                div {
                                                    class: "mr-len-grip out",
                                                    title: "Drag to change how long the title shows",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        push_undo(&format!("tlen{k}"));
                                                        let (at, d) = titles.read().get(k).map_or((0.0, 3.0), |t| (t.at, t.dur));
                                                        len_drag.set(Some((Sel::Title(k), false, evt.client_coordinates().x, at, d, 0.0, 1.0, 0.0)));
                                                    },
                                                }
                                                span { class: "mr-clip-dur", "{fmt_clip_dur(t.dur)}" }
                                                if t.group != 0 {
                                                    span { class: "mr-group-dot", style: "background: hsl({(t.group * 67) % 360}, 70%, 60%)" }
                                                }
                                                if t.kind == "Text" { "𝐓 {t.text}" } else { "◧ {t.kind}" }
                                            }
                                        }
                                    }
                                    div {
                                        class: if drop_hover() == Some(Lane::V2) { "mr-lane mr-lane-v mr-drop" } else if show_overlays() { "mr-lane mr-lane-v" } else { "mr-lane mr-lane-v mr-lane-off" },
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
                                        span {
                                            class: if show_overlays() { "mr-lane-tag mr-lane-toggle" } else { "mr-lane-tag mr-lane-toggle off" },
                                            title: if show_overlays() { "Cutaways shown — click to hide in the monitor" } else { "Cutaways hidden in the monitor — click to show" },
                                            onclick: move |evt| {
                                                evt.stop_propagation();
                                                let on = !show_overlays();
                                                show_overlays.set(on);
                                                seek_to(playhead());
                                            },
                                            if show_overlays() { "V2" } else { "V2⃠" }
                                        }
                                        for (j, o) in overlays().into_iter().enumerate() {
                                            div {
                                                key: "{j}-{o.path}",
                                                class: item_class(
                                                    "mr-lane-item",
                                                    selected() == Some(Sel::Over(j)),
                                                    marked().contains(&Sel::Over(j)),
                                                    !o.enabled,
                                                    o.solo,
                                                ),
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
                                                // Dual-edge trim: left slides `at` + in-point so the
                                                // right edge stays put; right only moves the out-point.
                                                div {
                                                    class: "mr-len-grip in",
                                                    title: "Drag to trim the start of this cutaway",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        push_undo(&format!("olen{j}"));
                                                        let snap = overlays.read().get(j).map(|o| {
                                                            (o.at, o.in_s, o.out_s, o.speed.max(0.01), o.duration)
                                                        }).unwrap_or((0.0, 0.0, 1.0, 1.0, 1.0));
                                                        len_drag.set(Some((Sel::Over(j), true, evt.client_coordinates().x, snap.0, snap.1, snap.2, snap.3, snap.4)));
                                                    },
                                                }
                                                div {
                                                    class: "mr-len-grip out",
                                                    title: "Drag to trim the end of this cutaway",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        push_undo(&format!("olen{j}"));
                                                        let snap = overlays.read().get(j).map(|o| {
                                                            (o.at, o.in_s, o.out_s, o.speed.max(0.01), o.duration)
                                                        }).unwrap_or((0.0, 0.0, 1.0, 1.0, 1.0));
                                                        len_drag.set(Some((Sel::Over(j), false, evt.client_coordinates().x, snap.0, snap.1, snap.2, snap.3, snap.4)));
                                                    },
                                                }
                                                span { class: "mr-clip-dur", "{fmt_clip_dur(o.trimmed())}" }
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
                                            // Filmstrip + under-clip wave (iMovie/FCP): layout
                                            // follows Clip Appearance (film/wave sizes + height).
                                            {
                                            let appear = clip_appear();
                                            let (film_h, wave_h) = appear.heights(clip_height());
                                            let show_film = appear.show_film();
                                            let show_wave = appear.show_wave() && c.has_audio;
                                            let names = show_clip_names();
                                            let film_style = {
                                                let mut s = format!("height:{film_h:.0}px;");
                                                if !c.thumb.is_empty() {
                                                    s.push_str(&format!(
                                                        "background-image:url({});background-size:auto 100%;background-repeat:repeat-x;background-position:left center;",
                                                        c.thumb
                                                    ));
                                                }
                                                s
                                            };
                                            let wave_style = {
                                                let mut s = format!("height:{wave_h:.0}px;");
                                                if !c.wave.is_empty() {
                                                    s.push_str(&wave_css(
                                                        &c.wave, c.duration, c.in_s, scale, c.speed,
                                                    ));
                                                }
                                                s
                                            };
                                            let vol_pct = (c.volume / 2.0 * 100.0).clamp(0.0, 100.0);
                                            let film_class = if c.thumb.is_empty() {
                                                if appear.labels_only() {
                                                    "mr-clip-film mr-clip-labels"
                                                } else {
                                                    "mr-clip-film mr-thumb-missing"
                                                }
                                            } else {
                                                "mr-clip-film"
                                            };
                                            let clip_class = item_class(
                                                "mr-clip",
                                                selected() == Some(Sel::Main(i)),
                                                marked().contains(&Sel::Main(i)),
                                                !c.enabled,
                                                c.solo,
                                            );
                                            let clip_w = ext[i] * scale;
                                            let lane_h = film_h
                                                + if show_wave { wave_h } else { 0.0 };
                                            let dur_label = fmt_clip_dur(c.trimmed());
                                            let has_group = c.group != 0;
                                            let group_hue = (c.group * 67) % 360;
                                            let has_xtrans = fade_in(&clips.read(), i) > 0.0;
                                            let xtrans_title = c.transition.clone();
                                            let has_fx = c.effect != "None";
                                            let clip_name = c.name.clone();
                                            let clip_path = c.path.clone();
                                            rsx! {
                                            div {
                                                key: "{i}-{clip_path}",
                                                class: "{clip_class}",
                                                style: "width: {clip_w}px; min-height: {lane_h.max(28.0):.0}px",
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
                                                if has_group {
                                                    span { class: "mr-group-dot", style: "background: hsl({group_hue}, 70%, 60%)" }
                                                }
                                                if has_xtrans {
                                                    span {
                                                        class: "mr-xtrans",
                                                        title: "{xtrans_title}",
                                                        "><"
                                                    }
                                                }
                                                span { class: "mr-clip-dur", "{dur_label}" }
                                                // Dual-edge trim grips — left changes the in-point,
                                                // right the out-point; magnetic items ride along.
                                                div {
                                                    class: "mr-len-grip in",
                                                    title: "Drag to trim the start of this clip",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        selected.set(Some(Sel::Main(i)));
                                                        push_undo(&format!("clen{i}"));
                                                        let snap = clips.read().get(i).map(|c| {
                                                            (c.in_s, c.out_s, c.speed.max(0.01), c.duration)
                                                        }).unwrap_or((0.0, 1.0, 1.0, 1.0));
                                                        len_drag.set(Some((Sel::Main(i), true, evt.client_coordinates().x, 0.0, snap.0, snap.1, snap.2, snap.3)));
                                                    },
                                                }
                                                div {
                                                    class: "mr-len-grip out",
                                                    title: "Drag to trim the end of this clip",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        selected.set(Some(Sel::Main(i)));
                                                        push_undo(&format!("clen{i}"));
                                                        let snap = clips.read().get(i).map(|c| {
                                                            (c.in_s, c.out_s, c.speed.max(0.01), c.duration)
                                                        }).unwrap_or((0.0, 1.0, 1.0, 1.0));
                                                        len_drag.set(Some((Sel::Main(i), false, evt.client_coordinates().x, 0.0, snap.0, snap.1, snap.2, snap.3)));
                                                    },
                                                }
                                                if show_film {
                                                    div {
                                                        class: "{film_class}",
                                                        style: "{film_style}",
                                                        if names {
                                                            span { class: "mr-clip-name",
                                                                if has_fx { "✨ " }
                                                                "{clip_name}"
                                                            }
                                                        }
                                                    }
                                                } else if names && !show_wave {
                                                    // Wave-only with names: overlay on the wave strip.
                                                    span { class: "mr-clip-name mr-clip-name-solo",
                                                        if has_fx { "✨ " }
                                                        "{clip_name}"
                                                    }
                                                }
                                                if show_wave {
                                                    div {
                                                        class: "mr-clip-wave",
                                                        title: "Adjust volume — drag up or down",
                                                        style: "{wave_style}",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            selected.set(Some(Sel::Main(i)));
                                                            push_undo(&format!("cvol{i}"));
                                                            let g = clips.read().get(i).map_or(1.0, |c| c.volume);
                                                            vol_drag.set(Some((Sel::Main(i), evt.client_coordinates().y, g)));
                                                        },
                                                        // Level hairline so the current gain is
                                                        // visible without opening the inspector.
                                                        div {
                                                            class: "mr-vol-line",
                                                            style: "bottom: {vol_pct}%",
                                                        }
                                                        if names && !show_film {
                                                            span { class: "mr-clip-name",
                                                                if has_fx { "✨ " }
                                                                "{clip_name}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            }
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
                                                    class: item_class(
                                                        "mr-lane-item audio",
                                                        selected() == Some(Sel::Aud(k)),
                                                        marked().contains(&Sel::Aud(k)),
                                                        !a.enabled,
                                                        a.solo,
                                                    ),
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
                                                        title: "Adjust volume — drag up or down",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("avol{k}"));
                                                            let g = audios.read().get(k).map_or(1.0, |a| a.volume);
                                                            vol_drag.set(Some((Sel::Aud(k), evt.client_coordinates().y, g)));
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
                } // if is_main (timeline)
                // Workflow spine: the reel-building phases left→right, the phase
                // you're in lit up, a ✓ on phases that already have content. Each
                // button is the primary action for its phase, not a menu clone.
                // Main window only — a popped inspector is chrome-free.
                if is_main {
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
                        class: if active_phase() == Phase::Effects { "mr-wf active" } else { "mr-wf" },
                        title: "Chroma key and image/particle effects for the current clip or overlay",
                        onclick: move |_| {
                            if selected().is_none() {
                                if let Some((i, _)) = locate(&clips.read(), playhead()) { selected.set(Some(Sel::Main(i))); }
                            }
                            active_phase.set(Phase::Effects);
                        },
                        span { class: "mr-wf-icon", "◧" }
                        span { class: "mr-wf-label", "FX" }
                        if clips.read().iter().any(|c| is_keyer(&c.effect))
                            || overlays.read().iter().any(|o| is_keyer(&o.effect) || !o.blend.is_empty()) {
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
                } // if is_main (phase bar)
            }
        }

        if let Some((cx, cy, target)) = ctx_menu().filter(|_| is_main) {
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
                                    label: "Add freeze frame".to_string(),
                                    shortcut: Some("F".to_string()),
                                    on_action: move |_| add_freeze_frame(()),
                                }
                                CtxItem {
                                    label: "Instant replay".to_string(),
                                    shortcut: Some("Ctrl+R".to_string()),
                                    on_action: move |_| instant_replay(()),
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
                                if i > 0 {
                                    CtxItem {
                                        label: "Add cross dissolve".to_string(),
                                        shortcut: Some("Ctrl+D".to_string()),
                                        on_action: move |_| add_cross_dissolve(()),
                                    }
                                }
                                CtxItem {
                                    label: "Join with neighbour".to_string(),
                                    shortcut: Some("Ctrl+J".to_string()),
                                    disabled: {
                                        let cl = clips.read();
                                        let left = i > 0 && can_join_clips(&cl[i - 1], &cl[i]);
                                        let right = i + 1 < cl.len() && can_join_clips(&cl[i], &cl[i + 1]);
                                        !left && !right
                                    },
                                    on_action: move |_| join_clips(()),
                                }
                                CtxItem {
                                    label: "Effects palette…".to_string(),
                                    on_action: move |_| {
                                        insp_open.set(true);
                                        show_effects.set(true);
                                    },
                                }
                                CtxItem {
                                    label: if clips.read().get(i).is_some_and(|c| c.volume <= 0.001) {
                                        "Unmute audio".to_string()
                                    } else {
                                        "Mute audio".to_string()
                                    },
                                    shortcut: Some("Ctrl+Shift+M".to_string()),
                                    disabled: !clips.read().get(i).is_some_and(|c| c.has_audio),
                                    on_action: move |_| mute_sel(()),
                                }
                                CtxItem {
                                    label: if clips.read().get(i).is_some_and(|c| c.enabled) {
                                        "Disable".to_string()
                                    } else {
                                        "Enable".to_string()
                                    },
                                    shortcut: Some("Shift+D".to_string()),
                                    on_action: move |_| toggle_disable_sel(()),
                                }
                                CtxItem {
                                    label: if clips.read().get(i).is_some_and(|c| c.solo) {
                                        "Unsolo".to_string()
                                    } else {
                                        "Solo".to_string()
                                    },
                                    shortcut: Some("Alt+S".to_string()),
                                    on_action: move |_| toggle_solo_sel(()),
                                }
                                CtxItem {
                                    label: "Detach audio to A1".to_string(),
                                    shortcut: Some("Ctrl+U".to_string()),
                                    disabled: !clips.read().get(i).is_some_and(|c| c.has_audio),
                                    on_action: move |_| detach_audio(()),
                                }
                                MenuSeparator {}
                                CtxItem {
                                    label: "Copy".to_string(),
                                    shortcut: Some("Ctrl+C".to_string()),
                                    on_action: move |_| copy_sel(()),
                                }
                                CtxItem {
                                    label: "Paste at playhead".to_string(),
                                    shortcut: Some("Ctrl+V".to_string()),
                                    disabled: clipboard().is_none(),
                                    on_action: move |_| paste_sel(()),
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
                                    label: "Copy".to_string(),
                                    shortcut: Some("Ctrl+C".to_string()),
                                    on_action: move |_| copy_sel(()),
                                }
                                CtxItem {
                                    label: "Paste at playhead".to_string(),
                                    shortcut: Some("Ctrl+V".to_string()),
                                    disabled: clipboard().is_none(),
                                    on_action: move |_| paste_sel(()),
                                }
                                CtxItem {
                                    label: "Effects palette…".to_string(),
                                    on_action: move |_| {
                                        insp_open.set(true);
                                        show_effects.set(true);
                                    },
                                }
                                CtxItem {
                                    label: if overlays.read().get(j).is_some_and(|o| o.enabled) {
                                        "Disable".to_string()
                                    } else {
                                        "Enable".to_string()
                                    },
                                    shortcut: Some("Shift+D".to_string()),
                                    on_action: move |_| toggle_disable_sel(()),
                                }
                                CtxItem {
                                    label: if overlays.read().get(j).is_some_and(|o| o.solo) {
                                        "Unsolo".to_string()
                                    } else {
                                        "Solo".to_string()
                                    },
                                    shortcut: Some("Alt+S".to_string()),
                                    on_action: move |_| toggle_solo_sel(()),
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
                                    label: "Copy".to_string(),
                                    shortcut: Some("Ctrl+C".to_string()),
                                    on_action: move |_| copy_sel(()),
                                }
                                CtxItem {
                                    label: "Paste at playhead".to_string(),
                                    shortcut: Some("Ctrl+V".to_string()),
                                    disabled: clipboard().is_none(),
                                    on_action: move |_| paste_sel(()),
                                }
                                CtxItem {
                                    label: if audios.read().get(k).is_some_and(|a| a.volume <= 0.001) {
                                        "Unmute".to_string()
                                    } else {
                                        "Mute".to_string()
                                    },
                                    shortcut: Some("Ctrl+Shift+M".to_string()),
                                    on_action: move |_| mute_sel(()),
                                }
                                CtxItem {
                                    label: if audios.read().get(k).is_some_and(|a| a.enabled) {
                                        "Disable".to_string()
                                    } else {
                                        "Enable".to_string()
                                    },
                                    shortcut: Some("Shift+D".to_string()),
                                    on_action: move |_| toggle_disable_sel(()),
                                }
                                CtxItem {
                                    label: if audios.read().get(k).is_some_and(|a| a.solo) {
                                        "Unsolo".to_string()
                                    } else {
                                        "Solo".to_string()
                                    },
                                    shortcut: Some("Alt+S".to_string()),
                                    on_action: move |_| toggle_solo_sel(()),
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
                                CtxItem {
                                    label: "Copy".to_string(),
                                    shortcut: Some("Ctrl+C".to_string()),
                                    on_action: move |_| copy_sel(()),
                                }
                                CtxItem {
                                    label: "Paste at playhead".to_string(),
                                    shortcut: Some("Ctrl+V".to_string()),
                                    disabled: clipboard().is_none(),
                                    on_action: move |_| paste_sel(()),
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

        // Dialogs live on the main window; a popped inspector's buttons set the
        // shared open-flags, so the modal surfaces there.
        if is_main {
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
                        span { class: "mor-statusbar-muted", "Import a voice track under the picture" }
                    }
                    button {
                        class: "mr-add-card",
                        disabled: exporting,
                        onclick: move |_| {
                            show_add.set(false);
                            toggle_voiceover(());
                        },
                        span { class: "mr-add-tag audio", "●" }
                        strong { if vo_session().is_some() { "Stop recording" } else { "Record voiceover" } }
                        span { class: "mor-statusbar-muted", "Mic → A2 at the playhead (V)" }
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
                Phase::Effects => "Key & image effects".to_string(),
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
                        // Prefer the selection; fall back to the clip under the
                        // playhead so the gallery works without a prior click.
                        let sel = match selected() {
                            Some(Sel::Main(i)) if i > 0 && i < clips.read().len() => Some(i),
                            _ => locate(&clips.read(), playhead())
                                .map(|(i, _)| i)
                                .filter(|&i| i > 0 && i < clips.read().len()),
                        };
                        match sel {
                            Some(i) => {
                                let current = clips.read()[i].transition.clone();
                                let target_name = clips.read()[i].name.clone();
                                rsx! {
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "How {target_name} eases in from the clip before it. A crossfade overlaps them, so the reel gets a little shorter. Tip: Ctrl+D drops a cross dissolve; F freezes the frame under the playhead."
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
                                                        if label != "None" && clips.read()[i].trans_dur < 0.1 {
                                                            clips.write()[i].trans_dur = 0.5;
                                                        }
                                                        ride(old, &|k| Some(start_of(k)));
                                                        selected.set(Some(Sel::Main(i)));
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
                                    "Park the playhead on a V1 clip after the first — a transition joins it to the clip before it. Or press Ctrl+D for a cross dissolve."
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
                    // The Effects workspace: keyers (chroma/colour/luma) plus a blend
                    // mode for image/particle layers — moranima's compositing, ported.
                    Phase::Effects => {
                        let sel = match selected() {
                            Some(Sel::Main(i)) => clips.read().get(i).map(|c| (c.effect.clone(), c.effect_amount, None::<usize>, String::new())),
                            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| (o.effect.clone(), o.effect_amount, Some(j), o.blend.clone())),
                            _ => None,
                        };
                        match sel {
                            Some((current, amount, over_idx, blend)) => rsx! {
                                div { class: "mr-fx-dialog",
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Key a colour to transparency so V1 shows through — drop a green-screen clip or a particle/light-leak plate on V2, then key it. Blend modes screen a glow over V1 the way moranima composites its particles."
                                    }
                                    if effect_filter_amt(&current, 0.5) != effect_filter(&current) {
                                        Slider {
                                            label: Some("Key tolerance"),
                                            min: 0.0, max: 1.0, step: 0.05, precision: 2,
                                            value: amount,
                                            oninput: Some(EventHandler::new(move |v: f64| set_effect_amount(v))),
                                        }
                                    }
                                    h4 { class: "mr-fx-cat", "Key" }
                                    div { class: "mr-fx-grid",
                                        // "None" clears any key; then every keyer.
                                        for (name, is_current) in std::iter::once(("None".to_string(), current == "None"))
                                            .chain(all_effects().into_iter().filter(|(c, _, _)| c == "Key").map(|(_, n, _)| { let cur = current == n; (n, cur) })) {
                                            button {
                                                key: "{name}",
                                                class: if is_current { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                onclick: {
                                                    let click = name.clone();
                                                    move |_| apply_effect(click.clone())
                                                },
                                                div { class: "mr-fx-ph" }
                                                span { "{name}" }
                                            }
                                        }
                                    }
                                    if let Some(j) = over_idx {
                                        h4 { class: "mr-fx-cat", "Blend (V2 over V1)" }
                                        div { class: "mr-fx-grid",
                                            for (label, mode) in BLEND_MODES.iter().copied() {
                                                button {
                                                    key: "{label}",
                                                    class: if blend == mode { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                    onclick: move |_| {
                                                        push_undo("blend");
                                                        if j < overlays.read().len() { overlays.write()[j].blend = mode.to_string(); }
                                                        seek_to(playhead());
                                                    },
                                                    div { class: "mr-fx-ph" }
                                                    span { "{label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            None => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Keying and blends apply to video — select a V1 clip or V2 overlay on the timeline, then open this palette again."
                                }
                            },
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
                                // Built-ins + hub bundle effects, grouped by category in order. Each
                                // entry carries a display name, a click copy (moved into the handler),
                                // whether it's the current pick, and its thumbnail — precomputed so the
                                // rsx loop body stays pure nodes. Keyers live in the Effects workspace,
                                // not here, so the Style look grid skips them.
                                type FxItem = (String, String, bool, Option<String>);
                                let mut fx_groups: Vec<(String, Vec<FxItem>)> = Vec::new();
                                for (c, n, _) in all_effects().into_iter().filter(|(c, _, _)| c != "Key") {
                                    let item = (n.clone(), n.clone(), current == n.as_str(), thumbs.get(&n).cloned());
                                    if let Some(g) = fx_groups.iter_mut().find(|(gc, _)| gc == &c) {
                                        g.1.push(item);
                                    } else {
                                        fx_groups.push((c, vec![item]));
                                    }
                                }
                                rsx! {
                                    div { class: "mr-fx-dialog",
                                        p { class: "mor-statusbar-muted mr-export-blurb",
                                            "Looks apply to the selected V1 clip or V2 overlay. Motion looks animate as you scrub."
                                        }
                                        // Only looks with a blend formula (their half-strength chain
                                        // differs from full) get a live slider — the rest are all-or-nothing,
                                        // so a slider there would be a dead knob.
                                        if effect_filter_amt(&current, 0.5) != effect_filter(&current) {
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
                                        for (cat, items) in fx_groups {
                                            h4 { class: "mr-fx-cat", "{cat}" }
                                            div { class: "mr-fx-grid",
                                                for (name, click, is_current, thumb) in items {
                                                    button {
                                                        key: "{name}",
                                                        class: if is_current { "mr-fx-tile active" } else { "mr-fx-tile" },
                                                        onclick: move |_| apply_effect(click.clone()),
                                                        if let Some(uri) = thumb {
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
            open: show_hub,
            title: "Plugin Hub".to_string(),
            div { class: "mr-hub",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Fetch from GitHub clones the plugin-hub repo for you (and pulls updates on repeat) — or "
                    "point MorReel at your own checkout. Installing an 'mcp' plugin writes an entry to "
                    "mcp-servers.json for Claude Code; an 'agent' is set up in your own Claude Code; a 'bundle' "
                    "adds effect looks here. Nothing runs code without you."
                }
                div { class: "mr-toolbar",
                    button {
                        class: "mor-btn primary",
                        onclick: move |_| {
                            spawn(async move {
                                status.set("Fetching plugin hub from GitHub…".to_string());
                                match hub::fetch_hub().await {
                                    Ok(dir) => {
                                        let ms = hub::load_manifests(&dir);
                                        set_hub_effects(hub::active_bundle_effects(&ms, &hub::InstallState::load()));
                                        hub_gen += 1;
                                        status.set(format!("Plugin hub ready — {} plugins", ms.len()));
                                    }
                                    Err(e) => status.set(format!("Fetch hub: {e}")),
                                }
                            });
                        },
                        "Fetch from GitHub"
                    }
                    button {
                        class: "mor-btn",
                        onclick: move |_| {
                            spawn(async move {
                                if let Some(h) = rfd::AsyncFileDialog::new().pick_folder().await {
                                    let dir = h.path().to_path_buf();
                                    match hub::set_hub_dir(&dir) {
                                        Ok(_) => {
                                            let ms = hub::load_manifests(&dir);
                                            set_hub_effects(hub::active_bundle_effects(&ms, &hub::InstallState::load()));
                                            hub_gen += 1;
                                            status.set(format!("Hub folder: {}", dir.display()));
                                        }
                                        Err(e) => status.set(format!("Hub folder: {e}")),
                                    }
                                }
                            });
                        },
                        "Choose hub folder…"
                    }
                    if let Some(dir) = hub::hub_dir() {
                        span { class: "mor-statusbar-muted", "{dir.display()}" }
                    }
                }
                if !hub_configured {
                    p { class: "mor-statusbar-muted",
                        "No hub yet. Hit Fetch from GitHub to clone MoribundMurdoch/mor-reel-studio-plugin-hub, or choose your own checkout."
                    }
                } else if hub_rows.is_empty() {
                    p { class: "mor-statusbar-muted", "No plugins found in the hub's plugins/ folder." }
                }
                for (m, installed, enabled) in hub_rows {
                    HubRow {
                        key: "{m.id}",
                        manifest: m,
                        installed,
                        enabled,
                        on_action: move |p| handle_hub(p),
                    }
                }
                div { class: "mr-toolbar",
                    button { class: "mor-btn", onclick: move |_| show_hub.set(false), "Done" }
                }
            }
        }
        Modal {
            open: show_shortcuts,
            title: "Keyboard shortcuts".to_string(),
            table { class: "mr-shortcut-table",
                for (keys, what) in [
                    ("Space", "Play / pause (proxy video + audio mix)"),
                    ("Ctrl+Home", "Play from beginning"),
                    ("Ctrl+L", "Toggle loop playback"),
                    ("V", "Record / stop voiceover onto A2"),
                    ("Ctrl+P", "Full preview with audio in mpv/ffplay"),
                    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                    ("Ctrl+C / Ctrl+V", "Copy / paste timeline item at the playhead"),
                    ("I / O", "Set in / out point at playhead"),
                    ("S", "Split at playhead"),
                    ("F", "Add freeze frame at the playhead"),
                    ("Ctrl+R", "Instant replay (last 1.5s at half speed)"),
                    ("Ctrl+D", "Add cross dissolve into the clip under the playhead"),
                    ("Ctrl+J", "Join adjacent same-source clips"),
                    ("Ctrl+Shift+M", "Mute / unmute selected clip or bed"),
                    ("Edit › Auto-cut silence…", "Remove quiet stretches by volume"),
                    ("Delete / Backspace", "Ripple delete selection"),
                    ("← / →", "Nudge playhead 0.1s (Shift = 1s)"),
                    (", / .", "Step playhead one frame (1/30 s)"),
                    ("↑ / ↓", "Jump to previous / next edit point"),
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
        } // if is_main (modals)
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
.mr-preview-col { display: flex; flex-direction: column; gap: 10px; align-items: center; min-height: 0; padding-top: 4px; flex: 0 0 auto; width: clamp(180px, 34vw, 560px); }
/* The phone fills whatever height is left after the scrub controls; a definite
   column width keeps the height-driven phone from overflowing (the old WebKit
   intrinsic-width bug). 9:16 is tall, so height is the binding constraint. */
.mr-stage { flex: 1; min-height: 0; width: 100%; display: flex; align-items: center; justify-content: center; }

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
  height: 100%; width: auto; max-width: 100%; min-width: 0;
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

.mr-scrub { width: 100%; flex: none; }
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
/* Popped-out inspector: it IS the window — fill it, drop the card chrome, and
   breathe a little wider than the docked panel. */
.mr-inspector.mr-inspector-solo {
  flex: 1; min-width: 0; width: 100%; height: 100%;
  border: none; border-radius: 0; box-shadow: none;
  padding: 0 18px 18px; gap: 14px;
  background:
    radial-gradient(120% 42% at 50% 0%, color-mix(in srgb, var(--mor-accent) 7%, transparent), transparent 62%),
    linear-gradient(180deg, color-mix(in srgb, var(--mor-panel) 96%, white), var(--mor-panel));
}
/* Window identity: names what you're editing, in that element's colour (--kind).
   The signature — a top accent rail + a glowing kind badge — is the one bold
   mark; every control below it stays the app's quiet violet/glass. */
.mr-solo-head {
  position: sticky; top: 0; z-index: 6;
  margin: 0 -18px 4px; padding: 18px 20px 15px;
  border-bottom: 1px solid color-mix(in srgb, var(--kind) 26%, var(--mor-border));
  background: linear-gradient(180deg,
    color-mix(in srgb, var(--kind) 14%, var(--mor-panel)),
    color-mix(in srgb, var(--kind) 4%, var(--mor-panel)));
  box-shadow: inset 0 3px 0 var(--kind);
}
.mr-solo-eyebrow {
  display: block; font-size: 10px; font-weight: 700;
  letter-spacing: 0.2em; text-transform: uppercase;
  color: color-mix(in srgb, var(--kind) 62%, var(--mor-text-muted));
}
.mr-solo-title-row { display: flex; align-items: center; gap: 13px; margin-top: 9px; }
.mr-solo-badge {
  flex: none; display: grid; place-items: center;
  width: 36px; height: 36px; border-radius: 10px;
  font-size: 18px; font-weight: 700; color: var(--kind);
  background: color-mix(in srgb, var(--kind) 15%, transparent);
  border: 1px solid color-mix(in srgb, var(--kind) 42%, transparent);
  box-shadow: 0 0 18px color-mix(in srgb, var(--kind) 20%, transparent);
}
.mr-solo-title {
  font-size: 27px; font-weight: 700; letter-spacing: -0.015em; line-height: 1;
  color: var(--mor-text);
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
  position: relative;
  /* Both bars always shown: horizontal is the time axis, vertical spans the
     lane stack — keep them present so the scrollable extent is always obvious. */
  display: flex; overflow-x: scroll; overflow-y: scroll; padding: 12px 10px 8px;
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

/* Phase-driven inspector: stacked action buttons for Add/Export. */
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
.mr-text-row { cursor: default; }
.mr-text-row:hover {
  border-color: var(--mor-accent);
  box-shadow: 0 2px 10px rgba(0, 0, 0, 0.3), 0 0 12px color-mix(in srgb, var(--mor-accent) 22%, transparent);
}
.mr-text-row-label {
  flex: 1; min-width: 0;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px;
}
/* Inline transcript editor: the words are editable in place. */
.mr-text-row-edit {
  flex: 1; min-width: 0; font-size: 12px; padding: 5px 8px; margin: 0;
}
.mr-text-jump {
  flex: none; cursor: pointer; border: none; background: none;
  padding: 0 2px; font-size: 13px; line-height: 1;
}
.mr-text-del {
  flex: none; cursor: pointer; border: none; background: none;
  color: var(--mor-text-muted); font-size: 13px; line-height: 1; padding: 0 2px;
}
.mr-text-del:hover { color: var(--mor-danger, #e5484d); }
.mr-text-row-time {
  flex: none; font-size: 11px; color: var(--mor-text-muted); cursor: pointer;
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
/* An eye-toggle tag opts back into clicks (the base tag is inert) and dims when
   its lane is hidden from the monitor. */
.mr-lane-toggle { pointer-events: auto; cursor: pointer; transition: opacity 0.15s ease, filter 0.15s ease; }
.mr-lane-toggle:hover { filter: brightness(1.12); }
.mr-lane-tag.off { opacity: 0.4; filter: grayscale(0.7); }
.mr-lane-off .mr-lane-item { opacity: 0.32; filter: grayscale(0.5); }
.mr-lane-a1 .mr-lane-tag { background: linear-gradient(180deg, #5ee8dc, var(--mor-success)); }
/* Taller audio lanes so the waveform envelope has room to read and is an easier
   drag/trim target. */
.mr-lane-a1 {
  height: 76px;
  /* Empty bed reads as a drop target, like iMovie's music track. */
  border: 1px dashed color-mix(in srgb, var(--mor-success) 28%, transparent);
  border-radius: 6px;
  background: color-mix(in srgb, #0e1418 70%, transparent);
}
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
/* Dual-edge resize grips — left + right, same hit size as audio fade grips.
   Titles tint gold; cutaways and V1 clips pick up the lane colour via parent. */
.mr-len-grip {
  position: absolute; top: 0; bottom: 0; width: 9px; z-index: 4;
  cursor: ew-resize; opacity: 0; transition: opacity 0.15s ease;
  background: color-mix(in srgb, var(--mor-title, gold) 75%, white);
  box-shadow: 0 0 6px color-mix(in srgb, var(--mor-title, gold) 60%, transparent);
}
.mr-len-grip.in { left: 0; border-radius: 5px 0 0 5px; }
.mr-len-grip.out { right: 0; border-radius: 0 5px 5px 0; }
.mr-lane-item:hover .mr-len-grip,
.mr-clip:hover .mr-len-grip { opacity: 0.55; }
.mr-lane-item.selected .mr-len-grip,
.mr-clip.selected .mr-len-grip { opacity: 0.7; }
.mr-len-grip:hover { opacity: 0.95 !important; }
.mr-clip .mr-len-grip,
.mr-lane-item:not(.title) .mr-len-grip {
  background: color-mix(in srgb, var(--mor-accent, #6af) 75%, white);
  box-shadow: 0 0 6px color-mix(in srgb, var(--mor-accent, #6af) 60%, transparent);
}
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
/* FCP-style Disable: dimmed on the timeline, still occupies its span. */
.mr-clip.disabled, .mr-lane-item.disabled {
  opacity: 0.38;
  filter: grayscale(0.9);
}
.mr-clip.disabled.selected {
  border-color: #8a8690;
  box-shadow: 0 0 0 1px color-mix(in srgb, #8a8690 40%, transparent);
}
/* FCP-style Solo: yellow outline on soloed items; non-soloed go B&W when any solo is on. */
.mr-clip.solo, .mr-lane-item.solo {
  outline: 2px solid #f0c419;
  outline-offset: 0;
  z-index: 3;
}
.mr-timeline.soloing .mr-clip:not(.solo):not(.disabled),
.mr-timeline.soloing .mr-lane-item:not(.solo):not(.disabled) {
  filter: grayscale(1);
  opacity: 0.72;
}
.mr-group-dot {
  position: absolute; right: 5px; top: 6px; z-index: 3;
  width: 7px; height: 7px; border-radius: 50%;
  box-shadow: 0 0 4px rgba(0, 0, 0, 0.7), 0 0 6px color-mix(in srgb, currentColor 40%, transparent);
  pointer-events: none;
}

/* V1 story lane: filmstrip clips (iMovie) on a quiet bed. */
.mr-clips {
  position: relative; display: flex; margin-bottom: 6px;
  min-height: 40px;
  align-items: stretch;
  background: color-mix(in srgb, #1a1a22 80%, transparent);
  border-radius: 6px;
  box-shadow: inset 0 1px 3px rgba(0, 0, 0, 0.28);
}
.mr-clip {
  position: relative; flex: none; box-sizing: border-box; overflow: hidden;
  cursor: grab; border: 2px solid transparent; border-radius: 5px;
  /* No padding — the film fills edge-to-edge like iMovie. */
  background: #0c0c10;
  display: flex; flex-direction: column;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.35);
  transition: border-color 0.14s ease, box-shadow 0.18s ease, filter 0.15s ease;
}
.mr-clip:hover {
  border-color: color-mix(in srgb, #f0c419 55%, transparent);
  filter: brightness(1.04);
}
/* iMovie yellow selection ring — the filmstrip's strongest cue. */
.mr-clip.selected {
  border-color: #f0c419;
  z-index: 2;
  box-shadow:
    0 0 0 1px color-mix(in srgb, #f0c419 40%, transparent),
    0 0 12px color-mix(in srgb, #f0c419 35%, transparent);
}
/* Poster tiled across the span = filmstrip of frames. Height set inline
   from Clip Appearance (film/wave modes + clip height slider). */
.mr-clip-film {
  position: relative; width: 100%; height: 72px; flex: 1 1 auto;
  min-height: 0;
  background-color: #000;
  /* Vertical hairlines between tiles when the poster repeats. */
  box-shadow: inset 0 -1px 0 rgba(0, 0, 0, 0.55);
}
.mr-clip-film.mr-clip-labels {
  background: linear-gradient(180deg, #1c1c26, #12121a);
  display: flex; align-items: center;
}
.mr-clip-film.mr-clip-labels .mr-clip-name {
  position: static; background: none; padding: 0 6px;
  width: 100%;
}
.mr-clip-film.mr-thumb-missing {
  background:
    repeating-linear-gradient(
      90deg,
      #121218 0 54px,
      #0a0a0e 54px 56px
    );
}
.mr-clip img { display: none; } /* legacy; film uses background-image */
/* Clip's own audio under the picture — blue strip, drag to set gain.
   Height set inline from Clip Appearance. */
.mr-clip-wave {
  position: relative; height: 22px; flex: none;
  cursor: ns-resize;
  /* Waveform URI is painted via inline style; this is the iMovie blue bed. */
  background-color: #1a3a5c;
  box-shadow:
    inset 0 1px 0 color-mix(in srgb, #7ec8ff 25%, transparent),
    inset 0 0 0 1px color-mix(in srgb, #2a6a9a 55%, transparent);
}
.mr-clip-wave .mr-clip-name {
  z-index: 2;
}
.mr-clip-name-solo {
  position: absolute; inset: 0; z-index: 2;
  display: flex; align-items: center; padding: 0 6px;
  background: linear-gradient(transparent, rgba(0,0,0,0.55));
}
/* Diagonal hatch sits above the wave image so the strip always reads as volume. */
.mr-clip-wave::after {
  content: ""; position: absolute; inset: 0; pointer-events: none; z-index: 1;
  background: repeating-linear-gradient(
    -45deg,
    transparent,
    transparent 3px,
    color-mix(in srgb, #4a9fe0 22%, transparent) 3px,
    color-mix(in srgb, #4a9fe0 22%, transparent) 6px
  );
}
.mr-clip-wave:hover {
  filter: brightness(1.08);
}
.mr-clip-wave .mr-vol-line {
  z-index: 2;
  opacity: 0.55;
  background: #e8f4ff;
  box-shadow: 0 0 4px color-mix(in srgb, #7ec8ff 70%, transparent);
}
.mr-clip-wave:hover .mr-vol-line { opacity: 0.95; }
.mr-xtrans {
  position: absolute; top: 4px; right: 4px; z-index: 3;
  font-size: 8px; line-height: 12px; padding: 0 4px; border-radius: 3px;
  background: linear-gradient(180deg, var(--mor-accent-hover), var(--mor-accent));
  color: #141417; letter-spacing: -1px; pointer-events: none;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
}
/* Name sits on the film as a bottom fade label. */
.mr-clip-name {
  position: absolute; left: 0; right: 0; bottom: 0; z-index: 1;
  padding: 10px 5px 2px;
  max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  font-size: 10px; color: #f2f0ea;
  background: linear-gradient(transparent, rgba(0, 0, 0, 0.72));
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.9);
  pointer-events: none;
}
/* Duration pill — top-left on the film, iMovie "1.5h" / "4.0s".
   On T/V2 lane cards it sits inside the left padding so the name still reads. */
.mr-clip-dur {
  position: absolute; top: 5px; left: 5px; transform: none;
  z-index: 3; pointer-events: none;
  font-size: 10px; font-weight: 700; letter-spacing: 0.02em;
  color: #fff; padding: 1px 6px; border-radius: 4px;
  background: rgba(0, 0, 0, 0.62);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
  white-space: nowrap;
}
.mr-lane-item .mr-clip-dur {
  top: 2px; left: 11px; font-size: 9px; padding: 0 4px; line-height: 16px;
}
.mr-clip.selected .mr-clip-dur,
.mr-lane-item.selected .mr-clip-dur {
  background: color-mix(in srgb, #f0c419 88%, #8a6a00);
  color: #1a1400;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.45);
}
.mr-lane-item.title.selected .mr-clip-dur {
  background: color-mix(in srgb, var(--mor-warning) 88%, #8a6a00);
}

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
.mr-appear-btn.on { color: var(--mor-accent); }

/* FCP-style Clip Appearance popover over the timeline. */
.mr-appear-pop {
  position: absolute; top: 8px; right: 12px; z-index: 40;
  width: 280px; padding: 12px 12px 10px;
  background: color-mix(in srgb, #1a1a22 96%, black);
  border: 1px solid color-mix(in srgb, white 12%, transparent);
  border-radius: 10px;
  box-shadow: 0 12px 32px rgba(0,0,0,0.55), 0 0 0 1px rgba(0,0,0,0.3);
  display: flex; flex-direction: column; gap: 10px;
}
.mr-appear-row {
  display: flex; align-items: center; gap: 8px;
}
.mr-appear-label { font-size: 13px; opacity: 0.85; width: 1.4em; text-align: center; }
.mr-appear-zoom { flex: 1; accent-color: var(--mor-accent); min-width: 0; }
.mr-appear-icon-btn {
  background: none; border: none; color: var(--mor-text-muted); cursor: pointer;
  font-size: 14px; padding: 2px 4px;
}
.mr-appear-icon-btn:hover { color: var(--mor-accent); }
.mr-appear-modes {
  display: flex; gap: 4px; justify-content: space-between;
}
.mr-appear-mode {
  flex: 1; min-width: 0; height: 36px; padding: 0 2px;
  border: 1px solid color-mix(in srgb, white 12%, transparent);
  border-radius: 6px; background: #121218; color: #c8c4ce;
  font-size: 11px; letter-spacing: -0.5px; cursor: pointer;
  font-family: ui-monospace, monospace;
}
.mr-appear-mode:hover { border-color: var(--mor-accent); color: #fff; }
.mr-appear-mode.on {
  border-color: var(--mor-accent);
  background: color-mix(in srgb, var(--mor-accent) 22%, #121218);
  color: #fff;
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--mor-accent) 40%, transparent);
}
.mr-appear-check {
  display: flex; align-items: center; gap: 8px;
  font-size: 12px; color: var(--mor-text-muted); cursor: pointer; user-select: none;
}
.mr-appear-check input { accent-color: var(--mor-accent); }
.mr-appear-close {
  align-self: flex-end; margin-top: 2px;
  background: color-mix(in srgb, var(--mor-accent) 25%, transparent);
  border: 1px solid color-mix(in srgb, var(--mor-accent) 50%, transparent);
  color: #fff; border-radius: 6px; padding: 4px 12px; font-size: 12px; cursor: pointer;
}
.mr-appear-close:hover { background: color-mix(in srgb, var(--mor-accent) 40%, transparent); }

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

/* iMovie-inspired title style browser — mock look on black tiles. */
.mr-title-gallery {
  display: grid; grid-template-columns: repeat(auto-fill, minmax(88px, 1fr));
  gap: 8px; margin: 6px 0 12px;
}
.mr-title-tile {
  padding: 3px; border: 2px solid transparent; border-radius: 7px;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-btn) 90%, white), var(--mor-btn));
  cursor: pointer; display: flex; flex-direction: column; gap: 3px; align-items: stretch;
  color: var(--mor-text); font-size: 10px;
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 6%, transparent);
  transition: border-color 0.15s ease, box-shadow 0.18s ease, transform 0.15s ease, filter 0.15s ease;
}
.mr-title-tile:hover:not(:disabled) {
  border-color: var(--mor-border-light);
  transform: translateY(-2px);
  filter: brightness(1.06);
}
.mr-title-tile:disabled { opacity: 0.45; cursor: not-allowed; }
.mr-title-preview {
  aspect-ratio: 16 / 10; border-radius: 4px; background: #0a0a0c;
  display: flex; align-items: var(--mr-ts-seat, center); justify-content: center;
  padding: 6px 4px; box-sizing: border-box; overflow: hidden;
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.06);
}
.mr-title-preview span {
  color: var(--mr-ts-color, #fff);
  background: var(--mr-ts-bg, transparent);
  text-shadow: var(--mr-ts-shadow, none);
  font-size: var(--mr-ts-size, 12px);
  font-weight: 700; letter-spacing: 0.04em; text-align: center;
  line-height: 1.15; padding: 2px 4px; border-radius: 2px;
  max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.mr-title-tile-name {
  max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  text-align: center; color: var(--mor-text-muted);
}

/* Format bar under the phone — the knobs you hit while looking at the words. */
.mr-format-bar {
  display: flex; flex-wrap: wrap; align-items: center; gap: 4px;
  margin: 6px 0 4px; padding: 6px 8px;
  background: linear-gradient(180deg, color-mix(in srgb, var(--mor-panel) 92%, white), var(--mor-panel));
  border: 1px solid var(--mor-border);
  border-radius: 8px;
  box-shadow: inset 0 1px 0 color-mix(in srgb, white 6%, transparent);
}
.mr-format-select {
  max-width: 110px; min-width: 72px;
  background: var(--mor-btn); color: var(--mor-text);
  border: 1px solid var(--mor-border); border-radius: 5px;
  padding: 3px 6px; font-size: 11px;
}
.mr-format-step { min-width: 28px; padding: 2px 6px !important; font-size: 12px !important; }
.mr-format-size {
  min-width: 28px; text-align: center; font-size: 12px; font-variant-numeric: tabular-nums;
  color: var(--mor-text-muted);
}
.mr-format-sep {
  width: 1px; align-self: stretch; margin: 2px 3px;
  background: color-mix(in srgb, var(--mor-border) 80%, transparent);
}
.mr-format-swatch {
  width: 18px; height: 18px; border-radius: 50%; border: 2px solid transparent;
  padding: 0; cursor: pointer;
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.35);
}
.mr-format-swatch.active {
  border-color: var(--mor-accent);
  box-shadow: 0 0 0 1px color-mix(in srgb, var(--mor-accent) 50%, transparent);
}
.mr-format-pos { font-size: 10px !important; padding: 2px 6px !important; }
.mr-format-reset { margin-left: auto; font-size: 11px !important; }

/* On-monitor title seat — drag the words like iMovie. */
.mr-title-handle-layer {
  position: absolute; inset: 0; z-index: 4;
}
/* iMovie-style text selection box on the monitor — light rim + corner ticks. */
.mr-title-box {
  position: absolute; left: 6%; right: 6%;
  border: 1.5px solid rgba(255, 255, 255, 0.92);
  border-radius: 2px;
  box-shadow:
    0 0 0 1px rgba(0, 0, 0, 0.35),
    0 0 10px rgba(0, 0, 0, 0.25);
  cursor: grab;
  pointer-events: auto;
  box-sizing: border-box;
  display: flex; align-items: center; justify-content: center;
  min-height: 18px;
  background: color-mix(in srgb, white 6%, transparent);
  transition: border-color 0.12s ease, background 0.12s ease, box-shadow 0.12s ease;
}
.mr-title-box::before,
.mr-title-box::after {
  content: ""; position: absolute; width: 8px; height: 8px;
  border: 2px solid #fff;
  box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.35);
  pointer-events: none;
}
.mr-title-box::before { top: -2px; left: -2px; border-right: none; border-bottom: none; }
.mr-title-box::after { bottom: -2px; right: -2px; border-left: none; border-top: none; }
.mr-title-box:hover {
  background: color-mix(in srgb, white 12%, transparent);
  box-shadow:
    0 0 0 1px rgba(0, 0, 0, 0.4),
    0 0 14px rgba(255, 255, 255, 0.18);
}
.mr-title-box.dragging {
  cursor: grabbing;
  border-color: #fff;
  background: color-mix(in srgb, white 14%, transparent);
}
.mr-title-ghost {
  font-size: 11px; font-weight: 700; letter-spacing: 0.03em;
  text-align: center; max-width: 100%;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  text-shadow: 0 1px 2px rgba(0,0,0,0.85);
  padding: 0 4px; pointer-events: none;
}

.mr-export-dialog { display: flex; flex-direction: column; gap: 10px; min-width: 320px; }
.mr-export-blurb { margin: -4px 0 2px; font-size: 12px; }

/* Keyframe diamond sits at the right end of an animatable Transform row. */
.mr-xf-row { display: flex; align-items: center; gap: 6px; }
.mr-xf-row .mor-slider-shell { flex: 1; min-width: 0; }
.mr-kf-diamond {
  flex: 0 0 auto; margin-top: 14px; width: 20px; height: 20px; padding: 0;
  border: none; background: none; cursor: pointer; line-height: 1;
  font-size: 13px; color: var(--mor-text-muted);
  transition: color 0.15s ease, transform 0.15s ease, text-shadow 0.18s ease;
}
.mr-kf-diamond:hover { color: var(--mor-accent); transform: scale(1.15); }
.mr-kf-diamond.on {
  color: var(--mor-accent);
  text-shadow: 0 0 7px color-mix(in srgb, var(--mor-accent) 55%, transparent);
}
.mr-export-dialog .mr-toolbar { justify-content: flex-end; margin-top: 4px; }
.mr-settings-dialog { display: flex; flex-direction: column; gap: 12px; min-width: 360px; }
.mr-settings-note { margin: -4px 0 2px; font-size: 12px; }
.mr-settings-dialog .mr-toolbar { justify-content: flex-end; margin-top: 4px; }
.mr-keys-dialog { display: flex; flex-direction: column; gap: 8px; min-width: 340px; }
.mr-keys-opt { width: 100%; display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.mr-keys-name { font-weight: 600; }
.mr-keys-dialog .mr-toolbar { justify-content: flex-end; margin-top: 6px; }

.mr-hub { display: flex; flex-direction: column; gap: 10px; min-width: 460px; max-width: 560px; }
.mr-hub-row { display: flex; flex-direction: column; gap: 4px; padding: 10px 12px; border: 1px solid var(--mor-border, #333); border-radius: 8px; }
.mr-hub-head { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
.mr-hub-name { font-weight: 600; }
.mr-hub-kind { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; padding: 1px 6px; border-radius: 6px; background: var(--mor-surface-2, #2a2a33); }
.mr-hub-kind-mcp { color: var(--mor-accent, #b98cff); }
.mr-hub-kind-agent { color: #f0a860; }
.mr-hub-kind-bundle { color: #5fd0c0; }
.mr-hub-desc { margin: 0; }
.mr-hub-repo { font-family: monospace; font-size: 11px; opacity: 0.7; }
.mr-hub-actions { display: flex; gap: 8px; margin-top: 4px; }
.mr-hub .mr-toolbar { justify-content: flex-end; margin-top: 6px; }

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
        let anims: Vec<&str> = engine::TITLE_ANIMS.to_vec();
        let builtins = builtin_title_styles();
        assert!(builtins.len() >= 4, "expected a real gallery");
        for p in &builtins {
            // A typo here would silently fall back to white / no bevel / mid.
            assert!(colors.contains(&p.style.color.as_str()), "{}: bad colour {}", p.name, p.style.color);
            assert!(colors.contains(&p.style.outline_color.as_str()), "{}: bad outline colour", p.name);
            assert!(bevels.contains(&p.style.bevel.as_str()), "{}: bad bevel {}", p.name, p.style.bevel);
            assert!(positions.contains(&p.style.pos.as_str()), "{}: bad position {}", p.name, p.style.pos);
            assert!(anims.contains(&p.style.anim.as_str()), "{}: bad anim {}", p.name, p.style.anim);
        }
        // Applying a style keeps the card's own words and timing, takes the look.
        let card = TitleItem { text: "Keep me".into(), at: 4.0, dur: 9.0, ..base_title() };
        let styled = restyle(&card, &builtins[0].style);
        assert_eq!(styled.text, "Keep me");
        assert_eq!((styled.at, styled.dur), (4.0, 9.0));
        assert_eq!(styled.outline, builtins[0].style.outline);
    }

    #[test]
    fn fmt_clip_dur_prefers_seconds_under_a_minute() {
        assert_eq!(fmt_clip_dur(4.0), "4.0s");
        assert_eq!(fmt_clip_dur(0.5), "0.5s");
        assert_eq!(fmt_clip_dur(90.0), "1:30.0");
        assert_eq!(fmt_clip_dur(5400.0), "1.5h");
    }

    #[test]
    fn freeze_place_splits_when_there_is_room_on_both_sides() {
        assert_eq!(
            freeze_place(0.0, 10.0, 4.0, 0.1),
            Some(FreezePlace::Split { local: 4.0 })
        );
        assert_eq!(freeze_place(0.0, 10.0, 0.05, 0.1), Some(FreezePlace::Before));
        assert_eq!(freeze_place(0.0, 10.0, 9.95, 0.1), Some(FreezePlace::After));
        assert_eq!(freeze_place(0.0, 0.15, 0.07, 0.1), Some(FreezePlace::After));
        assert_eq!(freeze_place(1.0, 5.0, 0.5, 0.1), None);
    }

    #[test]
    fn can_join_clips_requires_same_source_and_abutting_range() {
        let base = Clip {
            path: "/x.mp4".into(),
            name: "x".into(),
            duration: 20.0,
            in_s: 0.0,
            out_s: 5.0,
            has_audio: true,
            effect: "None".into(),
            effect_amount: 1.0,
            framing: "Crop".into(),
            transform: engine::AnimatedTransform::default(),
            grade: engine::Grade::default(),
            speed: 1.0,
            reverse: false,
            volume: 1.0,
            denoise: 0.0,
            treat: "None".into(),
            transition: "None".into(),
            trans_dur: 0.5,
            thumb: String::new(),
            wave: String::new(),
            proxy: String::new(),
            group: 0,
            enabled: true,
            solo: false,
        };
        let right = Clip {
            in_s: 5.0,
            out_s: 10.0,
            ..base.clone()
        };
        assert!(can_join_clips(&base, &right), "split halves should join");
        let gap = Clip {
            in_s: 5.1,
            out_s: 10.0,
            ..base.clone()
        };
        assert!(!can_join_clips(&base, &gap), "gap in source is not a clean join");
        let other = Clip {
            path: "/y.mp4".into(),
            in_s: 5.0,
            out_s: 10.0,
            ..base.clone()
        };
        assert!(!can_join_clips(&base, &other), "different files never join");
    }

    #[test]
    fn replay_span_takes_up_to_requested_source() {
        assert_eq!(replay_span(0.0, 10.0, 5.0, 1.5), Some((3.5, 5.0)));
        assert_eq!(replay_span(0.0, 10.0, 1.0, 1.5), Some((0.0, 1.0)));
        assert_eq!(replay_span(0.0, 10.0, 0.05, 1.5), None);
    }

    #[test]
    fn title_edge_resize_from_either_side() {
        // Right edge: only duration changes.
        assert_eq!(title_edge_resize(2.0, 4.0, false, 1.5), (2.0, 5.5));
        assert_eq!(title_edge_resize(2.0, 4.0, false, -10.0), (2.0, 0.3));
        // Left edge: right edge stays put (at + dur fixed).
        assert_eq!(title_edge_resize(2.0, 4.0, true, 1.0), (3.0, 3.0));
        assert_eq!(title_edge_resize(2.0, 4.0, true, -1.0), (1.0, 5.0));
        // Can't drag past the origin or collapse under the minimum hold.
        assert_eq!(title_edge_resize(0.5, 2.0, true, -2.0), (0.0, 2.5));
        let (at, dur) = title_edge_resize(2.0, 4.0, true, 100.0);
        assert!((at + dur - 6.0).abs() < 1e-9);
        assert!((dur - 0.3).abs() < 1e-9);
    }

    #[test]
    fn media_edge_resize_free_and_sequential() {
        // Free-positioned (overlay): left keeps the right edge on the timeline.
        let (at, inn, out) = media_edge_resize(5.0, 1.0, 5.0, 10.0, 1.0, true, 1.0, true);
        assert_eq!((at, inn, out), (6.0, 2.0, 5.0));
        // Right grows the out-point within source.
        let (at, inn, out) = media_edge_resize(5.0, 1.0, 5.0, 10.0, 1.0, false, 2.0, true);
        assert_eq!((at, inn, out), (5.0, 1.0, 7.0));
        // Sequential V1: left only changes in-point; `at` is ignored.
        let (at, inn, out) = media_edge_resize(0.0, 1.0, 5.0, 10.0, 1.0, true, 0.5, false);
        assert_eq!((at, inn, out), (0.0, 1.5, 5.0));
        // Speed retimes source deltas (2× means 1s of timeline = 2s of source).
        let (_, inn, out) = media_edge_resize(0.0, 0.0, 4.0, 20.0, 2.0, false, 1.0, false);
        assert!((inn - 0.0).abs() < 1e-9);
        assert!((out - 6.0).abs() < 1e-9);
    }

    #[test]
    fn title_preview_css_carries_colour_and_seat() {
        let t = TitleItem {
            color: "Gold".into(),
            pos: "Lower third".into(),
            boxed: true,
            box_opacity: 0.8,
            outline: 6.0,
            ..base_title()
        };
        let css = title_preview_css(&t);
        assert!(css.contains("--mr-ts-color:#E8C060"), "{css}");
        assert!(css.contains("--mr-ts-seat:flex-end"), "{css}");
        assert!(css.contains("rgba(0,0,0,0.80)") || css.contains("rgba(0,0,0,0.8)"), "{css}");
    }

    #[test]
    fn free_seat_overrides_named_pos_and_snaps_back() {
        let mut t = base_title();
        assert!((seat_y(&t) - 0.45).abs() < 1e-9, "Middle by default");
        assert!(t.y_frac.is_none());

        // Free drag away from every named seat.
        set_seat_y(&mut t, 0.30);
        assert!((seat_y(&t) - 0.30).abs() < 1e-9);
        assert_eq!(t.y_frac, Some(0.30));
        assert_eq!(t.style_of("Hi").y_frac, 0.30);

        // Near Lower third → snaps to the named preset and drops free offset.
        set_seat_y(&mut t, 0.72);
        assert!(t.y_frac.is_none(), "snap clears free seat");
        assert_eq!(t.pos, "Lower third");
        assert!(seat_matches(&t, "Lower third"));

        set_seat_named(&mut t, "Top");
        assert!(t.y_frac.is_none());
        assert!((seat_y(&t) - 0.10).abs() < 1e-9);
    }

    #[test]
    fn older_titles_without_y_frac_still_seat_from_pos() {
        let t: TitleItem = serde_json::from_str(legacy_title_json()).unwrap();
        assert!(t.y_frac.is_none());
        assert!((seat_y(&t) - title_y("Middle")).abs() < 1e-9);
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
    fn disabled_and_solo_default_on_for_old_projects() {
        // Older saves lack the fields — serde defaults must enable and unsolo.
        let json = r#"{"path":"/x.mp4","name":"x","duration":5.0,"in_s":0.0,"out_s":5.0,
            "has_audio":true,"effect":"None","effect_amount":1.0,"framing":"Crop",
            "speed":1.0,"volume":1.0,"transition":"None","trans_dur":0.5,"group":0}"#;
        let c: Clip = serde_json::from_str(json).expect("legacy clip");
        assert!(c.enabled, "legacy clip must load enabled");
        assert!(!c.solo, "legacy clip must load unsoloed");
        assert!(c.spec().enabled);
    }

    #[test]
    fn item_class_marks_disabled_and_solo() {
        assert!(item_class("mr-clip", true, false, true, false).contains("disabled"));
        assert!(item_class("mr-clip", false, false, false, true).contains("solo"));
        assert!(!item_class("mr-clip", false, false, false, false).contains("disabled"));
    }

    #[test]
    fn clip_appear_heights_scale_and_modes_hide_strips() {
        let (f, w) = ClipAppear::FilmWave.heights(1.0);
        assert!((f - 72.0).abs() < 0.01 && (w - 22.0).abs() < 0.01);
        let (f2, w2) = ClipAppear::FilmWave.heights(2.0);
        assert!((f2 - 144.0).abs() < 0.01 && (w2 - 44.0).abs() < 0.01);
        assert!(!ClipAppear::Wave.show_film());
        assert!(ClipAppear::Wave.show_wave());
        assert!(ClipAppear::Film.show_film());
        assert!(!ClipAppear::Film.show_wave());
        assert!(ClipAppear::Labels.labels_only());
        assert_eq!(ClipAppear::ALL.len(), 6);
    }

    #[test]
    fn dropped_files_are_classified_by_extension() {
        assert_eq!(kind_of("/x/a.mp4"), Kind::Video);
        assert_eq!(kind_of("/x/a.MKV"), Kind::Video);
        assert_eq!(kind_of("/x/a.gif"), Kind::Video); // animated: video, not a still
        // Primary stills: phone photos, design exports, web.
        assert_eq!(kind_of("/x/a.png"), Kind::Still);
        assert_eq!(kind_of("/x/a.JPEG"), Kind::Still);
        assert_eq!(kind_of("/x/a.heic"), Kind::Still);
        assert_eq!(kind_of("/x/a.HEIF"), Kind::Still);
        assert_eq!(kind_of("/x/a.tiff"), Kind::Still);
        assert_eq!(kind_of("/x/a.bmp"), Kind::Still);
        assert_eq!(kind_of("/x/a.webp"), Kind::Still);
        assert_eq!(kind_of("/x/a.avif"), Kind::Still);
        // Not product stills — extension unknown to IMAGE_EXT → video path;
        // probe will refuse non-media (pdf/psd/raw) or import if ffmpeg can.
        assert_eq!(kind_of("/x/a.pdf"), Kind::Video);
        assert_eq!(kind_of("/x/a.psd"), Kind::Video);
        assert_eq!(kind_of("/x/a.cr2"), Kind::Video);
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
        assert_eq!(t.segments(), vec![("one two three".to_string(), 2.0, 3.0, None)]);

        // Karaoke: one full-line card per word, each tagged with its active index.
        t.karaoke = true;
        let ks = t.segments();
        assert_eq!(ks.len(), 3, "one card per word");
        assert_eq!(ks[0].0, "one two three", "karaoke keeps the whole line");
        assert_eq!((ks[0].3, ks[2].3), (Some(0), Some(2)), "active word walks the line");
        assert!((ks[2].1 + ks[2].2 - 5.0).abs() < 1e-9, "the run ends with the title");
        t.karaoke = false;

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
        // Knob order (place → size → spin → anchor → opacity), so field i gets i+1.
        assert_eq!(
            (t.x, t.y, t.scale, t.scale_x, t.scale_y, t.rotation, t.anchor_x, t.anchor_y, t.opacity),
            (1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0)
        );
        // V1 has nothing underneath it, so opacity is not offered there.
        assert_eq!(transform_knobs(&t, false).len(), 8);
        assert_eq!(transform_knobs(&t, true).len(), 9);
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
