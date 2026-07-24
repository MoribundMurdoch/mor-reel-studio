// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — data model + pure helpers.
// The timeline structs (Clip, TitleItem, OverlayItem, AudioItem, Snapshot,
// Project…), their ffmpeg-spec builders, and the free helper functions the
// UI leans on. No Dioxus here — this is the domain layer under Editor().

use crate::engine::ClipSpec;
use crate::{engine, keyframe};
use dioxus::prelude::Coroutine;

/// Named looks — (category, name, ffmpeg filter snippet), applied identically
/// to preview frames and export so preview = export. "Motion" ports moranima's
/// BgMotion camera moves (Zoom/Drift/Sway) with the same phase math
/// (ph = 0.1π·t, so 2ph ≈ 0.628t) translated to ffmpeg time-expressions.
// ponytail: moranima's particle overlays (fireflies/snow/embers) need a second
// video input; the per-clip effect slot is one linear chain, so they wait
// until effects can be filter_complex branches. Tilt (animated perspective)
// has no per-frame ffmpeg equivalent — Sway covers the feel.
pub const EFFECTS: &[(&str, &str, &str)] = &[
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
    ("Stylize", "Dreamy", "gblur=sigma=2,eq=brightness=0.04:saturation=1.15"),
    ("Stylize", "Vignette", "vignette"),
    ("Stylize", "Vintage", "curves=preset=vintage"),
    ("Stylize", "Cross process", "curves=preset=cross_process"),
    ("Stylize", "Faded", "curves=preset=lighter,eq=saturation=0.82"),
    ("Stylize", "Golden hour", "colortemperature=3800,eq=saturation=1.2:brightness=0.02"),
    ("Stylize", "Blockbuster", "colorbalance=rs=.12:gs=.02:bs=-.12:rh=-.06:bh=.12,eq=saturation=1.15"),
    ("Stylize", "Bleach bypass", "eq=saturation=0.35:contrast=1.35:brightness=0.02"),
    ("Stylize", "Film grain", "noise=alls=18:allf=t"),
    ("Stylize", "Ink", "edgedetect=mode=colormix:high=0"),
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
pub const BLEND_MODES: &[(&str, &str)] = &[
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
pub static HUB_EFFECTS: std::sync::OnceLock<std::sync::RwLock<Vec<(String, String, String)>>> = std::sync::OnceLock::new();

pub fn hub_effects() -> &'static std::sync::RwLock<Vec<(String, String, String)>> {
    HUB_EFFECTS.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}

/// Replace the active hub bundle effects (called after a hub install/enable).
pub fn set_hub_effects(effects: Vec<(String, String, String)>) {
    *hub_effects().write().unwrap() = effects;
}

/// Built-in effects plus every active hub bundle effect — what the picker lists.
/// A bundle name that collides with a built-in loses (built-ins win the lookup).
pub fn all_effects() -> Vec<(String, String, String)> {
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
pub fn is_keyer(name: &str) -> bool {
    EFFECTS.iter().any(|(cat, n, _)| *cat == "Key" && *n == name)
}

/// The ffmpeg snippet for a named effect: built-ins first, then hub bundle effects.
/// Returns `String` (not `&'static str`) because a hub effect is owned at runtime.
pub fn effect_filter(name: &str) -> String {
    if let Some((_, _, f)) = EFFECTS.iter().find(|(_, n, _)| *n == name) {
        return f.to_string();
    }
    hub_effects().read().unwrap().iter().find(|(_, n, _)| n == name).map(|(_, _, f)| f.clone()).unwrap_or_default()
}

/// Effect at strength `a` (0..=1): parameters interpolate from identity to the
/// full look, so a=1 is exactly the EFFECTS table and a=0 is no filter. Motion
/// amounts scale amplitude, matching moranima's `amount` semantics.
pub fn effect_filter_amt(name: &str, a: f64) -> String {
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

pub const TITLE_COLORS: &[(&str, &str)] = &[
    ("White", "white"),
    ("Black", "black"),
    ("Gold", "#E8C060"),
    ("Red", "#E5484D"),
    ("Cyan", "#3DD6D0"),
];

pub const TITLE_POS: &[(&str, f64)] = &[("Top", 0.10), ("Middle", 0.45), ("Lower third", 0.72)];

/// How close a free seat must be to a named preset for the format bar to light
/// that preset up (and for Shift-drag to snap onto it).
pub const TITLE_SEAT_SNAP: f64 = 0.05;

/// Bevel styles from the mor_cameo_emboss plugin. The stored value keeps the
/// cameo/intaglio lineage; the label says what it actually looks like, the way
/// the designer app words it — "raised" and "sunken" mean something to someone
/// who has never cut a seal.
pub const BEVELS: &[(&str, &str)] = &[
    ("Off", "Off"),
    ("Cameo", "Raised — stands off the video"),
    ("Intaglio", "Sunken — carved into it"),
];

pub fn bevel_label(value: &str) -> String {
    BEVELS.iter().find(|(v, _)| *v == value).map_or("Off", |(_, l)| l).to_string()
}

pub fn bevel_value(label: &str) -> String {
    BEVELS.iter().find(|(_, l)| *l == label).map_or("Off", |(v, _)| v).to_string()
}

/// How a source fills 9:16 — mostly for landscape imports. Crop covers and
/// center-crops, Blur fits over a blurred fill, Fit letterboxes on black, Zoom
/// punches in 1.5× then crops.
pub const FRAMINGS: &[&str] = &["Crop", "Blur", "Fit", "Zoom"];

/// One-line explanation of a framing mode, shown under the picker.
pub fn framing_hint(name: &str) -> &'static str {
    match name {
        "Blur" => "Whole picture over a blurred fill of itself — best for landscape footage.",
        "Fit" => "Letterboxed on black — nothing cropped, bars top and bottom.",
        "Zoom" => "Punches in 1.5× then crops — tighter, loses the edges.",
        _ => "Fills the frame and center-crops — the usual portrait fit.",
    }
}

/// What V1 and overlay tracks accept: video and photos on the same lanes, since
/// ffmpeg loops a still and a Motion effect turns it into a camera move over it.
pub fn media_ext() -> Vec<&'static str> {
    engine::VIDEO_EXT.iter().chain(engine::IMAGE_EXT).copied().collect()
}

/// Caps for free-form multi-track lanes (not counting magnetic V1).
pub const MAX_V_LANES: u8 = 6; // V2..V7
pub const MAX_T_LANES: u8 = 4; // T1..T4
pub const MAX_A_LANES: u8 = 6; // A1..A6
pub const DEF_V_LANES: u8 = 2;
pub const DEF_T_LANES: u8 = 2;
pub const DEF_A_LANES: u8 = 2;

pub fn def_v_lanes() -> u8 {
    DEF_V_LANES
}
pub fn def_t_lanes() -> u8 {
    DEF_T_LANES
}
pub fn def_a_lanes() -> u8 {
    DEF_A_LANES
}
pub fn overlay_track_default() -> u8 {
    2
}
pub fn title_track_default() -> u8 {
    1
}

/// Overlay track numbers shown for `v_lanes` count (2, 3, …).
pub fn overlay_tracks(v_lanes: u8) -> Vec<u8> {
    let n = v_lanes.clamp(1, MAX_V_LANES);
    (0..n).map(|i| 2 + i).collect()
}
/// Title track numbers 1..=t_lanes.
pub fn title_tracks(t_lanes: u8) -> Vec<u8> {
    let n = t_lanes.clamp(1, MAX_T_LANES);
    (1..=n).collect()
}
/// Audio bus numbers 1..=a_lanes.
pub fn audio_tracks(a_lanes: u8) -> Vec<u8> {
    let n = a_lanes.clamp(1, MAX_A_LANES);
    (1..=n).collect()
}

/// Timeline span a freshly imported source takes: a video keeps its whole
/// length, a still gets a sensible default the Out point can stretch.
pub fn initial_out(path: &str, duration: f64) -> f64 {
    if engine::is_still(path) { engine::STILL_DEFAULT } else { duration }
}

/// Upload ceilings for a portrait video, shortest first. Going over doesn't
/// break the export — it just means that platform will reject or truncate it,
/// which is worth knowing before you render rather than after.
// ponytail: static table, not a fetched policy — platforms change these rarely
// and a stale number here is a nudge, not a hard block.
pub const LIMITS: &[(&str, f64)] = &[("Shorts", 60.0), ("Reels", 90.0), ("TikTok", 600.0)];

/// Which platforms the reel has outgrown, e.g. "over Shorts 1:00.0". `platform`
/// narrows the check to a single target ("All platforms" checks every cap).
/// None while it still fits.
pub fn over_limits(total: f64, platform: &str) -> Option<String> {
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
pub enum KeyScheme {
    MorReel,
    Resolve,
    Premiere,
    FinalCut,
}

impl KeyScheme {
    pub const ALL: [KeyScheme; 4] = [Self::MorReel, Self::Resolve, Self::Premiere, Self::FinalCut];

    pub fn label(self) -> &'static str {
        match self {
            Self::MorReel => "MorReel (default)",
            Self::Resolve => "DaVinci Resolve",
            Self::Premiere => "Adobe Premiere Pro",
            Self::FinalCut => "Final Cut Pro",
        }
    }

    /// Stable token for persistence — never shown, so it can stay terse.
    pub fn id(self) -> &'static str {
        match self {
            Self::MorReel => "morreel",
            Self::Resolve => "resolve",
            Self::Premiere => "premiere",
            Self::FinalCut => "finalcut",
        }
    }

    pub fn from_id(s: &str) -> Self {
        Self::ALL.into_iter().find(|k| k.id() == s).unwrap_or(Self::MorReel)
    }

    /// The split-at-playhead (blade / add-edit) key in this editor's convention.
    pub fn split(self) -> &'static str {
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
pub fn help_key<'a>(keys: &'a str, what: &str, scheme: KeyScheme) -> &'a str {
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
pub enum Phase {
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
pub fn phase_lane_class(p: Phase) -> &'static str {
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
pub fn phase_for_selection(sel: Option<Sel>, current: Phase) -> Phase {
    match sel {
        Some(Sel::Title(_)) => Phase::Text,
        Some(Sel::Aud(_)) => Phase::Audio,
        // An adjustment layer carries a grade + effect — the Style phase's tools.
        Some(Sel::Adjust(_)) => {
            if current == Phase::Effects { current } else { Phase::Style }
        }
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

pub fn keyscheme_path() -> std::path::PathBuf {
    engine::config_dir().join("keyscheme")
}

/// App-wide, not per-project — a keyboard preference belongs to the person, like
/// the window mode, not to any one reel. Persisted beside the other app config.
pub fn load_keyscheme() -> KeyScheme {
    std::fs::read_to_string(keyscheme_path())
        .map(|s| KeyScheme::from_id(s.trim()))
        .unwrap_or(KeyScheme::MorReel)
}

pub fn save_keyscheme(k: KeyScheme) {
    let _ = std::fs::create_dir_all(engine::config_dir());
    let _ = std::fs::write(keyscheme_path(), k.id());
}

pub fn title_color(name: &str) -> &'static str {
    TITLE_COLORS.iter().find(|(n, _)| *n == name).map_or("white", |(_, c)| c)
}

pub fn title_y(name: &str) -> f64 {
    TITLE_POS.iter().find(|(n, _)| *n == name).map_or(0.45, |(_, y)| *y)
}

/// Vertical seat for a card: free `y_frac` when the user dragged it, otherwise
/// the named Top/Middle/Lower-third preset. Matches drawtext's
/// `y=(h-text_h)*y_frac` (0 = top of free space).
pub fn seat_y(t: &TitleItem) -> f64 {
    t.y_frac.unwrap_or_else(|| title_y(&t.pos)).clamp(0.0, 1.0)
}

/// Named preset whose seat is within snap distance of `y`, if any.
pub fn nearest_title_pos(y: f64) -> Option<&'static str> {
    TITLE_POS
        .iter()
        .filter(|(_, py)| (y - *py).abs() <= TITLE_SEAT_SNAP)
        .min_by(|a, b| (y - a.1).abs().partial_cmp(&(y - b.1).abs()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(n, _)| *n)
}

/// Write a free vertical seat. Snaps onto a named preset when close enough so
/// the format-bar seat buttons still light up after a careful drag.
pub fn set_seat_y(t: &mut TitleItem, y: f64) {
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
pub fn set_seat_named(t: &mut TitleItem, name: &str) {
    t.pos = name.to_string();
    t.y_frac = None;
}

/// Whether the format-bar seat button for `name` should show as active.
pub fn seat_matches(t: &TitleItem, name: &str) -> bool {
    (seat_y(t) - title_y(name)).abs() <= TITLE_SEAT_SNAP
}

/// Greedy word-wrap for caption cards — drawtext has no auto-wrap, so long
/// transcript segments would run off the 1080px frame.
pub fn wrap_caption(text: &str, max: usize) -> String {
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
pub fn chunk_caption(start: f64, end: f64, text: &str, max_words: usize) -> Vec<(f64, f64, String)> {
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
pub fn chunk_caption_splits_and_conserves_time() {
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
pub async fn render_one(t: &TitleItem) -> Result<Vec<String>, String> {
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
pub struct TitleItem {
    pub text: String,
    pub at: f64,
    pub dur: f64,
    pub font_size: f64,
    pub color: String,
    /// Named vertical seat: "Top" | "Middle" | "Lower third". Used when
    /// `y_frac` is unset; still updated to the nearest named seat after a drag
    /// so style galleries and old UI paths keep making sense.
    pub pos: String,
    /// Free vertical seat as a fraction of free space (0 = top), set by
    /// dragging the title on the monitor. `None` = use the named `pos` preset.
    /// Older projects load with None and look exactly as before.
    #[serde(default)]
    pub y_frac: Option<f64>,
    pub bevel: String,
    pub bevel_size: f64,
    /// Any installed fontconfig family.
    pub font: String,
    /// How multiple lines line up: "Centre" | "Left" | "Right".
    #[serde(default = "centre")]
    pub align: String,
    /// How the card arrives and leaves; see engine::TITLE_ANIMS.
    #[serde(default = "none_label")]
    pub anim: String,
    /// Bring the words in one at a time instead of all at once — the caption
    /// style every phone editor has. Costs one rendered card per word.
    #[serde(default)]
    pub reveal: bool,
    /// Karaoke: keep the whole line on screen and recolour each word as it is
    /// "spoken" (its even slice of the card's duration). Its own kinetic mode,
    /// distinct from `reveal`'s one-word-at-a-time build; renders via libass.
    #[serde(default)]
    pub karaoke: bool,
    /// The colour the active word takes in karaoke mode — the rest of the line
    /// stays `color`.
    #[serde(default = "karaoke_hi")]
    pub karaoke_color: String,
    /// "Text" or one of the shapes. A shape is a T-lane card like any other —
    /// it just draws a box instead of words.
    #[serde(default = "text_kind")]
    pub kind: String,
    #[serde(default = "shape_w_default")]
    pub shape_w: f64,
    #[serde(default = "shape_h_default")]
    pub shape_h: f64,
    #[serde(default)]
    pub shape_x: f64,
    /// Semi-opaque backdrop box behind the text (caption legibility).
    pub boxed: bool,
    /// Backdrop-box opacity, 0..1. The punchy caption plate wants ~0.85; the
    /// old fixed value was 0.45, which stays the default so nothing shifts.
    #[serde(default = "box_opacity_default")]
    pub box_opacity: f64,
    /// Outline width in px, 0 = none — legibility without an opaque plate.
    #[serde(default)]
    pub outline: f64,
    #[serde(default = "black")]
    pub outline_color: String,
    /// The rest of the bevel's own controls. Defaults match the designer app
    /// this bevel came from, so an older project loads looking as it did.
    #[serde(default = "bevel_soften")]
    pub soften: f64,
    #[serde(default = "bevel_depth")]
    pub depth: f64,
    #[serde(default = "bevel_angle")]
    pub angle: f64,
    #[serde(default = "bevel_altitude")]
    pub altitude: f64,
    #[serde(default = "bevel_opacity")]
    pub hi_opacity: f64,
    #[serde(default = "bevel_opacity")]
    pub sh_opacity: f64,
    /// Made by Auto captions — lets "Remove captions" clear them in bulk.
    pub caption: bool,
    /// Text track: 1 = T1, 2 = T2, … Higher composites on top of lower.
    /// Older projects load on T1.
    #[serde(default = "title_track_default")]
    pub track: u8,
    /// Rendered cards, one per revealed step (just one unless the words
    /// come in one at a time). Empty while a render is in flight.
    #[serde(skip)]
    pub pngs: Vec<String>,
    /// Drag-together group id; 0 = ungrouped.
    pub group: usize,
    /// When false: not composited (FCP-style disable for text cards).
    #[serde(default = "enabled_true")]
    pub enabled: bool,
}

/// The palette name, not the CSS colour — `title_color` looks these up by
/// display name and falls back to white on a miss.
pub fn black() -> String {
    "Black".to_string()
}
pub fn bevel_soften() -> f64 {
    4.0
}
pub fn bevel_depth() -> f64 {
    100.0
}
pub fn bevel_angle() -> f64 {
    120.0
}
pub fn bevel_altitude() -> f64 {
    30.0
}
pub fn bevel_opacity() -> f64 {
    0.75
}

/// The historical fixed caption-box opacity — the default so old projects and
/// untouched cards look exactly as before.
pub fn box_opacity_default() -> f64 {
    0.45
}

/// Default highlight colour for the active word in karaoke mode.
pub fn karaoke_hi() -> String {
    "Gold".to_string()
}

/// Which part of the on-screen transform box is being dragged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XfGrab {
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
pub fn xf_corners(t: &engine::Transform) -> [(f64, f64); 4] {
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
pub fn xf_edges(t: &engine::Transform) -> [(f64, f64); 4] {
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
pub fn xf_apply(
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
    // Everything pivots on the BOX's centre on screen, not the frame's —
    // scaling or rotating a sticker parked in a corner must measure from the
    // sticker, or the ratios (and the whole drag) feel unhinged.
    let (cx, cy) = (rl + (0.5 + start.x) * rw, rt + (0.5 + start.y) * rh);
    let (sin, cos) = start.rotation.to_radians().sin_cos();
    let mut t = start;
    match grab {
        XfGrab::Move => {
            t.x = start.x + (to.0 - from.0) / rw;
            t.y = start.y + (to.1 - from.1) / rh;
        }
        XfGrab::Scale => {
            // Ratio of distances from the box centre, so grabbing any corner
            // (or a corner clamped back into view) scales the same way.
            let d0 = ((from.0 - cx).powi(2) + (from.1 - cy).powi(2)).sqrt();
            let d1 = ((to.0 - cx).powi(2) + (to.1 - cy).powi(2)).sqrt();
            if d0 > 2.0 {
                t.scale = (start.scale * d1 / d0).clamp(0.1, 4.0);
            }
        }
        // A side handle stretches one axis, measured along the box's own
        // (rotated) axis — radial distance would make dragging sideways change
        // the height too, and screen-axis distance breaks on a tilted box.
        XfGrab::StretchX => {
            let d0 = ((from.0 - cx) * cos + (from.1 - cy) * sin).abs();
            let d1 = ((to.0 - cx) * cos + (to.1 - cy) * sin).abs();
            if d0 > 2.0 {
                t.scale_x = (start.scale_x * d1 / d0).clamp(0.1, 4.0);
            }
        }
        XfGrab::StretchY => {
            let d0 = ((from.1 - cy) * cos - (from.0 - cx) * sin).abs();
            let d1 = ((to.1 - cy) * cos - (to.0 - cx) * sin).abs();
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
pub type ShapeKnob = (&'static str, f64, fn(&mut TitleItem, f64));

pub fn shape_knobs(t: &TitleItem) -> Vec<ShapeKnob> {
    let set_w: fn(&mut TitleItem, f64) = |i, v| i.shape_w = v;
    vec![
        ("Width", t.shape_w, set_w),
        ("Height", t.shape_h, |i, v| i.shape_h = v),
        ("Across", t.shape_x, |i, v| i.shape_x = v),
    ]
}

/// One row of the Transform panel: label, value, min, max, step, and how to
/// write it back. Both lanes carry the same struct, so one table serves both.
pub type XformKnob = (&'static str, f64, f64, f64, f64, fn(&mut engine::Transform, f64));

/// Opacity is only offered where it composites over something — on V1 there is
/// nothing underneath it but black.
pub fn transform_knobs(t: &engine::Transform, with_opacity: bool) -> Vec<XformKnob> {
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
pub fn xf_keyable(label: &str) -> bool {
    matches!(label, "Scale" | "Opacity" | "Rotation")
}

/// The `Animated` field a transform-row label edits. Every knob maps to one, so
/// a slider drag can key an animated field in place rather than flattening it.
pub fn xf_field<'a>(
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
pub fn xf_write(at: &mut engine::AnimatedTransform, label: &str, v: f64, t: f64) {
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
pub fn xf_toggle_key(at: &mut engine::AnimatedTransform, label: &str, t: f64) {
    if let Some(f) = xf_field(at, label) {
        if f.has_key(t) {
            f.remove_key(t);
        } else {
            let v = f.sample(t);
            f.set_key(t, v, keyframe::Interp::Smooth);
        }
    }
}

/// The easing of the key at the playhead (`t`, clip-local), if one sits there.
/// Drives the velocity chip next to the diamond.
pub fn xf_key_interp(at: &engine::AnimatedTransform, label: &str, t: f64) -> Option<keyframe::Interp> {
    // xf_field takes &mut; a throwaway clone lets a read borrow it immutably.
    let mut at = at.clone();
    xf_field(&mut at, label).and_then(|f| f.key_interp(t))
}

/// Cycle the ease of the key at the playhead: Smooth → Linear → Hold → Smooth.
/// This is the "velocity" control — each mode compiles to a different segment
/// curve in `engine::curve_expr`, so preview and export both change with it.
pub fn xf_cycle_interp(at: &mut engine::AnimatedTransform, label: &str, t: f64) {
    use keyframe::Interp::*;
    if let Some(f) = xf_field(at, label) {
        if let Some(cur) = f.key_interp(t) {
            let next = match cur {
                Smooth => Linear,
                Linear => Hold,
                Hold => Smooth,
            };
            f.set_key(t, f.sample(t), next);
        }
    }
}

/// Short label for the velocity chip.
pub fn interp_glyph(i: keyframe::Interp) -> &'static str {
    match i {
        keyframe::Interp::Hold => "Hold",
        keyframe::Interp::Linear => "Lin",
        keyframe::Interp::Smooth => "Ease",
    }
}

/// Is this row's field currently a curve? Fills its diamond.
pub fn xf_field_animated(at: &engine::AnimatedTransform, label: &str) -> bool {
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
pub type GradeKnob = (&'static str, f64, f64, f64, f64, fn(&mut engine::Grade, f64));

pub fn grade_knobs(g: &engine::Grade) -> Vec<GradeKnob> {
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
pub type BevelKnob = (&'static str, f64, f64, f64, fn(&mut TitleItem, f64));

pub fn bevel_knobs(t: &TitleItem) -> Vec<BevelKnob> {
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
pub const REVEAL_SPAN: f64 = 0.6;

/// Prefixes of `text` ending at each word, cut out of the original string so
/// the line breaks it already has survive exactly. Rejoining split words with
/// spaces would unwrap a caption and make it jump between lines mid-reveal.
pub fn word_prefixes(text: &str) -> Vec<String> {
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
    pub fn segments(&self) -> Vec<(String, f64, f64, Option<usize>)> {
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
    pub fn card_at(&self, t: f64) -> Option<usize> {
        self.segments().iter().position(|(_, at, dur, _)| t >= *at && t < at + dur)
    }

    /// Map the timeline item onto the engine's render parameters. The item
    /// stores friendly choices (a colour name, a position name); the style
    /// stores what ffmpeg and the bevel actually need.
    /// This card's look, carrying whichever words this step shows.
    pub fn style_of(&self, text: &str) -> engine::TitleStyle {
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
pub fn title_alpha(t: f64, at: f64, dur: f64) -> f64 {
    let f = engine::title_fade(dur).max(0.01);
    ((t - at) / f).min((at + dur - t) / f).clamp(0.0, 1.0)
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Clip {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub in_s: f64,
    pub out_s: f64,
    pub has_audio: bool,
    pub effect: String,
    /// Effect strength 0..=1 (parameter interpolation, not a crossfade).
    pub effect_amount: f64,
    pub framing: String,
    /// Where the picture sits in the frame — scale, position, rotation.
    #[serde(default)]
    pub transform: engine::AnimatedTransform,
    /// Primary colour grade — exposure/contrast/saturation/warmth. Runs before
    /// the effect preset in `look()`; identity by default so old projects load.
    #[serde(default)]
    pub grade: engine::Grade,
    /// Playback rate: 0.5 is slow motion, 2.0 is double speed.
    #[serde(default = "unity")]
    pub speed: f64,
    /// Play the trimmed span backwards. Off by default so old projects load.
    #[serde(default)]
    pub reverse: bool,
    /// Gain on this clip's own audio; 0.0 mutes it.
    #[serde(default = "unity")]
    pub volume: f64,
    /// "Reduce background noise" strength 0..=1 for this clip's own audio.
    #[serde(default)]
    pub denoise: f64,
    /// EQ / voice treatment (engine::AUDIO_TREATS); "None" = flat.
    #[serde(default = "none_label")]
    pub treat: String,
    /// Transition *into* this clip. Stored on the incoming clip so it survives
    /// reordering, and ignored on the first clip — nothing precedes it.
    #[serde(default = "none_label")]
    pub transition: String,
    #[serde(default = "half")]
    pub trans_dur: f64,
    #[serde(skip)]
    pub thumb: String,
    /// Full-source waveform data URI for this clip's own audio; empty until the
    /// background render lands, and always empty for a silent source.
    #[serde(skip)]
    pub wave: String,
    /// 480p scrub proxy path; empty until the background build finishes.
    #[serde(skip)]
    pub proxy: String,
    /// Drag-together group id; 0 = ungrouped.
    pub group: usize,
    /// When false: invisible and silent in preview/export, still on the
    /// timeline (FCP Clip › Disable). Default true for older projects.
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    /// When true and any item is soloed, only soloed items contribute audio;
    /// non-soloed clips render B&W (FCP Clip › Solo).
    #[serde(default)]
    pub solo: bool,
}

impl Clip {
    pub fn spec(&self) -> ClipSpec {
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
    pub fn look(&self) -> String {
        join_chain(
            join_chain(self.transform.chain(engine::W, engine::H, false), self.grade.chain()),
            effect_filter_amt(&self.effect, self.effect_amount),
        )
    }

    /// Seconds on the timeline — the source span retimed by the speed.
    pub fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// Source-file time for a point `off` seconds of *source* into the trimmed
    /// span — mirrored when reversed, so preview and export read the same frame.
    pub fn src_at(&self, off: f64) -> f64 {
        if self.reverse { self.out_s - off } else { self.in_s + off }
    }

    /// What preview/scrub extraction should read: the proxy once built.
    pub fn scrub_path(&self) -> String {
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
pub struct OverlayItem {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    pub effect: String,
    /// Effect strength 0..=1 (parameter interpolation, not a crossfade).
    pub effect_amount: f64,
    pub framing: String,
    /// Where the picture sits in the frame. A scaled-down overlay is a
    /// picture-in-picture, since V2 composites over V1.
    #[serde(default)]
    pub transform: engine::AnimatedTransform,
    /// Primary colour grade — same as a V1 clip, runs before the effect look.
    #[serde(default)]
    pub grade: engine::Grade,
    /// Playback rate, same as a V1 clip: 0.5 is slow motion.
    #[serde(default = "unity")]
    pub speed: f64,
    /// Play the trimmed span backwards.
    #[serde(default)]
    pub reverse: bool,
    /// ffmpeg `blend=all_mode` value for compositing this layer (empty = alpha-over).
    /// Screen/Add turn a black-backed light-leak or particle plate into a glow over V1.
    #[serde(default)]
    pub blend: String,
    /// Overlay track number: 2 = V2, 3 = V3, … Higher composites on top of lower.
    /// Older projects load as V2.
    #[serde(default = "overlay_track_default")]
    pub track: u8,
    #[serde(skip)]
    pub proxy: String,
    /// Drag-together group id; 0 = ungrouped.
    pub group: usize,
    /// When false: skip compositing (invisible). Still sits on its track.
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    /// Solo isolate — see [`Clip::solo`].
    #[serde(default)]
    pub solo: bool,
}

impl OverlayItem {
    /// Seconds this cutaway covers V1 for — its source span, retimed.
    pub fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// Source-file time for a point `off` seconds into the cutaway, mirrored
    /// when reversed. (Preview maps timeline seconds 1:1, matching the old code.)
    pub fn src_at(&self, off: f64) -> f64 {
        if self.reverse { self.out_s - off } else { self.in_s + off }
    }

    /// Same as a clip's, but built for a layer that composites: the area the
    /// picture vacates is transparent, so V1 shows through around it.
    pub fn look(&self) -> String {
        join_chain(
            join_chain(self.transform.chain(engine::W, engine::H, true), self.grade.chain()),
            effect_filter_amt(&self.effect, self.effect_amount),
        )
    }

    pub fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

/// A full-frame adjustment layer on the FX lane: a grade + effect applied to
/// everything composited beneath it (V1 + V2) over its span, carrying no media
/// of its own. The lazy analog of Premiere/After Effects adjustment layers —
/// one look across several clips without setting it on each.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdjustmentItem {
    pub at: f64,
    pub dur: f64,
    #[serde(default = "none_label")]
    pub effect: String,
    #[serde(default = "unity")]
    pub effect_amount: f64,
    #[serde(default)]
    pub grade: engine::Grade,
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    /// Drag-together group id; 0 = ungrouped.
    #[serde(default)]
    pub group: usize,
}

impl AdjustmentItem {
    pub fn new(at: f64, dur: f64) -> Self {
        AdjustmentItem {
            at,
            dur,
            effect: "None".into(),
            effect_amount: 1.0,
            grade: engine::Grade::default(),
            enabled: true,
            group: 0,
        }
    }

    /// The grade + effect chain applied to the picture below — no geometry, it's
    /// a full-frame filter. Empty when nothing is set (a no-op adjustment).
    pub fn look(&self) -> String {
        join_chain(self.grade.chain(), effect_filter_amt(&self.effect, self.effect_amount))
    }

    /// Short lane label: the effect name, else "Grade" when only a grade is set,
    /// else a plain "FX" for an empty (no-op) layer.
    pub fn label(&self) -> String {
        if self.effect != "None" && !self.effect.is_empty() {
            self.effect.clone()
        } else if !self.grade.is_identity() {
            "Grade".to_string()
        } else {
            "FX".to_string()
        }
    }

    pub fn spec(&self) -> engine::AdjustSpec {
        engine::AdjustSpec {
            at: self.at,
            dur: self.dur,
            look: self.look(),
            enabled: self.enabled,
        }
    }
}

pub fn audio_lane_default() -> u8 {
    1
}
/// Sentinel for "volume end equals volume start" so older projects stay flat.
pub fn vol_end_default() -> f64 {
    -1.0
}
/// afftdn noise floor for projects saved before the knob existed — the value
/// that was hardcoded, so old projects denoise exactly as they used to.
pub fn noise_floor_default() -> f64 {
    -25.0
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioItem {
    pub path: String,
    pub name: String,
    pub duration: f64,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    /// Start gain; with `vol_end` forms a linear automation ramp.
    pub volume: f64,
    /// End gain for volume automation. Negative = same as `volume`.
    #[serde(default = "vol_end_default")]
    pub vol_end: f64,
    /// How hard this bed ducks under the main track while it is talking.
    /// 0 = never. Music under a voiceover is the reason this exists.
    #[serde(default)]
    pub duck: f64,
    /// Fade in from silence (seconds of the kept span).
    #[serde(default)]
    pub fade_in: f64,
    /// Fade out to silence (seconds of the kept span).
    #[serde(default)]
    pub fade_out: f64,
    /// Spectral denoise 0..=1.
    #[serde(default)]
    pub denoise: f64,
    /// afftdn noise floor in dB (−80..=−20); the sensitivity knob.
    #[serde(default = "noise_floor_default")]
    pub noise_floor: f64,
    /// Adaptively track the noise floor over the clip (afftdn `tn`).
    #[serde(default)]
    pub track_noise: bool,
    /// Broadband compression 0..=1.
    #[serde(default)]
    pub compress: f64,
    /// Noise gate 0..=1 (`agate`) — kills room tone between words.
    #[serde(default)]
    pub gate: f64,
    /// De-click strength 0..=1 (`adeclick`) — pops in field audio.
    #[serde(default)]
    pub declick: f64,
    /// One of engine::AUDIO_TREATS — EQ / voice shaping.
    #[serde(default = "none_label")]
    pub treat: String,
    /// Mix bus: 1 = A1, 2 = A2, … up to A6. All under V1.
    #[serde(default = "audio_lane_default")]
    pub lane: u8,
    /// Full-source waveform data URI; empty until the background render lands.
    #[serde(skip)]
    pub wave: String,
    /// Drag-together group id; 0 = ungrouped.
    pub group: usize,
    /// When false: silent and out of the mix (still on the lane).
    #[serde(default = "enabled_true")]
    pub enabled: bool,
    /// Solo isolate — see [`Clip::solo`].
    #[serde(default)]
    pub solo: bool,
}

impl AudioItem {
    pub fn end_gain(&self) -> f64 {
        if self.vol_end < 0.0 {
            self.volume
        } else {
            self.vol_end
        }
    }

    pub fn lane_tag(&self) -> String {
        format!("A{}", self.lane.max(1).min(MAX_A_LANES))
    }

    pub fn span(&self) -> f64 {
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

/// One mixer channel: a gain fader, a mute, and a solo.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Strip {
    #[serde(default = "unity")]
    pub gain: f64,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
}
impl Default for Strip {
    fn default() -> Self {
        Strip { gain: 1.0, mute: false, solo: false }
    }
}

/// Track index into [`Mixer::tracks`]: 0 = V1, 1 = A1, 2 = A2, …
pub const MIX_V1: usize = 0;

pub fn mix_label(i: usize) -> String {
    if i == 0 {
        "V1".into()
    } else {
        format!("A{i}")
    }
}

pub fn default_mixer_tracks() -> Vec<Strip> {
    vec![Strip::default(); 1 + DEF_A_LANES as usize]
}

/// Multi-bus mixer plus a master fader. Index 0 is V1 clip audio; 1.. are A-lanes.
/// Grows with added audio tracks. Per-clip gain still rides on top. Applied once
/// in `gather_specs`, so preview and export hear exactly the same balance.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mixer {
    #[serde(default = "default_mixer_tracks")]
    pub tracks: Vec<Strip>,
    #[serde(default = "unity")]
    pub master: f64,
}
impl Default for Mixer {
    fn default() -> Self {
        Mixer {
            tracks: default_mixer_tracks(),
            master: 1.0,
        }
    }
}
impl Mixer {
    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|s| s.solo)
    }
    /// Ensure strips exist for V1 + `a_lanes` audio buses.
    pub fn ensure_lanes(&mut self, a_lanes: u8) {
        let need = 1 + a_lanes.max(1).min(MAX_A_LANES) as usize;
        while self.tracks.len() < need {
            self.tracks.push(Strip::default());
        }
    }
    /// Effective linear gain for track `i`, folding in mute, solo and master.
    /// 0.0 means silent — muted, or soloed-out while another track solos.
    pub fn gain_of(&self, i: usize) -> f64 {
        let Some(s) = self.tracks.get(i) else {
            return self.master.max(0.0);
        };
        if s.mute || (self.any_solo() && !s.solo) {
            0.0
        } else {
            (s.gain * self.master).max(0.0)
        }
    }
    /// Track index for an audio bed's lane number (1 = A1, 2 = A2, …).
    pub fn lane_track(lane: u8) -> usize {
        lane.max(1).min(MAX_A_LANES) as usize
    }
}

/// Inline CSS windowing a full-source waveform image to the span an item keeps.
/// The image spans the whole source, so it is stretched to `duration` seconds
/// wide and shifted left by the in point; trims and splits are then free, since
/// they only move this window rather than re-rendering anything.
///
/// `speed` compresses both, so a retimed V1 clip's waveform still lines up with
/// its retimed width on the timeline. A1 items are never retimed and pass 1.0.
pub fn wave_css(wave: &str, duration: f64, in_s: f64, scale: f64, speed: f64) -> String {
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
pub fn unity() -> f64 {
    1.0
}
/// Clip/overlay/audio/title `enabled` default — older projects load as on.
pub fn enabled_true() -> bool {
    true
}
/// Solid black monitor frame for a disabled V1 clip under the playhead.
pub const BLACK_PREVIEW: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='540' height='960'%3E%3Crect fill='black' width='100%25' height='100%25'/%3E%3C/svg%3E";
pub fn none_label() -> String {
    "None".to_string()
}
pub fn centre() -> String {
    "Centre".to_string()
}
pub fn text_kind() -> String {
    "Text".to_string()
}
pub fn shape_w_default() -> f64 {
    0.6
}
pub fn shape_h_default() -> f64 {
    0.12
}
/// Default transition length. Short, because a reel cut is quick.
pub fn half() -> f64 {
    0.5
}

/// A saved title look, kept outside any project so a series of reels can share
/// one. The whole item is stored and only its styling is applied — that way a
/// preset gains any field a title gains, with nothing to keep in step.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TitlePreset {
    pub name: String,
    pub style: TitleItem,
}

pub fn presets_path() -> std::path::PathBuf {
    engine::config_dir().join("title-presets.json")
}

pub fn load_presets() -> Vec<TitlePreset> {
    std::fs::read_to_string(presets_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_presets(all: &[TitlePreset]) -> Result<(), String> {
    let dir = engine::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(presets_path(), json).map_err(|e| e.to_string())
}

/// The out-of-the-box title look every new card and every built-in style starts
/// from. Callers override `text`/`at`/`dur` per instance.
pub fn base_title() -> TitleItem {
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
        track: 1,
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
pub fn builtin_title_styles() -> Vec<TitlePreset> {
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
pub struct Layout {
    pub name: String,
    pub inspector_open: bool,
    pub inspector_float: bool,
    #[serde(default)]
    pub float_xy: Option<(f64, f64)>,
    #[serde(default)]
    pub float_size: Option<(f64, f64)>,
}

/// Built-in arrangements, always offered above the user's saved ones.
pub fn preset_layouts() -> [Layout; 3] {
    [
        Layout { name: "Editing".into(), inspector_open: true, inspector_float: false, float_xy: None, float_size: None },
        Layout { name: "Focus".into(), inspector_open: false, inspector_float: false, float_xy: None, float_size: None },
        Layout { name: "Floating".into(), inspector_open: true, inspector_float: true, float_xy: None, float_size: None },
    ]
}

pub fn layouts_path() -> std::path::PathBuf {
    engine::config_dir().join("layouts.json")
}

pub fn load_layouts() -> Vec<Layout> {
    std::fs::read_to_string(layouts_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_layouts(all: &[Layout]) -> Result<(), String> {
    let dir = engine::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(layouts_path(), json).map_err(|e| e.to_string())
}

/// Take `src`'s look but keep `dst`'s own words, timing and lane identity.
/// A style is everything a card looks like; it is never what it says or when.
pub fn restyle(dst: &TitleItem, src: &TitleItem) -> TitleItem {
    TitleItem {
        text: dst.text.clone(),
        at: dst.at,
        dur: dst.dur,
        group: dst.group,
        caption: dst.caption,
        enabled: dst.enabled,
        track: dst.track,
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
pub struct Snapshot {
    pub clips: Vec<Clip>,
    pub overlays: Vec<OverlayItem>,
    pub audios: Vec<AudioItem>,
    pub titles: Vec<TitleItem>,
    /// FX lane: full-frame grade/effect layers over the picture below. Older
    /// projects (and the CLI/MCP) load with none.
    #[serde(default)]
    pub adjustments: Vec<AdjustmentItem>,
    /// Beat markers, in timeline seconds, sorted. Not a lane — they hold no
    /// media, they are just the places you want cuts to land.
    #[serde(default)]
    pub markers: Vec<f64>,
    /// Track mixer (V1 + A-lanes level, mute, solo + master). Older projects load
    /// with a flat, un-muted mixer.
    #[serde(default)]
    pub mixer: Mixer,
    /// How many overlay tracks (V2..) to show. Older projects default to 2.
    #[serde(default = "def_v_lanes")]
    pub v_lanes: u8,
    /// How many text tracks (T1..) to show.
    #[serde(default = "def_t_lanes")]
    pub t_lanes: u8,
    /// How many audio tracks (A1..) to show.
    #[serde(default = "def_a_lanes")]
    pub a_lanes: u8,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            clips: Vec::new(),
            overlays: Vec::new(),
            audios: Vec::new(),
            titles: Vec::new(),
            adjustments: Vec::new(),
            markers: Vec::new(),
            mixer: Mixer::default(),
            v_lanes: DEF_V_LANES,
            t_lanes: DEF_T_LANES,
            a_lanes: DEF_A_LANES,
        }
    }
}

/// Grow lane counts so every item has a visible track, clamp to caps.
pub fn normalize_lanes(s: &mut Snapshot) {
    let max_v = s
        .overlays
        .iter()
        .map(|o| o.track.max(2))
        .max()
        .unwrap_or(2);
    // track 2 → 1 lane, track 3 → 2 lanes, …
    let need_v = (max_v - 1).clamp(1, MAX_V_LANES);
    s.v_lanes = s.v_lanes.max(need_v).clamp(1, MAX_V_LANES);

    let max_t = s
        .titles
        .iter()
        .map(|t| t.track.max(1))
        .max()
        .unwrap_or(1);
    s.t_lanes = s.t_lanes.max(max_t).clamp(1, MAX_T_LANES);

    let max_a = s
        .audios
        .iter()
        .map(|a| a.lane.max(1))
        .max()
        .unwrap_or(1);
    s.a_lanes = s.a_lanes.max(max_a).clamp(1, MAX_A_LANES);
    s.mixer.ensure_lanes(s.a_lanes);

    for o in &mut s.overlays {
        o.track = o.track.max(2).min(1 + s.v_lanes);
    }
    for t in &mut s.titles {
        t.track = t.track.max(1).min(s.t_lanes);
    }
    for a in &mut s.audios {
        a.lane = a.lane.max(1).min(s.a_lanes);
    }
}

/// Whether `current` differs from the last-saved baseline JSON — the "● Edited"
/// test. Comparing serialized JSON (not the struct) is deliberate: thumb/wave/
/// proxy are `#[serde(skip)]`, so a background proxy or waveform landing never
/// counts as an edit — only what would be written to disk does. With no baseline
/// (never saved this session), any content on the timeline counts as unsaved.
pub fn timeline_dirty(current: &Snapshot, baseline: Option<&str>) -> bool {
    let empty = current.clips.is_empty()
        && current.overlays.is_empty()
        && current.audios.is_empty()
        && current.titles.is_empty()
        && current.adjustments.is_empty();
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
pub fn otio_rt(seconds: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": (seconds * fps).round() })
}

pub fn otio_range(start: f64, dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": otio_rt(start, fps),
        "duration": otio_rt(dur.max(0.0), fps),
    })
}

/// A clip referencing an external media file by `source_range` (the portion of
/// the source used). `path` becomes a `file://` URL the receiving app resolves.
pub fn otio_clip(name: &str, path: &str, src_start: f64, src_dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({
        "OTIO_SCHEMA": "Clip.1",
        "name": name,
        "source_range": otio_range(src_start, src_dur, fps),
        "media_reference": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": format!("file://{path}") },
    })
}

pub fn otio_gap(dur: f64, fps: f64) -> serde_json::Value {
    serde_json::json!({ "OTIO_SCHEMA": "Gap.1", "source_range": otio_range(0.0, dur, fps) })
}

/// Lay `(at, dur, node)` items onto one track, inserting a `Gap` before any item
/// that doesn't start where the previous one ended. Items must be sorted by `at`.
pub fn otio_lay(items: Vec<(f64, f64, serde_json::Value)>, fps: f64) -> Vec<serde_json::Value> {
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
pub fn snapshot_to_otio(snap: &Snapshot, name: &str, fps: f64) -> String {
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
pub struct ProjectSettings {
    /// One of the `LIMITS` names, or "All platforms" to warn against every cap.
    pub platform: String,
    /// Target export width (a `SIZES` entry); 9:16 fixes the height.
    pub resolution: u32,
    /// Whether safe-area guides start on for this project.
    pub guides: bool,
    pub title: String,
    pub author: String,
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
pub struct Project {
    #[serde(flatten)]
    pub snap: Snapshot,
    #[serde(default)]
    pub settings: ProjectSettings,
}

/// The duration cap for one platform, if it has one.
pub fn platform_cap(platform: &str) -> Option<f64> {
    LIMITS.iter().find(|(n, _)| *n == platform).map(|(_, c)| *c)
}

/// What the inspector is editing.
#[derive(Clone, Copy, PartialEq)]
pub enum Sel {
    Main(usize),
    Over(usize),
    Aud(usize),
    Title(usize),
    Adjust(usize),
}

/// The noun for whatever's selected, shown in the inspector title so the panel
/// header names what you're editing instead of a redundant in-body label.
pub fn sel_noun(sel: Option<Sel>, titles: &[TitleItem]) -> Option<&'static str> {
    Some(match sel? {
        Sel::Main(_) => "Clip",
        Sel::Over(_) => "Cutaway",
        Sel::Aud(_) => "Audio",
        Sel::Title(k) => {
            let t = titles.get(k)?;
            if t.caption { "Caption" } else if t.kind != "Text" { "Shape" } else { "Text" }
        }
        Sel::Adjust(_) => "Adjustment",
    })
}

/// The popped inspector window's identity: what you're editing, in that kind's
/// own colour, so the window reads as a purpose-built tool rather than a
/// stretched panel. A live selection wins; with nothing selected it names the
/// current workspace instead. Returns (accent hex, glyph, title, eyebrow).
pub fn solo_identity(
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
pub enum Ctx {
    Monitor,
    Timeline,
    Clip(usize),
    Over(usize),
    Aud(usize),
    Title(usize),
}

/// The transition leading into clip `i`, clamped to something both it and the
/// clip before it can accommodate. 0 for a cut, and always 0 for the first clip.
pub fn fade_in(clips: &[Clip], i: usize) -> f64 {
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
pub fn extents(clips: &[Clip]) -> Vec<f64> {
    (0..clips.len())
        .map(|i| (clips[i].trimmed() - fade_in(clips, i + 1)).max(0.05))
        .collect()
}

/// If `t` falls inside the transition leading into some clip, return that
/// clip's index, how far the blend has run (0..1), and the source time to pull
/// its frame from. The overlap sits at the end of the outgoing clip's extent,
/// which is exactly where the next clip's own footage has already started.
pub fn transition_at(clips: &[Clip], t: f64) -> Option<(usize, f64, f64)> {
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
pub fn locate(clips: &[Clip], t: f64) -> Option<(usize, f64)> {
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

pub fn fmt_t(s: f64) -> String {
    let s = s.max(0.0); // squash negatives and -0.0 → "0:00.0"
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// Human file size for the share dialog's info strip. Estimates only ever
/// reach MB/GB territory, but small stays honest as KB.
pub fn fmt_bytes(b: u64) -> String {
    match b {
        0..=999_999 => format!("{:.0} KB", b as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1} MB", b as f64 / 1e6),
        _ => format!("{:.2} GB", b as f64 / 1e9),
    }
}

/// Short clip length for the filmstrip badge — iMovie-style "4.0s" under a
/// minute, "1.5h" at an hour and up, full deck readout in between.
pub fn fmt_clip_dur(s: f64) -> String {
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
pub fn title_edge_resize(at0: f64, dur0: f64, left: bool, dt: f64) -> (f64, f64) {
    pub const MIN: f64 = 0.3;
    if left {
        let end = at0 + dur0;
        let at = (at0 + dt).clamp(0.0, (end - MIN).max(0.0));
        (at, (end - at).max(MIN))
    } else {
        (at0, (dur0 + dt).max(MIN))
    }
}

/// Duration so a title that starts at `at` ends at the reel out point (`total`).
/// Same 0.3s floor as edge resize — a card past the end still gets a hold.
pub fn title_dur_to_end(at: f64, total: f64) -> f64 {
    (total - at).max(0.3)
}

/// Resize media from either edge. `free_at` is true for free-positioned items
/// (V2 overlays): the left edge moves `at` so the right edge stays put. For V1
/// (`free_at` false) only in/out change — timeline position is owned by extents.
/// `dt` is timeline seconds; source in/out move by `dt * speed`.
pub fn media_edge_resize(
    at0: f64,
    in0: f64,
    out0: f64,
    src_dur: f64,
    speed: f64,
    left: bool,
    dt: f64,
    free_at: bool,
) -> (f64, f64, f64) {
    pub const MIN_T: f64 = 0.1;
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

/// Resize a still overlay from either edge. A still is a held, looped frame with
/// no source to trim, so it behaves like a title card: either edge stretches the
/// hold (uncapped) and the in-point stays 0 — the left grip grows the span
/// leftward instead of walling at in=0. Returns `(at, in_s, out_s)`.
pub fn still_edge_resize(at0: f64, in0: f64, out0: f64, speed: f64, left: bool, dt: f64) -> (f64, f64, f64) {
    let sp = speed.max(0.01);
    let span0 = ((out0 - in0) / sp).max(0.0);
    let (at, dur) = title_edge_resize(at0, span0, left, dt);
    (at, 0.0, dur * sp)
}

/// CSS custom props that paint a cheap on-tile mock of a title look — colour,
/// plate, outline, and vertical seat — so the style gallery reads without
/// waiting on ffmpeg rasterize for every preset.
pub fn title_preview_css(t: &TitleItem) -> String {
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
pub fn item_class(base: &str, sel: bool, mark: bool, disabled: bool, solo: bool) -> String {
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
pub enum ClipAppear {
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
    pub const ALL: [ClipAppear; 6] = [
        Self::Wave,
        Self::WaveFilm,
        Self::Equal,
        Self::FilmWave,
        Self::Film,
        Self::Labels,
    ];

    pub fn label(self) -> &'static str {
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
    pub fn glyph(self) -> &'static str {
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
    pub fn base_heights(self) -> (f64, f64) {
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
    pub fn heights(self, height: f64) -> (f64, f64) {
        let m = height.clamp(0.5, 2.0);
        let (f, w) = self.base_heights();
        (f * m, w * m)
    }

    pub fn show_film(self) -> bool {
        !matches!(self, Self::Wave)
    }

    pub fn show_wave(self) -> bool {
        !matches!(self, Self::Film | Self::Labels)
    }

    pub fn labels_only(self) -> bool {
        matches!(self, Self::Labels)
    }
}

/// A cut at source-time `local` is valid only if both halves keep at least
/// `min` seconds; returns the cut point when it is.
pub fn cut_local(in_s: f64, out_s: f64, local: f64, min: f64) -> Option<f64> {
    (local >= in_s + min && local <= out_s - min).then_some(local)
}

/// How long a freeze frame holds by default. Edge-resize stretches it after.
pub const FREEZE_HOLD: f64 = 2.0;

/// Instant replay: source seconds rewound from the playhead, played back at
/// half speed so the beat reads on a short-form reel.
pub const REPLAY_SRC: f64 = 1.5;
pub const REPLAY_SPEED: f64 = 0.5;

/// Two V1 clips can be joined when they are the same source file, same rate and
/// direction, and the right clip's in-point continues the left's out-point —
/// i.e. they were a split (or a freeze was removed) and the cut is still clean.
pub fn can_join_clips(a: &Clip, b: &Clip) -> bool {
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
pub fn replay_span(in_s: f64, out_s: f64, local: f64, src_len: f64) -> Option<(f64, f64)> {
    let local = local.clamp(in_s, out_s);
    let start = (local - src_len).max(in_s);
    if local - start < 0.1 {
        return None;
    }
    Some((start, local))
}

/// Where a freeze frame lands relative to the V1 clip under the playhead.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FreezePlace {
    /// Split the clip at source-time `local`; freeze sits between the halves.
    Split { local: f64 },
    /// Playhead is near the head — freeze goes *before* this clip.
    Before,
    /// Playhead is near the tail (or the clip is tiny) — freeze goes *after*.
    After,
}

/// Decide freeze placement from the clip's source range and the source time
/// under the playhead. `None` if the local time is outside the kept span.
pub fn freeze_place(in_s: f64, out_s: f64, local: f64, min: f64) -> Option<FreezePlace> {
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
pub enum ClipboardItem {
    Main(Clip),
    Over(OverlayItem),
    Aud(AudioItem),
    Title(TitleItem),
}

/// Magnetic timeline: how far an item anchored at `at` moves when a V1 edit
/// rearranges the clips. `old` is each clip's (start, end) span before the
/// edit; `new_start` maps an old clip index to its start after the edit
/// (None = clip deleted). Unattached or orphaned items hold position.
pub fn magnet_delta(at: f64, old: &[(f64, f64)], new_start: impl Fn(usize) -> Option<f64>) -> f64 {
    old.iter()
        .position(|&(s, e)| at >= s && at < e)
        .and_then(|k| new_start(k).map(|ns| ns - old[k].0))
        .unwrap_or(0.0)
}

/// A timeline lane, as a drop target.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    V1,
    /// Overlay track number starting at 2 (V2, V3, …).
    V(u8),
    /// Audio bus starting at 1 (A1, A2, …).
    A(u8),
}

/// What a file is, by extension. An unknown extension is treated as video:
/// `probe` decides for real, and its no-duration fallback catches images in
/// containers these tables have never heard of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Video,
    Still,
    Audio,
}

pub fn kind_of(path: &str) -> Kind {
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
pub fn join_chain(a: String, b: String) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => format!("{a},{b}"),
    }
}

pub fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Where a dropped file actually lands. The lane you drop on says what you
/// meant, but the file has the final say: sound can't go on a video track and
/// a photo has nothing to contribute to an audio one. Returns the lane to use,
/// plus a note when that differs from where it was dropped.
pub fn route_drop(kind: Kind, onto: Lane) -> Result<(Lane, Option<&'static str>), &'static str> {
    match (kind, onto) {
        // Sound is sound — any A-lane accepts it; elsewhere it defaults to A1.
        (Kind::Audio, Lane::A(_)) => Ok((onto, None)),
        (Kind::Audio, _) => Ok((Lane::A(1), Some("audio goes to A1"))),
        // A video dropped on an audio lane contributes its soundtrack.
        (Kind::Video, Lane::A(_)) => Ok((onto, Some("using its soundtrack"))),
        (Kind::Still, Lane::A(_)) => Err("a photo has no sound to mix"),
        (_, lane) => Ok((lane, None)),
    }
}

/// Which index a drop at `t` seconds should insert before on V1. The main track
/// is a concat with no gaps, so a drop can only ever mean "between these two
/// clips" — never "at 12.4s". Past the midpoint of a clip means after it.
pub fn insert_index(clips: &[Clip], t: f64) -> usize {
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
pub fn snap_to(at: f64, targets: &[f64], tol: f64) -> f64 {
    targets
        .iter()
        .copied()
        .filter(|t| (t - at).abs() <= tol)
        .min_by(|a, b| (a - at).abs().total_cmp(&(b - at).abs()))
        .unwrap_or(at)
}

/// Old→new index for a drag-reorder: clips[lo..lo+len] move so the block
/// starts at `dest`, where `dest` indexes the sequence with the block removed.
pub fn block_map(k: usize, lo: usize, len: usize, dest: usize) -> usize {
    if k >= lo && k < lo + len {
        dest + (k - lo)
    } else {
        let w = if k < lo { k } else { k - len };
        if w < dest { w } else { w + len }
    }
}

/// Localhost port the running editor listens on for live coordinate commands from
/// the MCP server. ponytail: fixed port; `MORREEL_LIVE_PORT` overrides on a clash.
pub const LIVE_PORT: u16 = 8177;

pub fn live_port() -> u16 {
    std::env::var("MORREEL_LIVE_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(LIVE_PORT)
}

/// One live command in flight: a plugin call and the channel to answer it on.
pub struct LiveCmd {
    pub plugin: String,
    pub tool: String,
    pub params: serde_json::Value,
    pub reply: tokio::sync::oneshot::Sender<Result<String, String>>,
}

/// Accept newline-delimited JSON commands on localhost and hand each to the UI
/// coroutine, writing its result back on the same connection. A request is
/// `{"plugin","tool","params"}`; a reply is `{"ok": msg}` or `{"error": msg}`.
///
/// One connection at a time — a single editor driven by a single model needs no
/// concurrency, and serial handling keeps every edit ordered on the undo stack.
/// ponytail: loopback only, no auth — any local process can drive the editor,
/// which is fine for a personal tool. Add a shared token the day it isn't.
pub async fn live_server(live: Coroutine<LiveCmd>) {
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
pub enum HubAction {
    Install,
    Uninstall,
    Enable,
    Disable,
}
