// SPDX-License-Identifier: GPL-3.0-or-later
// engine.rs — MorReel's media engine: the ffmpeg/ffprobe CLIs.
// Same split kdenlive (MLT) and openshot (libopenshot) use, minus the C++ library.

use crate::keyframe::{Animated, Interp, Key};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Portrait-only output. Every clip is center-cropped to fill this frame.
pub const W: u32 = 1080;
pub const H: u32 = 1920;
const FPS: u32 = 30;

/// Where the picture sits inside the frame, how big it is, and how it is
/// turned — the Final Cut "Transform" set, which kdenlive calls Position,
/// scale and opacity. Applied after the framing that fills 9:16 and before the
/// look effects, so it composes with both.
// ponytail: no anchor point. It only earns its keep once rotation can be
// keyframed, and nothing here animates yet.
/// The colour that fills the frame wherever a clip doesn't cover it — behind a
/// shrunk or banded clip on V1 (a landscape band with a coloured surround is the
/// reel look). A `Copy` enum, not a `String`, so `Transform` stays `Copy` and
/// every caller that builds one is unaffected.
#[derive(Clone, Copy, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Bg {
    #[default]
    Black,
    White,
    Charcoal,
    Gray,
}

impl Bg {
    pub const ALL: [Bg; 4] = [Bg::Black, Bg::White, Bg::Charcoal, Bg::Gray];
    /// ffmpeg pad colour.
    pub fn color(self) -> &'static str {
        match self {
            Bg::Black => "black",
            Bg::White => "white",
            Bg::Charcoal => "#14141c",
            Bg::Gray => "#8a8a8a",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Bg::Black => "Black",
            Bg::White => "White",
            Bg::Charcoal => "Charcoal",
            Bg::Gray => "Gray",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Transform {
    /// The master size, the way Final Cut's "Scale All" works: 1.0 fills the
    /// frame, 0.5 is half.
    #[serde(default = "unit")]
    pub scale: f64,
    /// Per-axis multipliers on top of `scale` — this is the stretch. Kept
    /// separate from it so an old project, which had only one size, loads as
    /// exactly the shape it was saved in.
    #[serde(default = "unit")]
    pub scale_x: f64,
    #[serde(default = "unit")]
    pub scale_y: f64,
    /// Mirror. Flipping a selfie back the right way round is the usual reason.
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    /// Offset as a fraction of the frame: 0 is centred, 0.5 is half a frame
    /// right (or down). Fractions rather than pixels so a transform survives an
    /// export at a different size.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// Degrees clockwise.
    #[serde(default)]
    pub rotation: f64,
    /// Only means anything where the layer composites over something else — a
    /// V2 cutaway over V1. On V1 there is nothing underneath but the background.
    #[serde(default = "unit")]
    pub opacity: f64,
    /// What fills the frame where the picture doesn't reach (V1 only). Ignored
    /// when the transform covers the whole frame — no padding, nothing to fill.
    #[serde(default)]
    pub bg: Bg,
    /// Fill the scaled box by cover-cropping the picture undistorted instead of
    /// stretching it to fit. What makes a landscape band show real footage rather
    /// than a vertically-squished frame: the box is still `scale_x`×`scale_y`, but
    /// the picture inside keeps its aspect and the overflow is cropped. Off by
    /// default, so a free Stretch (`scale_x` ≠ `scale_y`) still distorts on purpose
    /// and old projects load exactly as saved.
    #[serde(default)]
    pub cover: bool,
}

fn unit() -> f64 {
    1.0
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            scale_x: 1.0,
            scale_y: 1.0,
            flip_h: false,
            flip_v: false,
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            bg: Bg::Black,
            cover: false,
        }
    }
}

/// The animatable form of [`Transform`]: every numeric field is an
/// [`Animated<f64>`]. A still clip stores plain scalars — byte-identical to the
/// old static `Transform` on disk, since a constant serialises as the bare number
/// — and only a field the user keyframes grows into a curve. Flip flags don't
/// animate; a mirror is a cut, not a move.
///
/// The editor works on a single static pose ([`pose`](AnimatedTransform::pose) /
/// [`set_pose`](AnimatedTransform::set_pose)); the engine samples the whole thing
/// to a `Transform` at a given time via [`at`](AnimatedTransform::at). Both
/// preview and export build their chain from a sampled `Transform`, so the
/// preview == export invariant holds field-for-field.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct AnimatedTransform {
    #[serde(default = "anim_one")]
    pub scale: Animated<f64>,
    #[serde(default = "anim_one")]
    pub scale_x: Animated<f64>,
    #[serde(default = "anim_one")]
    pub scale_y: Animated<f64>,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
    #[serde(default = "anim_zero")]
    pub x: Animated<f64>,
    #[serde(default = "anim_zero")]
    pub y: Animated<f64>,
    #[serde(default = "anim_zero")]
    pub rotation: Animated<f64>,
    #[serde(default = "anim_one")]
    pub opacity: Animated<f64>,
    /// Frame background — not animated, a colour is a colour.
    #[serde(default)]
    pub bg: Bg,
    /// Cover-crop the box instead of stretching — not animated, a fit is a fit.
    #[serde(default)]
    pub cover: bool,
}

fn anim_one() -> Animated<f64> {
    Animated::Const(1.0)
}
fn anim_zero() -> Animated<f64> {
    Animated::Const(0.0)
}

impl Default for AnimatedTransform {
    fn default() -> Self {
        Self::from(Transform::default())
    }
}

impl From<Transform> for AnimatedTransform {
    /// A fully static (un-keyframed) transform — every field a constant.
    fn from(p: Transform) -> Self {
        Self {
            scale: Animated::Const(p.scale),
            scale_x: Animated::Const(p.scale_x),
            scale_y: Animated::Const(p.scale_y),
            flip_h: p.flip_h,
            flip_v: p.flip_v,
            x: Animated::Const(p.x),
            y: Animated::Const(p.y),
            rotation: Animated::Const(p.rotation),
            opacity: Animated::Const(p.opacity),
            bg: p.bg,
            cover: p.cover,
        }
    }
}

impl AnimatedTransform {
    /// The static pose at clip-local time `t` — what the engine renders that
    /// frame. This is the single sampling point every chain builder goes through.
    pub fn at(&self, t: f64) -> Transform {
        Transform {
            scale: self.scale.sample(t),
            scale_x: self.scale_x.sample(t),
            scale_y: self.scale_y.sample(t),
            flip_h: self.flip_h,
            flip_v: self.flip_v,
            x: self.x.sample(t),
            y: self.y.sample(t),
            rotation: self.rotation.sample(t),
            opacity: self.opacity.sample(t),
            bg: self.bg,
            cover: self.cover,
        }
    }

    /// The pose the inspector shows and edits: the value at the clip's start.
    pub fn pose(&self) -> Transform {
        self.at(0.0)
    }

    /// Replace every field with a constant — how a slider or on-screen drag edit
    /// lands today.
    ///
    /// ponytail: this flattens any animation. Keyframe-aware editing (insert a key
    /// at the playhead rather than overwrite) supersedes it once the keyframe UI
    /// exists; until then nothing creates a curve, so nothing is lost.
    pub fn set_pose(&mut self, p: Transform) {
        *self = Self::from(p);
    }

    /// The ffmpeg geometry chain, animated where it can be.
    ///
    /// The common, un-keyframed case returns the proven static [`transform_chain`]
    /// **byte-for-byte** — its "crop provably inside the padded picture" safety is
    /// untouched. Only when `scale` is keyframed does this take the animated
    /// branch: a Ken Burns zoom compiled to `zoompan`, the exact mechanism the
    /// Motion presets use, so preview and export animate through the one string.
    ///
    /// ponytail: zoom only goes in (z≥1). A keyframed shrink, non-uniform stretch,
    /// keyframed rotation, flip and opacity aren't expressible in this branch, so
    /// they take the start pose. Generalising them means migrating the whole
    /// geometry onto zoompan and trading away that safety proof — deferred until a
    /// reel actually needs it; presets + this cover the Ken Burns case today.
    pub fn chain(&self, w: u32, h: u32, alpha: bool) -> String {
        if !self.scale.is_animated() {
            return transform_chain(&self.pose(), w, h, alpha);
        }
        let p = self.pose();
        let z = curve_expr(&self.scale, "it");
        let mut c = String::new();
        if alpha {
            c += "format=rgba,";
        }
        if p.flip_h {
            c += "hflip,";
        }
        if p.flip_v {
            c += "vflip,";
        }
        // z clamped ≥1: zoompan can't zoom out past the source frame.
        c += &format!(
            "zoompan=z='max({z},1)':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s={w}x{h}:fps=30"
        );
        if p.rotation.abs() > 1e-6 {
            c += &format!(",rotate={:.5}:c=black", p.rotation.to_radians());
        }
        if alpha && p.opacity < 0.999 {
            c += &format!(",colorchannelmixer=aa={:.3}", p.opacity.clamp(0.0, 1.0));
        }
        c + ",setsar=1"
    }
}

/// Compile an animated parameter to an ffmpeg expression in `var` (e.g. `"it"`,
/// input time in seconds). A constant is just its number; a curve becomes a
/// nested `if()` that clamps to the endpoints and interpolates each segment with
/// its own easing — the same shape the Motion presets hand ffmpeg, built from the
/// user's keyframes.
fn curve_expr(p: &Animated<f64>, var: &str) -> String {
    let keys = match p {
        Animated::Const(v) => return format!("{v:.5}"),
        Animated::Curve(k) => k,
    };
    // Innermost fallback: hold the last value once past the final key.
    let mut expr = format!("{:.5}", keys[keys.len() - 1].v);
    // Fold segments last→first: below b.t use the segment, else what's above it.
    for w in keys.windows(2).rev() {
        expr = format!("if(lt({var},{:.5}),{},{expr})", w[1].t, segment_expr(w[0], w[1], var));
    }
    // Clamp before the first key to its value.
    format!("if(lt({var},{:.5}),{:.5},{expr})", keys[0].t, keys[0].v)
}

/// One segment `a`→`b` as an expression in `var`, eased per `b.interp`.
fn segment_expr(a: Key<f64>, b: Key<f64>, var: &str) -> String {
    let span = (b.t - a.t).max(1e-6);
    let f = format!("(({var}-{:.5})/{:.5})", a.t, span); // 0..1 across the segment
    let d = b.v - a.v;
    match b.interp {
        Interp::Hold => format!("{:.5}", a.v),
        Interp::Linear => format!("({:.5}+{:.5}*{f})", a.v, d),
        Interp::Smooth => format!("({:.5}+{:.5}*({f}*{f}*(3-2*{f})))", a.v, d),
    }
}

impl Transform {
    /// Untouched: emit no filter at all rather than a chain that scales by 1.
    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-6
            && (self.scale_x - 1.0).abs() < 1e-6
            && (self.scale_y - 1.0).abs() < 1e-6
            && !self.flip_h
            && !self.flip_v
            && self.x.abs() < 1e-6
            && self.y.abs() < 1e-6
            && self.rotation.abs() < 1e-6
            && (self.opacity - 1.0).abs() < 1e-6
    }
}

/// ffmpeg chain for a transform, or "" when it is the identity.
///
/// `alpha` builds it for a layer that composites over something else: the area
/// the picture no longer covers has to be transparent, not black, or a
/// scaled-down V2 cutaway would black out the V1 clip it is supposed to sit on
/// top of. That is what makes picture-in-picture work.
pub fn transform_chain(t: &Transform, w: u32, h: u32, alpha: bool) -> String {
    if t.is_identity() {
        return String::new();
    }
    // Every dimension is computed here rather than as an ffmpeg expression, so
    // the crop window is provably inside the padded picture.
    let even = |v: f64| ((v.round().max(2.0)) as u32) & !1u32;
    // Master size times the per-axis stretch, so "Scale" still moves both.
    let sw = even(w as f64 * t.scale * t.scale_x.max(0.01));
    let sh = even(h as f64 * t.scale * t.scale_y.max(0.01));
    let dx = (t.x * w as f64).round() as i64;
    let dy = (t.y * h as f64).round() as i64;
    // Pad out to whatever the offset crop needs, so the picture can be moved
    // clean off the edge of the frame instead of jamming against it.
    let pw = even((sw as f64).max(w as f64 + 2.0 * dx.abs() as f64)).max(sw).max(w);
    let ph = even((sh as f64).max(h as f64 + 2.0 * dy.abs() as f64)).max(sh).max(h);
    // A composited layer (V2) pads transparent; V1 pads with the chosen colour,
    // which is what shows behind a banded or shrunk clip.
    let fill = if alpha { "black@0" } else { t.bg.color() };

    let mut c = String::new();
    if alpha {
        c += "format=rgba,";
    }
    // Mirror before anything else, so a flip does not fight the rotation.
    if t.flip_h {
        c += "hflip,";
    }
    if t.flip_v {
        c += "vflip,";
    }
    // Fill the sw×sh box: cover-crop keeps the picture's aspect (a real
    // landscape band), a plain scale stretches it to fit (the Stretch sliders).
    // `increase` scales until the box is covered, then the crop takes the centre —
    // the scaled picture is ≥ the box on both axes, so the crop is provably inside.
    if t.cover {
        c += &format!("scale={sw}:{sh}:force_original_aspect_ratio=increase,crop={sw}:{sh}");
    } else {
        c += &format!("scale={sw}:{sh}");
    }
    if t.rotation.abs() > 1e-6 {
        // rotate keeps the input size, so the corners it opens up take the fill.
        c += &format!(",rotate={:.5}:c={fill}", t.rotation.to_radians());
    }
    c += &format!(",pad={pw}:{ph}:{}:{}:color={fill}", (pw - sw) / 2, (ph - sh) / 2);
    // Positive x moves the picture right, so the window it is seen through
    // moves left by the same amount.
    c += &format!(
        ",crop={w}:{h}:{}:{}",
        (pw as i64 - w as i64) / 2 - dx,
        (ph as i64 - h as i64) / 2 - dy
    );
    if alpha && t.opacity < 0.999 {
        c += &format!(",colorchannelmixer=aa={:.3}", t.opacity.clamp(0.0, 1.0));
    }
    c + ",setsar=1"
}

/// A rectangle to align against, in the same normalized frame space as
/// [`Transform::x`]/[`y`]: the frame spans `[-0.5, 0.5]` on each axis, `(0,0)`
/// is dead centre, `+x` is right and `+y` is down. So an offset computed here
/// drops straight into a pose.
///
/// GIMP's align tool calls this the "reference" — the thing targets snap to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AlignBox {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

impl AlignBox {
    /// The whole 9:16 frame.
    pub const FRAME: AlignBox = AlignBox { left: -0.5, right: 0.5, top: -0.5, bottom: 0.5 };
    /// Inside the phone-UI safe areas — the exact insets the on-screen guides
    /// draw (header 8% top, caption 24% bottom, action rail 18% right, nothing
    /// on the left). Aligning here parks a band clear of the chrome that every
    /// platform paints over a reel.
    // ponytail: these four numbers are the .mr-safe-* CSS heights. If the guide
    // geometry ever moves, move it here too — one reel, two places, on purpose:
    // the guides are DOM, this is frame maths, and neither imports the other.
    pub const SAFE: AlignBox =
        AlignBox { left: -0.5, right: 0.5 - 0.18, top: -0.5 + 0.08, bottom: 0.5 - 0.24 };
}

/// GIMP's six align operations. A horizontal one moves only `x`, a vertical one
/// only `y`, so a Left then a Top compose into a corner. No distribute: this app
/// layers elements rather than tiling three-plus of them, so there is nothing to
/// space out — add it the day a reel holds a row of independent boxes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    Left,
    HCenter,
    Right,
    Top,
    VCenter,
    Bottom,
}

impl Transform {
    /// Snap this transform's placement to `r`, GIMP-style: the element's edge —
    /// or centre — meets the reference's. Only the axis the op acts on changes;
    /// scale, the other axis, rotation and the rest stay put.
    ///
    /// The on-frame picture is `scale·scale_x` wide and `scale·scale_y` tall
    /// (see [`transform_chain`]), so half of each is its reach from centre.
    /// Because the offset comes from the *actual* box, a band lands flush at any
    /// height — which is the whole reason this beats a fixed `y` preset that is
    /// only correct at one `scale_y`.
    pub fn align_to(&mut self, op: Align, r: AlignBox) {
        let hw = (self.scale * self.scale_x).abs() / 2.0;
        let hh = (self.scale * self.scale_y).abs() / 2.0;
        match op {
            Align::Left => self.x = r.left + hw,
            Align::HCenter => self.x = (r.left + r.right) / 2.0,
            Align::Right => self.x = r.right - hw,
            Align::Top => self.y = r.top + hh,
            Align::VCenter => self.y = (r.top + r.bottom) / 2.0,
            Align::Bottom => self.y = r.bottom - hh,
        }
    }
}

/// What the file dialogs offer, and the single source of truth for what counts
/// as what. These are only a convenience filter — ffmpeg reads far more than
/// this, which is why every dialog also offers "All files": if ffprobe can
/// open it, the editor can use it.
pub const VIDEO_EXT: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "m4v", "avi", "gif", "mpg", "mpeg", "mpe", "ts", "mts", "m2ts",
    "flv", "wmv", "asf", "3gp", "3g2", "ogv", "vob", "mxf", "divx", "f4v", "rm", "rmvb", "y4m",
];

/// Anything ffmpeg decodes to a single frame. These take the `-loop 1` path.
pub const IMAGE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "webp", "bmp", "tif", "tiff", "avif", "heic", "heif", "tga",
    "ppm", "pgm", "pbm", "pnm", "dds", "ico", "jp2", "j2k", "exr", "hdr", "qoi",
];

pub const AUDIO_EXT: &[&str] = &[
    "mp3", "m4a", "m4b", "aac", "wav", "flac", "ogg", "oga", "opus", "wma", "aiff", "aif", "aifc",
    "alac", "mka", "ac3", "eac3", "dts", "amr", "au", "caf", "mp2", "wv", "ape",
];

/// A still photo on the timeline. ffmpeg gets `-loop 1` for these, so the one
/// frame becomes a stream the trim can bound and the Motion effects (moranima's
/// camera moves) have real timestamps to animate against — a photo with Drift
/// or Pulse zoom is the whole point of putting one on a reel.
///
/// GIF is deliberately *not* here: an animated one is a video, and a static one
/// still probes with a duration, so the normal path handles both.
pub fn is_still(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| IMAGE_EXT.contains(&e.as_str()))
}

/// A still has no duration of its own. `STILL_SOURCE` is the nominal source
/// length — how far the Out point can stretch it — and `STILL_DEFAULT` is what
/// a freshly imported one occupies.
pub const STILL_SOURCE: f64 = 60.0;
pub const STILL_DEFAULT: f64 = 5.0;

/// Colour emoji faces are bitmap strikes rather than outlines: drawtext cannot
/// set a pixel size on them and fails the entire render with "invalid library
/// handle". Keep them out of the picker rather than offering a font that can
/// only break.
// ponytail: matched by name, which is crude but catches every colour emoji
// face shipped by the usual distributions. Probing each of 600 fonts with a
// trial render would cost seconds of startup to catch a handful more.
fn font_is_unusable(family: &str) -> bool {
    family.to_ascii_lowercase().contains("emoji")
}

/// Every font family fontconfig can see, for the title picker. The three
/// generics stay pinned at the top because they resolve on any machine — a
/// project naming "Impact" opens fine on a box that has never had it, but the
/// card renders in whatever fontconfig substitutes.
///
/// Enumerated once: fonts do not appear mid-session and spawning `fc-list` per
/// render would be silly.
pub fn font_families() -> &'static [String] {
    static FONTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let mut out = vec!["Sans".to_string(), "Serif".to_string(), "Mono".to_string()];
        let listed = std::process::Command::new("fc-list").args([":", "family"]).output();
        if let Ok(listed) = listed {
            let mut families: Vec<String> = String::from_utf8_lossy(&listed.stdout)
                .lines()
                .flat_map(|line| line.split(','))
                .map(|f| f.trim().to_string())
                .filter(|f| !f.is_empty() && !font_is_unusable(f))
                .collect();
            families.sort_by_key(|f| f.to_ascii_lowercase());
            families.dedup();
            out.extend(families);
        }
        out.dedup();
        out
    })
}

/// What a T-lane card actually is. Shapes ride the title lane because they
/// need exactly what a title needs — a rasterized card, a place on the
/// timeline, a fade — and nothing a title does not.
pub const TITLE_KINDS: &[&str] = &["Text", "Box", "Ellipse", "Line"];

/// Numeric colour for geq, which cannot take the names drawtext accepts.
fn rgb_of(color: &str) -> (u32, u32, u32) {
    match color {
        "black" => (0, 0, 0),
        hex if hex.len() == 7 && hex.starts_with('#') => {
            let byte = |at: usize| u32::from_str_radix(&hex[at..at + 2], 16).unwrap_or(255);
            (byte(1), byte(3), byte(5))
        }
        _ => (255, 255, 255),
    }
}

/// A shape, drawn straight into the alpha plane.
///
/// It has to be geq: drawbox writes colour but leaves alpha alone, so on the
/// transparent canvas a title card is rasterized onto, its box comes out fully
/// coloured and completely invisible.
fn shape_chain(s: &TitleStyle, w: u32, h: u32) -> String {
    let (r, g, b) = rgb_of(&s.color);
    let (fw, fh) = (w as f64, h as f64);
    let cx = fw * (0.5 + s.shape_x);
    let cy = fh * s.y_frac.clamp(0.0, 1.0);
    let hw = (fw * s.shape_w / 2.0).max(1.0);
    let hh = (fh * s.shape_h / 2.0).max(1.0);
    let covers = |hw: f64, hh: f64| match s.kind.as_str() {
        "Ellipse" => format!("lte(pow((X-{cx:.1})/{hw:.1},2)+pow((Y-{cy:.1})/{hh:.1},2),1)"),
        _ => format!(
            "between(X,{:.1},{:.1})*between(Y,{:.1},{:.1})",
            cx - hw,
            cx + hw,
            cy - hh,
            cy + hh
        ),
    };
    // An outline thickness turns a solid shape into a ring — inside the outer
    // edge but not inside the inner one. A line is always solid.
    let t = s.outline.max(0.0);
    let alpha = if t > 0.0 && s.kind != "Line" && hw > t && hh > t {
        format!("if({}*(1-{}),255,0)", covers(hw, hh), covers(hw - t, hh - t))
    } else {
        format!("if({},255,0)", covers(hw, hh))
    };
    format!("format=rgba,geq=r='{r}':g='{g}':b='{b}':a='{alpha}'")
}

/// How multi-line title text lines up within its block.
pub const ALIGNMENTS: &[(&str, &str)] = &[
    ("Centre", "center"),
    ("Left", "left"),
    ("Right", "right"),
];

pub fn align_flag(label: &str) -> &'static str {
    ALIGNMENTS.iter().find(|(l, _)| *l == label).map_or("center", |(_, f)| f)
}

/// Transitions, as (what the menu says, what xfade calls it). A transition
/// belongs to the clip it leads *into*, so it survives reordering and deleting
/// the clip before it.
pub const TRANSITIONS: &[(&str, &str)] = &[
    ("None", ""),
    ("Cross dissolve", "fade"),
    ("Dip to black", "fadeblack"),
    ("Dip to white", "fadewhite"),
    ("Slide", "slideleft"),
    ("Wipe", "wiperight"),
    ("Circle", "circleopen"),
    ("Dissolve", "dissolve"),
];

/// The xfade name for a menu label; "" for None or anything unrecognised.
pub fn xfade_name(label: &str) -> &'static str {
    TRANSITIONS.iter().find(|(l, _)| *l == label).map_or("", |(_, x)| x)
}

#[derive(Clone, PartialEq, Debug)]
pub struct ClipSpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub has_audio: bool,
    /// ffmpeg filter snippet appended to the video chain; empty = no effect.
    pub effect: String,
    /// How the source fills 9:16: "Crop" (default), "Fit" (letterbox), "Zoom".
    pub framing: String,
    /// Playback rate: 0.5 is half speed (slow motion), 2.0 is double.
    pub speed: f64,
    /// Gain on this clip's own audio; 0.0 mutes it.
    pub volume: f64,
    /// Transition *into* this clip, by menu label. Ignored on the first clip —
    /// there is nothing before it to blend from.
    pub transition: String,
    /// How long that transition runs. 0 means a straight cut.
    pub trans_dur: f64,
}

/// Hand-written so `..Default::default()` can never leave `speed` at 0.0 and
/// divide the timeline by zero.
impl Default for ClipSpec {
    fn default() -> Self {
        Self {
            path: String::new(),
            in_s: 0.0,
            out_s: 0.0,
            has_audio: false,
            effect: String::new(),
            framing: String::new(),
            speed: 1.0,
            volume: 1.0,
            transition: String::new(),
            trans_dur: 0.0,
        }
    }
}

impl ClipSpec {
    /// How long this clip runs for — source span divided by the speed, so a
    /// 4 s clip at 2× runs 2 s.
    pub fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }

    /// The transition leading into this clip, in seconds, once it has been
    /// clamped to something the clip can actually accommodate. 0 = a cut.
    pub fn fade_in(&self, prev: Option<&ClipSpec>) -> f64 {
        let Some(prev) = prev else { return 0.0 }; // nothing before the first clip
        if xfade_name(&self.transition).is_empty() {
            return 0.0;
        }
        // A transition cannot outlast either clip, or xfade has nothing left to
        // blend and the offset maths goes negative.
        self.trans_dur.clamp(0.0, self.trimmed().min(prev.trimmed()) * 0.9)
    }
}

/// How much of the timeline each clip owns. A transition overlaps a clip's tail
/// with the next clip's head, so a clip followed by one owns less of the
/// timeline than it runs for. These sum to the finished length.
pub fn extents(clips: &[ClipSpec]) -> Vec<f64> {
    (0..clips.len())
        .map(|i| {
            let after = clips.get(i + 1).map_or(0.0, |n| n.fade_in(Some(&clips[i])));
            (clips[i].trimmed() - after).max(0.05)
        })
        .collect()
}

/// The finished length of the timeline, transitions accounted for.
pub fn timeline_len(clips: &[ClipSpec]) -> f64 {
    extents(clips).iter().sum()
}

/// `atempo` only accepts 0.5–2.0 per instance, so anything outside that range
/// becomes a chain of halvings/doublings with the remainder on the end.
/// Empty string when the speed is 1× (no filter needed).
fn atempo_chain(speed: f64) -> String {
    let mut s = speed.clamp(0.05, 20.0);
    let mut parts: Vec<String> = Vec::new();
    while s > 2.0 {
        parts.push("atempo=2.0".into());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".into());
        s *= 2.0;
    }
    if (s - 1.0).abs() > 1e-6 {
        parts.push(format!("atempo={s:.4}"));
    }
    parts.join(",")
}

/// Output container and codec.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Mp4,
    WebM,
    Gif,
}

impl Format {
    pub const ALL: &'static [Format] = &[Format::Mp4, Format::WebM, Format::Gif];

    pub fn label(self) -> &'static str {
        match self {
            Format::Mp4 => "MP4 · H.264",
            Format::WebM => "WebM · VP9",
            Format::Gif => "Animated GIF",
        }
    }

    /// One line on what it is good for, so the picker doesn't need a manual.
    pub fn blurb(self) -> &'static str {
        match self {
            Format::Mp4 => "Plays everywhere. The one to upload.",
            Format::WebM => "Smaller at the same quality, but not every phone plays it.",
            Format::Gif => "Loops forever, silent, limited colours. For stickers and memes.",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::WebM => "webm",
            Format::Gif => "gif",
        }
    }

    /// GIF has no audio track at all — the mix is discarded, not muted.
    pub fn has_audio(self) -> bool {
        self != Format::Gif
    }

    pub fn from_label(label: &str) -> Format {
        Format::ALL.iter().copied().find(|f| f.label() == label).unwrap_or(Format::Mp4)
    }
}

/// The quality/time trade-off. Draft is also what the in-app full preview
/// renders at, where getting a file to play is the entire point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Draft,
    Balanced,
    High,
}

impl Quality {
    pub const ALL: &'static [Quality] = &[Quality::Draft, Quality::Balanced, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft — fastest",
            Quality::Balanced => "Balanced",
            Quality::High => "High — slowest",
        }
    }

    pub fn from_label(label: &str) -> Quality {
        Quality::ALL.iter().copied().find(|q| q.label() == label).unwrap_or(Quality::Balanced)
    }

    /// (speed knob, crf) for a codec. x264 takes a preset name; libvpx-vp9
    /// takes a -cpu-used number, and its crf scale is coarser and higher.
    fn encode(self, format: Format) -> (&'static str, u32) {
        match (format, self) {
            (Format::WebM, Quality::Draft) => ("5", 40),
            (Format::WebM, Quality::Balanced) => ("3", 33),
            (Format::WebM, Quality::High) => ("1", 28),
            (_, Quality::Draft) => ("ultrafast", 32),
            (_, Quality::Balanced) => ("veryfast", 23),
            (_, Quality::High) => ("medium", 18),
        }
    }
}

/// Portrait sizes worth offering. The edit is always composed at 1080×1920 —
/// these scale once at the end of the graph.
pub const SIZES: &[(&str, u32, u32)] = &[
    ("1080 × 1920 (full)", 1080, 1920),
    ("720 × 1280 (smaller file)", 720, 1280),
    ("540 × 960 (draft)", 540, 960),
];

pub fn size_label(w: u32) -> String {
    SIZES.iter().find(|(_, sw, _)| *sw == w).map_or(SIZES[0].0, |(l, _, _)| l).to_string()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ExportOpts {
    pub format: Format,
    pub quality: Quality,
    pub width: u32,
    pub height: u32,
}

impl Default for ExportOpts {
    fn default() -> Self {
        Self { format: Format::Mp4, quality: Quality::Balanced, width: W, height: H }
    }
}

impl ExportOpts {
    /// The in-app "full preview" render: fastest thing that still plays, at
    /// half size, because it goes straight to a player and then to the bin.
    pub fn preview() -> Self {
        Self { format: Format::Mp4, quality: Quality::Draft, width: W / 2, height: H / 2 }
    }

    pub fn with_size(mut self, w: u32) -> Self {
        let (_, w, h) = SIZES.iter().copied().find(|(_, sw, _)| *sw == w).unwrap_or(SIZES[0]);
        (self.width, self.height) = (w, h);
        self
    }
}

/// V2: full-frame cutaway laid over the main track at global time `at`.
/// Main-track audio keeps playing underneath (classic B-roll).
#[derive(Clone, PartialEq, Debug)]
pub struct OverlaySpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    pub effect: String,
    pub framing: String,
    /// Playback rate, same as a V1 clip: 0.5 is slow motion.
    pub speed: f64,
}

/// Hand-written for the same reason ClipSpec's is: a defaulted 0.0 rate would
/// divide the cutaway's length by zero.
impl Default for OverlaySpec {
    fn default() -> Self {
        Self {
            path: String::new(),
            in_s: 0.0,
            out_s: 0.0,
            at: 0.0,
            effect: String::new(),
            framing: String::new(),
            speed: 1.0,
        }
    }
}

impl OverlaySpec {
    /// Seconds this cutaway covers V1 for — its source span, retimed.
    pub fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }
}

/// Named EQ / voice treatments applied before gain and fades. Plain English
/// labels; the chains are fixed so a preset always sounds the same.
pub const AUDIO_TREATS: &[&str] = &[
    "None",
    "Voice enhance",
    "Warm",
    "Bright",
    "Bass cut",
    "Podcast",
];

/// ffmpeg filter chain for a treatment label (no leading/trailing commas).
pub fn audio_treat_chain(name: &str) -> &'static str {
    match name {
        // High-pass rumble, lift presence, dip a bit of mud.
        "Voice enhance" => {
            "highpass=f=80,equalizer=f=3000:t=q:w=1.2:g=3.5,equalizer=f=220:t=q:w=1:g=-2.5"
        }
        "Warm" => "equalizer=f=140:t=q:w=1:g=3,equalizer=f=4500:t=q:w=1:g=-2",
        "Bright" => "treble=g=4,equalizer=f=180:t=q:w=1:g=-1.5",
        "Bass cut" => "highpass=f=140",
        // Broadcast-ish: clean lows, slight presence, gentle glue.
        "Podcast" => {
            "highpass=f=80,equalizer=f=2500:t=q:w=1:g=2,\
             acompressor=threshold=-20dB:ratio=3:attack=8:release=80:makeup=2"
        }
        _ => "",
    }
}

/// A1/A2: audio mixed under the main track starting at global time `at`.
#[derive(Clone, PartialEq, Debug)]
pub struct AudioSpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    /// Start gain (linear, 1.0 = unity). With `vol_end` this is also the start
    /// of a linear volume automation ramp across the item.
    pub volume: f64,
    /// End gain for volume automation. Negative means "same as `volume`" so
    /// older projects without the field stay flat.
    pub vol_end: f64,
    /// How hard to pull this bed down while the main track is talking.
    /// 0 = never; 1 = duck hard. The classic music-under-voiceover move.
    pub duck: f64,
    /// Fade in from silence at the start of the kept span (seconds).
    pub fade_in: f64,
    /// Fade out to silence at the end of the kept span (seconds).
    pub fade_out: f64,
    /// Spectral denoise strength 0..=1 (`afftdn`).
    pub denoise: f64,
    /// Broadband compression amount 0..=1 (skipped when the treatment already
    /// bakes a compressor, e.g. Podcast).
    pub compress: f64,
    /// One of [`AUDIO_TREATS`].
    pub treat: String,
    /// Mix bus: 1 = A1, 2 = A2. Both mix under V1; the split is editorial.
    pub lane: u8,
}

impl Default for AudioSpec {
    fn default() -> Self {
        Self {
            path: String::new(),
            in_s: 0.0,
            out_s: 0.0,
            at: 0.0,
            volume: 1.0,
            vol_end: -1.0,
            duck: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            denoise: 0.0,
            compress: 0.0,
            treat: "None".into(),
            lane: 1,
        }
    }
}

impl AudioSpec {
    /// Effective end gain (resolves the "same as volume" sentinel).
    pub fn end_gain(&self) -> f64 {
        if self.vol_end < 0.0 {
            self.volume
        } else {
            self.vol_end
        }
    }

    /// Kept duration in source seconds.
    pub fn span(&self) -> f64 {
        (self.out_s - self.in_s).max(0.01)
    }
}

/// A sidechain compressor keyed off the main track. The threshold decides how
/// much speech it takes to trigger and the ratio how far it pulls; attack and
/// release are pinned at values that catch a word without audibly pumping
/// between them.
// ponytail: two knobs behind one "amount" — expose the rest if a voice ever
// needs different tuning.
fn duck_chain(amount: f64) -> String {
    let a = amount.clamp(0.0, 1.0);
    format!(
        "sidechaincompress=threshold={:.4}:ratio={:.2}:attack=20:release=300",
        (0.1 - 0.085 * a).max(0.01),
        2.0 + 10.0 * a
    )
}

/// Denoise → treat → compress fragment (no volume/fades/delay).
fn audio_process_chain(a: &AudioSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    let d = a.denoise.clamp(0.0, 1.0);
    if d > 0.001 {
        // afftdn: nr is noise reduction in dB (gentle → aggressive).
        parts.push(format!("afftdn=nr={:.1}:nf=-25", 4.0 + 20.0 * d));
    }
    let treat = audio_treat_chain(&a.treat);
    if !treat.is_empty() {
        parts.push(treat.to_string());
    }
    // Podcast already glues; stacking another compressor muddies it.
    let c = a.compress.clamp(0.0, 1.0);
    if c > 0.001 && a.treat != "Podcast" {
        parts.push(format!(
            "acompressor=threshold={:.1}dB:ratio={:.2}:attack=12:release=120:makeup={:.1}",
            -12.0 - 12.0 * c,
            1.5 + 6.0 * c,
            1.0 + 3.0 * c
        ));
    }
    parts.join(",")
}

/// Volume (flat or linear ramp) + optional fades over the kept span.
fn audio_gain_fades(a: &AudioSpec) -> String {
    let dur = a.span();
    let v0 = a.volume.max(0.0);
    let v1 = a.end_gain().max(0.0);
    let mut parts: Vec<String> = Vec::new();
    if (v0 - v1).abs() < 0.001 {
        parts.push(format!("volume={v0:.3}"));
    } else {
        // Frame-eval linear automation from start gain to end gain.
        parts.push(format!(
            "volume='{v0:.4}+({v1:.4}-{v0:.4})*t/{dur:.4}':eval=frame"
        ));
    }
    let fin = a.fade_in.clamp(0.0, dur * 0.49);
    let fout = a.fade_out.clamp(0.0, dur * 0.49);
    if fin > 0.001 {
        parts.push(format!("afade=t=in:st=0:d={fin:.3}:curve=tri"));
    }
    if fout > 0.001 {
        parts.push(format!(
            "afade=t=out:st={:.3}:d={fout:.3}:curve=tri",
            (dur - fout).max(0.0)
        ));
    }
    parts.join(",")
}

/// Mix the A1 beds under `main`, ducking the ones that ask for it, ending at
/// `[out]`. Shared by the export graph and the playback mix so what you hear
/// while scrubbing is what gets rendered.
fn mix_audio(main: &str, audio: &[AudioSpec], out: &str) -> String {
    if audio.is_empty() {
        return format!("[{main}]anull[{out}]");
    }
    let ducked: Vec<usize> =
        (0..audio.len()).filter(|k| audio[*k].duck > 0.0).collect();
    let mut f = String::new();
    // Every ducked bed needs its own copy of the main track to key from, and
    // the mix still needs one, so the split has one output more than there are
    // ducked beds.
    let head = if ducked.is_empty() {
        main.to_string()
    } else {
        f += &format!("[{main}]asplit={}[amain]", ducked.len() + 1);
        for k in &ducked {
            f += &format!("[akey{k}]");
        }
        f += ";";
        for k in &ducked {
            f += &format!("[au{k}][akey{k}]{}[au{k}d];", duck_chain(audio[*k].duck));
        }
        "amain".to_string()
    };
    f += &format!("[{head}]");
    for k in 0..audio.len() {
        f += &if ducked.contains(&k) { format!("[au{k}d]") } else { format!("[au{k}]") };
    }
    f += &format!("amix=inputs={}:duration=first:normalize=0[{out}]", audio.len() + 1);
    f
}

/// How a title card arrives and leaves. Kinetic text is most of what a reel
/// title *is*, and the card is composited with `overlay`, whose x and y accept
/// time expressions — so sliding one on costs nothing but arithmetic.
// ponytail: no scale-based pop. overlay can move a card but not resize it, and
// scale takes no per-frame expression; that one needs the card re-rasterized
// per frame or a real keyframe system.
pub const TITLE_ANIMS: &[&str] =
    &["None", "Slide up", "Slide down", "Slide in left", "Slide in right"];

/// T: a pre-rendered 1080×1920 transparent PNG shown from `at` for `dur`.
#[derive(Clone, PartialEq, Debug)]
pub struct TitleSpec {
    pub png: String,
    pub at: f64,
    pub dur: f64,
    /// One of TITLE_ANIMS. Anything unrecognised sits still.
    pub anim: String,
    /// A word-by-word reveal is a run of cards back to back. Only the first
    /// fades up and only the last fades out, or every word would pulse.
    pub fade_in: bool,
    pub fade_out: bool,
}

impl Default for TitleSpec {
    fn default() -> Self {
        Self {
            png: String::new(),
            at: 0.0,
            dur: 0.0,
            anim: String::new(),
            fade_in: true,
            fade_out: true,
        }
    }
}

impl TitleSpec {
    /// How long the card takes to arrive, and to leave again. Tied to the
    /// alpha fade so the movement and the fade finish together rather than
    /// fighting each other.
    fn anim_len(&self) -> f64 {
        title_fade(self.dur)
    }

    /// `overlay` x/y options that carry the card on and off, trailing colon
    /// included so they splice in front of the next option. Empty when the card
    /// should just sit where it was rasterized.
    ///
    /// `p` runs 0..1 while arriving and 1..0 while leaving; the offset is eased
    /// with p*p so it decelerates into place instead of arriving at full tilt.
    fn overlay_xy(&self) -> String {
        let travel = match self.anim.as_str() {
            "Slide up" => (0.0, H as f64 * 0.25),
            "Slide down" => (0.0, -(H as f64) * 0.25),
            "Slide in left" => (-(W as f64) * 0.5, 0.0),
            "Slide in right" => (W as f64 * 0.5, 0.0),
            _ => return String::new(),
        };
        let (a, d, len) = (self.at, self.dur, self.anim_len().max(0.01));
        // 1 while off-position, 0 once settled. The two ramps are "how much
        // arrival is left" and "how much departure has begun"; only one is
        // positive at a time, so it is their max — a min would clamp to 0 for
        // the whole card and nothing would ever move.
        let away = format!(
            "pow(clip(max(({a:.3}+{len:.3}-t)/{len:.3},(t-({a:.3}+{d:.3}-{len:.3}))/{len:.3}),0,1),2)"
        );
        let axis = |v: f64| {
            if v.abs() < 0.01 { "0".to_string() } else { format!("{v:.1}*{away}") }
        };
        format!("x='{}':y='{}':", axis(travel.0), axis(travel.1))
    }
}

/// Alpha fade length for a title of `dur` seconds — one definition feeds both
/// the export graph and the scrub preview's approximation.
pub fn title_fade(dur: f64) -> f64 {
    (dur / 3.0).min(0.3)
}

/// Everything that decides what a title card looks like. One struct so the
/// content-addressed cache can hash it wholesale and adding a knob doesn't grow
/// `render_title`'s argument list.
///
/// The bevel half mirrors the control set (and the defaults) of
/// wearable-dictionary-designer, which is where this bevel came from — there
/// only `size` and the raised/sunken flip were ever exposed here, and the rest
/// of the look was nailed shut at values nobody chose.
#[derive(Clone, PartialEq, Debug)]
pub struct TitleStyle {
    pub text: String,
    pub font_size: u32,
    pub color: String,
    /// Vertical placement as a fraction of the free space; 0 = top.
    pub y_frac: f64,
    /// fontconfig family; "" = drawtext default. Any installed family works,
    /// not just the generics.
    pub font: String,
    /// How multiple lines line up against each other: "Centre" | "Left" | "Right".
    pub align: String,
    /// Outline width in px, 0 = none. Carries legibility over busy video
    /// without the opaque plate a backdrop box needs.
    pub outline: f64,
    pub outline_color: String,
    /// Semi-opaque backdrop box behind the text.
    pub boxed: bool,
    /// One of TITLE_KINDS. "Text" draws words; the rest draw a shape and
    /// ignore everything about type.
    pub kind: String,
    /// Shape size and horizontal offset, as fractions of the frame. Vertical
    /// placement reuses `y_frac`, the same control a title uses.
    pub shape_w: f64,
    pub shape_h: f64,
    pub shape_x: f64,
    /// "Off" | "Cameo" (raised) | "Intaglio" (sunken).
    pub bevel: String,
    pub bevel_size: f64,
    pub soften: f64,
    pub depth: f64,
    pub angle: f64,
    pub altitude: f64,
    pub hi_opacity: f64,
    pub sh_opacity: f64,
}

impl Default for TitleStyle {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_size: 110,
            color: "white".into(),
            y_frac: 0.45,
            font: "Sans".into(),
            align: "Centre".into(),
            outline: 0.0,
            outline_color: "black".into(),
            boxed: false,
            kind: "Text".into(),
            shape_w: 0.6,
            shape_h: 0.12,
            shape_x: 0.0,
            bevel: "Off".into(),
            // The designer app's own bevel defaults, so a card struck here and
            // a card struck there look like the same tool made them.
            bevel_size: 21.0,
            // The designer app defaults this to 0, which is right for the traced
            // artwork it bevels. drawtext glyphs have antialiased diagonals that
            // stair-step in the distance transform, and at 0 the relief combs
            // into visible hatching along them; 4 is where that disappears.
            soften: 4.0,
            depth: 100.0,
            angle: 120.0,
            altitude: 30.0,
            hi_opacity: 0.75,
            sh_opacity: 0.75,
        }
    }
}

/// Hand-written because f64 isn't Hash: the float knobs go in by bit pattern,
/// which is exactly the identity the cache wants.
impl std::hash::Hash for TitleStyle {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.text.hash(h);
        self.font_size.hash(h);
        self.color.hash(h);
        self.font.hash(h);
        self.align.hash(h);
        self.outline_color.hash(h);
        self.boxed.hash(h);
        self.kind.hash(h);
        self.bevel.hash(h);
        for f in [
            self.y_frac,
            self.outline,
            self.bevel_size,
            self.soften,
            self.depth,
            self.angle,
            self.altitude,
            self.hi_opacity,
            self.sh_opacity,
            self.shape_w,
            self.shape_h,
            self.shape_x,
        ] {
            f.to_bits().hash(h);
        }
    }
}

/// Rasterize a title card: ffmpeg drawtext onto a transparent canvas, then an
/// optional cameo/intaglio bevel (the mor_cameo_emboss algorithm) baked in.
/// Content-addressed in the cache, so identical params never re-render.
pub async fn render_title(s: &TitleStyle) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    // Bump when the rendering changes, so cards cached by an older build are
    // re-rendered instead of served stale. v5: adds shapes.
    const CACHE_VER: u32 = 5;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    CACHE_VER.hash(&mut h);
    s.hash(&mut h);
    let dir = cache_dir("titles");
    let png = dir.join(format!("{:016x}.png", h.finish()));
    if png.exists() {
        return Ok(png.display().to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // A shape needs none of the type machinery below it.
    let shape = s.kind != "Text" && !s.kind.is_empty();
    if shape {
        let out = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black@0.0:s={W}x{H},format=rgba"))
            .args(["-vf", &shape_chain(s, W, H), "-frames:v", "1", "-pix_fmt", "rgba"])
            .arg(&png)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        // Falls through to the bevel below, so a shape can be embossed too.
        return finish_title(&png, s);
    }

    // textfile= sidesteps drawtext's escaping rules entirely.
    let txt = png.with_extension("txt");
    // A literal \n in the text box is a line break. The kit's text input is a
    // single line, so this is the only way to type one, and drawtext reads the
    // file verbatim.
    std::fs::write(&txt, s.text.replace("\\n", "\n")).map_err(|e| e.to_string())?;
    // Anything that already carries legibility — a backdrop box, an outline,
    // the bevel's own relief — makes the drop shadow redundant.
    let plain = s.bevel == "Off" && !s.boxed && s.outline <= 0.0;
    let shadow = if plain { ":shadowcolor=black@0.5:shadowx=3:shadowy=3" } else { "" };
    let boxp = if s.boxed { ":box=1:boxcolor=black@0.45:boxborderw=18" } else { "" };
    let border = if s.outline > 0.0 {
        format!(":borderw={:.0}:bordercolor={}", s.outline, s.outline_color)
    } else {
        String::new()
    };
    let fontp = if s.font.is_empty() { String::new() } else { format!(":font='{}'", s.font) };
    let vf = format!(
        "drawtext=textfile={}{fontp}:fontsize={}:fontcolor={}:text_align={}\
         :x=(w-text_w)/2:y=(h-text_h)*{:.3}{shadow}{boxp}{border}",
        txt.display(),
        s.font_size,
        s.color,
        align_flag(&s.align),
        s.y_frac
    );
    let out = Command::new("ffmpeg")
        // format=rgba has to be part of the *input* chain. Left to itself the
        // lavfi color source negotiates an opaque pixel format, the @0.0 alpha
        // is thrown away, and a later format=rgba refills alpha at 255 — which
        // turns every title card into a black rectangle over the whole frame.
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black@0.0:s={W}x{H},format=rgba"))
        .args(["-vf", &vf, "-frames:v", "1", "-pix_fmt", "rgba"])
        .arg(&png)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    let _ = std::fs::remove_file(&txt);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }

    finish_title(&png, s)
}

/// Bake the bevel, if any, into a rasterized card. Shared by text and shapes —
/// an embossed box is as reasonable as embossed type.
fn finish_title(png: &std::path::Path, s: &TitleStyle) -> Result<String, String> {
    if s.bevel != "Off" {
        let img = image::open(png).map_err(|e| e.to_string())?;
        let mut rgba = img.to_rgba8();
        let (w, h_px) = rgba.dimensions();
        let result = crate::bevel::compute_bevel(
            rgba.as_raw(),
            w,
            h_px,
            &crate::bevel::BevelParams {
                size: s.bevel_size.max(1.0) as u32,
                soften: s.soften.max(0.0) as u32,
                angle: s.angle as f32,
                altitude: s.altitude as f32,
                depth: s.depth.max(1.0) as u32,
                hi_opacity: s.hi_opacity as f32,
                sh_opacity: s.sh_opacity as f32,
                cameo: s.bevel == "Cameo",
            },
        );
        // Shadow first (multiply the black buffer), highlight second (screen the
        // white one) — the layer order the designer app composites in, so the
        // highlight is never dimmed by a shadow pass applied after it.
        // Alpha is untouched: the bevel shades the glyphs, it never grows them.
        let buf = rgba.as_mut();
        for i in 0..(w * h_px) as usize {
            let hi_a = result.hi_rgba[i * 4 + 3] as f32 / 255.0;
            let sh_a = result.sh_rgba[i * 4 + 3] as f32 / 255.0;
            for c in 0..3 {
                let shadowed = buf[i * 4 + c] as f32 * (1.0 - sh_a);
                buf[i * 4 + c] = (shadowed + hi_a * (255.0 - shadowed)) as u8;
            }
        }
        rgba.save(png).map_err(|e| e.to_string())?;
    }
    Ok(png.display().to_string())
}

async fn capture(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run {bin}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Returns (duration_seconds, has_audio).
pub async fn probe(path: &str) -> Result<(f64, bool), String> {
    if is_still(path) {
        // Nothing to read a duration from — just confirm ffmpeg can decode it,
        // so a corrupt file fails at import rather than at export.
        let dims = capture(
            "ffprobe",
            &["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width", "-of", "csv=p=0", path],
        )
        .await?;
        if dims.trim().is_empty() {
            return Err(format!("no image found in {path}"));
        }
        return Ok((STILL_SOURCE, false));
    }
    let dur = capture(
        "ffprobe",
        &["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
    )
    .await?;
    let audio = capture(
        "ffprobe",
        &["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", path],
    )
    .await?;
    let has_audio = !audio.trim().is_empty();
    match dur.trim().parse::<f64>() {
        Ok(d) if d > 0.0 => Ok((d, has_audio)),
        // No usable duration. An image in a container this table doesn't list
        // still has a video stream, so treat it as a still rather than refusing
        // a file ffmpeg is perfectly happy to decode — this is what lets the
        // "All files" option in the dialogs actually mean all files.
        _ => {
            let video = capture(
                "ffprobe",
                &["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width", "-of", "csv=p=0", path],
            )
            .await?;
            if video.trim().is_empty() {
                return Err(format!("no video or duration found in {path}"));
            }
            Ok((STILL_SOURCE, false))
        }
    }
}

/// One portrait-cropped JPEG frame at time `t`, as a data: URI for <img src>.
/// `effect` is the same filter snippet the export uses, and `title` (a rendered
/// title PNG + its opacity at this instant, approximating the export's fade)
/// is composited on top, so preview = export.
/// What gets composited on top of the base frame, bottom to top. Bundled into
/// one argument so a preview can gain a layer without the parameter list
/// growing a tail of Options.
#[derive(Clone, Default, Debug)]
pub struct Over {
    /// The incoming clip of a transition, and how far the blend has run
    /// (0 = not yet visible, 1 = fully replaced the base).
    /// (path, source time, framing, effect chain, alpha)
    pub blend: Option<(String, f64, String, String, f64)>,
    /// A rendered title card and its opacity at this instant.
    pub title: Option<(String, f64)>,
}

pub async fn frame_data_uri(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    framing: &str,
    effect: &str,
    over: Over,
) -> Result<String, String> {
    let mut chain = frame_chain(framing, w, h, "m");
    if !effect.is_empty() {
        // Seeking restarts the filter clock at 0, which would freeze every
        // time-based effect at its t=0 pose — most visible on a still, where
        // the frame itself never changes either. Run the effect on a clock
        // shifted to the seek point, then shift back so the title overlay
        // downstream still lines up on PTS 0.
        //
        // This is why every Motion look is written against input time rather
        // than a frame counter: one extracted frame has no frame history, so
        // anything that accumulates across frames renders the same at every
        // playhead position no matter what the clock says.
        chain = format!("{chain},setpts=PTS+{t:.3}/TB,{effect},setpts=PTS-{t:.3}/TB");
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if is_still(path) {
        cmd.args(["-loop", "1"]); // a lone frame has nothing to seek to
    }
    cmd.args(["-ss", &format!("{t:.3}"), "-i", path]);

    // Layer the extra inputs on in the order they will be composited, so the
    // filter graph's input indices follow the stacking order.
    let mut graph = format!("[0:v]{chain}[base];");
    let mut top = "base".to_string();
    let mut idx = 0;
    if let Some((bpath, bt, bframing, beffect, alpha)) = &over.blend {
        idx += 1;
        if is_still(bpath) {
            cmd.args(["-loop", "1"]);
        }
        cmd.args(["-ss", &format!("{bt:.3}"), "-i", bpath]);
        let mut bchain = frame_chain(bframing, w, h, "b");
        if !beffect.is_empty() {
            bchain = format!("{bchain},setpts=PTS+{bt:.3}/TB,{beffect},setpts=PTS-{bt:.3}/TB");
        }
        // The incoming clip fades up over the outgoing one — the same thing
        // xfade does in the export, so a scrub inside a transition shows the
        // blend rather than a hard cut.
        graph += &format!(
            "[{idx}:v]{bchain},format=rgba,colorchannelmixer=aa={:.3}[inc];\
             [{top}][inc]overlay[x{idx}];",
            alpha.clamp(0.0, 1.0)
        );
        top = format!("x{idx}");
    }
    if let Some((png, alpha)) = &over.title {
        idx += 1;
        cmd.args(["-i", png]);
        graph += &format!(
            "[{idx}:v]scale={w}:{h},format=rgba,colorchannelmixer=aa={:.3}[ttl];\
             [{top}][ttl]overlay[x{idx}];",
            alpha.clamp(0.0, 1.0)
        );
        top = format!("x{idx}");
    }
    if idx == 0 {
        cmd.args(["-vf", &chain]);
    } else {
        graph += &format!("[{top}]null[out]");
        cmd.args(["-filter_complex", &graph, "-map", "[out]"]);
    }
    let out = cmd
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "mjpeg", "-"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(format!("data:image/jpeg;base64,{}", b64(&out.stdout)))
}

/// Where a user's own settings live, as opposed to things that can be
/// regenerated. Presets belong here: losing the cache costs a re-render,
/// losing this costs work.
pub fn config_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("morreel")
}

fn cache_dir(sub: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("morreel").join(sub)
}

/// Cache path for a source's scrub proxy, keyed by path + mtime + size so an
/// edited/replaced source gets a fresh proxy.
fn proxy_path(src: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    if let Ok(m) = std::fs::metadata(src) {
        m.len().hash(&mut h);
        if let Ok(t) = m.modified() {
            t.hash(&mut h);
        }
    }
    cache_dir("proxies").join(format!("{:016x}.mp4", h.finish()))
}

/// Build (or reuse) a 480p video-only proxy for fast scrub seeks.
/// Export never touches proxies — they exist only for preview extraction.
pub async fn ensure_proxy(src: &str) -> Result<String, String> {
    // A still decodes instantly; a proxy would only be a slower copy of it.
    if is_still(src) {
        return Ok(src.to_string());
    }
    let dst = proxy_path(src);
    if dst.exists() {
        return Ok(dst.display().to_string());
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Build to a temp name then rename, so a killed build never leaves a
    // truncated proxy that would poison the cache.
    let tmp = dst.with_extension("part.mp4");
    let out = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", src])
        .args(["-vf", "scale=-2:480", "-c:v", "libx264", "-preset", "veryfast", "-crf", "28"])
        .args(["-g", "30", "-an", "-movflags", "+faststart"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(dst.display().to_string())
}

fn eff(effect: &str) -> String {
    if effect.is_empty() { String::new() } else { format!(",{effect}") }
}

/// How a source fills the portrait frame — the same chain feeds preview,
/// thumbnails and export, so framing always looks like it ships.
/// "Crop" (default) covers and center-crops; "Blur" fits the whole picture over
/// a blurred, zoomed copy of itself (the reel look for landscape footage); "Fit"
/// letterboxes on black; "Zoom" punches in 1.5× then crops.
///
/// `tag` uniquifies the filter labels the "Blur" branch splits into: every clip
/// and overlay shares one filter_complex namespace, so `[bg]`/`[fg]` would
/// collide across items without it. Pass the item's index; linear framings ignore it.
// ponytail: fixed 1.5× zoom / sigma — a per-clip slider when someone asks.
fn frame_chain(framing: &str, w: u32, h: u32, tag: &str) -> String {
    match framing {
        "Fit" => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"
        ),
        "Zoom" => format!(
            "scale={}:{}:force_original_aspect_ratio=increase,crop={w}:{h}",
            w * 3 / 2,
            h * 3 / 2
        ),
        "Blur" => {
            // Blur scaled to the frame, so a 108px thumbnail isn't a smear and a
            // 1080px export isn't under-blurred.
            let sigma = (w as f64 / 45.0).max(2.0);
            format!(
                "split=2[bg{tag}][fg{tag}];\
                 [bg{tag}]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},gblur=sigma={sigma:.1}[bb{tag}];\
                 [fg{tag}]scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2[ff{tag}];\
                 [bb{tag}][ff{tag}]overlay=(W-w)/2:(H-h)/2"
            )
        }
        _ => format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"),
    }
}

/// Audio chain for V1 clip at input `i` — silent clips get anullsrc so the
/// concat stays aligned with the video track.
fn clip_audio(i: usize, c: &ClipSpec) -> String {
    // A muted clip becomes silence of the right length rather than a volume=0
    // stream, so a muted source that also fails to decode can't stall the mix.
    if c.has_audio && c.volume > 0.0 {
        let tempo = atempo_chain(c.speed);
        let tempo = if tempo.is_empty() { String::new() } else { format!(",{tempo}") };
        format!(
            "[{i}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,\
             aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo{tempo},\
             volume={:.2}[a{i}];",
            c.in_s, c.out_s, c.volume
        )
    } else {
        format!("anullsrc=r=48000:cl=stereo,atrim=0:{:.3}[a{i}];", c.trimmed())
    }
}

/// A1/A2 chain for item `k` at input `idx`: trim → process → gain/fades → delay.
fn a1_audio(idx: usize, k: usize, a: &AudioSpec) -> String {
    let mut mid = String::new();
    let proc = audio_process_chain(a);
    if !proc.is_empty() {
        mid.push(',');
        mid.push_str(&proc);
    }
    let gain = audio_gain_fades(a);
    if !gain.is_empty() {
        mid.push(',');
        mid.push_str(&gain);
    }
    format!(
        "[{idx}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,\
         aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo\
         {mid},adelay={}:all=1[au{k}];",
        a.in_s,
        a.out_s,
        (a.at * 1000.0).round() as u64
    )
}

/// The whole edit as one filter graph: V1 clips trim + portrait crop + effect,
/// concat; V2 overlays composited on top; T titles above those; A1 audio mixed
/// under. Ends [vout][aout]. Input order: clips, overlays, titles, audio.
pub fn build_filter(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    opts: ExportOpts,
) -> String {
    let mut f = String::new();
    for (i, c) in clips.iter().enumerate() {
        // Dividing PTS by the speed is what retimes the video; fps= after it
        // resamples so slow motion gets duplicated frames instead of a stutter.
        f += &format!(
            "[{i}:v]trim=start={:.3}:end={:.3},setpts=(PTS-STARTPTS)/{:.4},fps={FPS},\
             {},setsar=1{}[v{i}];",
            c.in_s,
            c.out_s,
            c.speed.max(0.01),
            frame_chain(&c.framing, W, H, &format!("c{i}")),
            eff(&c.effect)
        );
        f += &clip_audio(i, c);
    }
    // No transitions anywhere is the common case, and one concat of everything
    // is both cheaper and exactly what shipped before, so it stays the path.
    let fades: Vec<f64> = (0..clips.len())
        .map(|i| if i == 0 { 0.0 } else { clips[i].fade_in(Some(&clips[i - 1])) })
        .collect();
    let cuts_only = fades.iter().all(|d| *d <= 0.0);
    // The concat path hands on [vc]/[ac]; the pairwise path builds up from the
    // first clip's own labels.
    let (mut vhead, mut ahead) = if cuts_only {
        ("vc".to_string(), "ac".to_string())
    } else {
        ("v0".to_string(), "a0".to_string())
    };
    if cuts_only {
        for i in 0..clips.len() {
            f += &format!("[v{i}][a{i}]");
        }
        f += &format!("concat=n={}:v=1:a=1[vc][ac];", clips.len());
    } else {
        // Otherwise join the clips a pair at a time: xfade where there is a
        // transition, concat where there is a cut. xfade's offset is measured
        // in the accumulated stream, which is exactly where the incoming clip
        // starts on the finished timeline.
        let mut acc = clips[0].trimmed();
        for i in 1..clips.len() {
            let d = fades[i];
            if d > 0.0 {
                f += &format!(
                    "[{vhead}][v{i}]xfade=transition={}:duration={:.3}:offset={:.3}[vt{i}];",
                    xfade_name(&clips[i].transition),
                    d,
                    acc - d
                );
                // The audio side crossfades at the join, which needs no offset —
                // acrossfade overlaps the tail of one with the head of the next.
                f += &format!("[{ahead}][a{i}]acrossfade=d={d:.3}:c1=tri:c2=tri[at{i}];");
                acc += clips[i].trimmed() - d;
            } else {
                // concat hands on a 1/1000000 timebase and xfade refuses inputs
                // whose timebases differ, so put it back on the frame clock.
                f += &format!("[{vhead}][v{i}]concat=n=2:v=1:a=0,settb=1/{FPS}[vt{i}];");
                f += &format!("[{ahead}][a{i}]concat=n=2:v=0:a=1[at{i}];");
                acc += clips[i].trimmed();
            }
            vhead = format!("vt{i}");
            ahead = format!("at{i}");
        }
    }

    let mut vl = vhead;
    for (j, o) in overlays.iter().enumerate() {
        let idx = clips.len() + j;
        f += &format!(
            "[{idx}:v]trim=start={:.3}:end={:.3},setpts=(PTS-STARTPTS)/{:.4}+{:.3}/TB,fps={FPS},\
             {},setsar=1{}[ov{j}];",
            o.in_s,
            o.out_s,
            o.speed.max(0.01),
            o.at,
            frame_chain(&o.framing, W, H, &format!("o{j}")),
            eff(&o.effect)
        );
        f += &format!(
            "[{vl}][ov{j}]overlay=eof_action=pass:enable='between(t,{:.3},{:.3})'[vx{j}];",
            o.at,
            o.at + o.trimmed()
        );
        vl = format!("vx{j}");
    }

    for (j, t) in titles.iter().enumerate() {
        let idx = clips.len() + overlays.len() + j;
        // Title PNGs are fed with -loop 1 (see export), so the stream has real
        // timestamps to fade against: alpha in/out over `fade`, then shifted to
        // its timeline spot. Short titles shrink the fade so they still read.
        let fade = title_fade(t.dur);
        let fin = if t.fade_in { format!("fade=t=in:st=0:d={fade:.3}:alpha=1,") } else { String::new() };
        let fout = if t.fade_out {
            format!("fade=t=out:st={:.3}:d={fade:.3}:alpha=1,", t.dur - fade)
        } else {
            String::new()
        };
        f += &format!(
            "[{idx}:v]format=rgba,trim=duration={:.3},{fin}{fout}setpts=PTS+{:.3}/TB[ti{j}];",
            t.dur, t.at
        );
        f += &format!(
            "[{vl}][ti{j}]overlay={}enable='between(t,{:.3},{:.3})'[vt{j}];",
            t.overlay_xy(),
            t.at,
            t.at + t.dur
        );
        vl = format!("vt{j}");
    }

    let mut al = ahead.clone();
    if !audio.is_empty() {
        for (k, a) in audio.iter().enumerate() {
            f += &a1_audio(clips.len() + overlays.len() + titles.len() + k, k, a);
        }
        f += &mix_audio(&ahead, audio, "am");
        f += ";";
        al = "am".to_string();
    }

    // The edit is always composed at 1080x1920 (title cards are rasterized at
    // that size), so a smaller export is one scale at the very end rather than
    // a differently shaped graph.
    let fit = if (opts.width, opts.height) == (W, H) {
        "null".to_string()
    } else {
        format!("scale={}:{}:flags=lanczos", opts.width, opts.height)
    };
    f += &format!("[{vl}]{fit}[vout];");
    // GIF carries no audio track. The mix still has to go somewhere: an
    // unmapped [aout] is a hard filtergraph error, so it drains into a sink.
    if opts.format.has_audio() {
        f += &format!("[{al}]anull[aout]");
    } else {
        f += &format!("[{al}]anullsink", );
    }
    if opts.format == Format::Gif {
        // One palette measured over the whole clip, then applied. Without this
        // a GIF of real video bands into mud.
        f += ";[vout]split[gp0][gp1];[gp0]palettegen=stats_mode=diff[pal];\
             [gp1][pal]paletteuse=dither=bayer:bayer_scale=3[gout]";
    }
    f
}

/// V1+A1 audio only, ending [aout]. Input order: clips, then audio files.
/// In-app playback renders this to a wav — video stays on the scrub pipeline.
pub fn build_audio_filter(clips: &[ClipSpec], audio: &[AudioSpec]) -> String {
    let mut f = String::new();
    for (i, c) in clips.iter().enumerate() {
        f += &clip_audio(i, c);
    }
    for i in 0..clips.len() {
        f += &format!("[a{i}]");
    }
    f += &format!("concat=n={}:v=0:a=1[ac];", clips.len());
    for (k, a) in audio.iter().enumerate() {
        f += &a1_audio(clips.len() + k, k, a);
    }
    f += &mix_audio("ac", audio, "aout");
    f
}

/// Render the timeline's audio mix to a wav at `out`. No video encode, so this
/// is fast enough to run on every Play press.
pub async fn render_audio_mix(
    clips: &[ClipSpec],
    audio: &[AudioSpec],
    out: &Path,
) -> Result<(), String> {
    if clips.is_empty() {
        return Err("nothing to play".into());
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error"]);
    for path in clips.iter().map(|c| &c.path).chain(audio.iter().map(|a| &a.path)) {
        cmd.args(["-i", path]);
    }
    cmd.args(["-filter_complex", &build_audio_filter(clips, audio)])
        .args(["-map", "[aout]", "-c:a", "pcm_s16le"])
        .arg(out)
        .stdin(Stdio::null());
    let o = cmd.output().await.map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(())
}

/// Play `path` from `start` seconds, sound only, no window. Returns the child
/// so pausing can kill it; the child is also killed if dropped.
pub fn launch_audio(path: &Path, start: f64) -> Result<tokio::process::Child, String> {
    let mpv_start = format!("--start={start:.3}");
    let ss = format!("{start:.3}");
    let candidates: [(&str, Vec<&str>); 2] = [
        ("mpv", vec!["--no-video", "--really-quiet", &mpv_start]),
        ("ffplay", vec!["-nodisp", "-autoexit", "-v", "error", "-ss", &ss]),
    ];
    for (bin, args) in candidates {
        if let Ok(child) = Command::new(bin)
            .args(&args)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            return Ok(child);
        }
    }
    Err("no audio player found — install mpv or ffplay".into())
}

/// Detect silent stretches in a file's audio via ffmpeg `silencedetect`.
/// Returns `(start, end)` pairs in source seconds. `noise_db` is a negative
/// threshold (e.g. −32); `min_dur` is the shortest silence worth reporting.
pub async fn detect_silence(
    path: &str,
    noise_db: f64,
    min_dur: f64,
) -> Result<Vec<(f64, f64)>, String> {
    let noise = noise_db.min(-1.0); // always treat as "below N dB"
    let min_dur = min_dur.max(0.05);
    let af = format!("silencedetect=noise={noise:.1}dB:d={min_dur:.3}");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-i", path, "-af", &af, "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    // silencedetect logs on stderr; exit can still be 0 with empty audio.
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && !log.contains("silence_start") && !log.contains("silence_end") {
        let err = log.trim();
        if err.is_empty() {
            return Err("silencedetect failed".into());
        }
        return Err(err.to_string());
    }
    Ok(parse_silence_log(&log))
}

/// Parse ffmpeg silencedetect stderr into (start, end) pairs.
pub fn parse_silence_log(log: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(rest) = line.split("silence_start:").nth(1) {
            if let Ok(t) = rest.split_whitespace().next().unwrap_or("").parse::<f64>() {
                start = Some(t);
            }
        }
        if let Some(rest) = line.split("silence_end:").nth(1) {
            if let Ok(t) = rest.split_whitespace().next().unwrap_or("").parse::<f64>() {
                if let Some(s) = start.take() {
                    if t > s {
                        out.push((s, t));
                    }
                }
            }
        }
    }
    // File ends mid-silence: silencedetect may omit silence_end. Drop the open
    // start — the caller clips against out_s and trailing silence is implied
    // by "no keep after last end".
    out
}

/// Invert silence ranges into keep-loud ranges within `[in_s, out_s]`.
/// `pad` shrinks each silence (keeps a little room around speech);
/// `min_keep` drops leftover scraps shorter than that.
pub fn keep_loud_ranges(
    in_s: f64,
    out_s: f64,
    silences: &[(f64, f64)],
    pad: f64,
    min_keep: f64,
) -> Vec<(f64, f64)> {
    if out_s - in_s < min_keep {
        return Vec::new();
    }
    let pad = pad.max(0.0);
    let min_keep = min_keep.max(0.05);
    // Clip + shrink silences by pad so speech edges aren't bitten.
    let mut sil: Vec<(f64, f64)> = silences
        .iter()
        .filter_map(|&(s, e)| {
            let s = (s + pad).clamp(in_s, out_s);
            let e = (e - pad).clamp(in_s, out_s);
            (e > s).then_some((s, e))
        })
        .collect();
    sil.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Merge overlaps.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (s, e) in sil {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        merged.push((s, e));
    }
    let mut keeps = Vec::new();
    let mut t = in_s;
    for (s, e) in merged {
        if s > t && s - t >= min_keep {
            keeps.push((t, s));
        }
        t = e.max(t);
    }
    if out_s > t && out_s - t >= min_keep {
        keeps.push((t, out_s));
    }
    keeps
}

/// Waveform strip of a file's audio (bright teal on transparent) as a data: URI.
/// Drawn once for the whole source — timeline items window into it with
/// background-size/position, so trims and splits need no re-render.
///
/// `scale=sqrt` + a mild pre-gain lifts quiet speech so the strip reads as a
/// real envelope rather than a thin mid-line; `draw=full` fills to the peaks.
pub async fn waveform_data_uri(path: &str) -> Result<String, String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", path])
        .args([
            "-filter_complex",
            // Mono strip, dual-tone teal for depth. Rendered tall and dense
            // (stretched to the lane height by wave_css), and with cbrt scaling +
            // a hotter gain so quiet passages still fill the lane — the envelope
            // reads thick and detailed rather than a thin centre thread.
            "aformat=channel_layouts=mono,volume=6dB,\
             showwavespic=s=2400x200:colors=0x7af5ee|0x3dd6c8:scale=cbrt:draw=full",
        ])
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", "png", "-"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(format!("data:image/png;base64,{}", b64(&out.stdout)))
}

/// Parse SRT into (start_s, end_s, text). Tolerant: blocks without a valid
/// timing line are skipped; both `,` and `.` millisecond separators accepted.
pub fn parse_srt(srt: &str) -> Vec<(f64, f64, String)> {
    fn ts(s: &str) -> Option<f64> {
        let s = s.trim();
        let (hms, ms) = s.split_once(',').or_else(|| s.split_once('.'))?;
        let mut p = hms.split(':');
        let (h, m, sec) = (p.next()?, p.next()?, p.next()?);
        Some(
            h.parse::<f64>().ok()? * 3600.0
                + m.parse::<f64>().ok()? * 60.0
                + sec.parse::<f64>().ok()?
                + ms.trim().parse::<f64>().ok()? / 1000.0,
        )
    }
    srt.split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
            let mut timing = lines.next()?;
            if !timing.contains("-->") {
                timing = lines.next()?; // skip the numeric index
            }
            let (a, b) = timing.split_once("-->")?;
            let text = lines.collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                return None;
            }
            Some((ts(a)?, ts(b)?, text))
        })
        .collect()
}

/// End timestamp of a Whisper live-output line like
/// "[00:03.400 --> 00:07.120]  text" — colon groups fold as base-60.
fn seg_end_s(line: &str) -> Option<f64> {
    let ts = line.split("-->").nth(1)?.split(']').next()?.trim();
    let mut acc = 0.0;
    for part in ts.split(':') {
        acc = acc * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(acc)
}

/// Transcribe a wav into timed segments using an installed Whisper CLI
/// (openai-whisper or whisper-ctranslate2 — both share the same interface).
/// `on_progress` gets 0.0..=1.0 as segments stream out, measured against
/// `total_s` (the mix duration).
pub async fn transcribe(
    wav: &Path,
    total_s: f64,
    mut on_progress: impl FnMut(f64),
) -> Result<Vec<(f64, f64, String)>, String> {
    let dir = wav.parent().unwrap_or_else(|| Path::new("."));
    // ctranslate2 first: it has VAD filtering (skips music/silence stretches
    // that derail decoding) and is faster on CPU at the same model size.
    let candidates: [(&str, &[&str]); 2] = [
        ("whisper-ctranslate2", &["--model", "small", "--output_format", "srt", "--vad_filter", "True", "--output_dir"]),
        ("whisper", &["--model", "small", "--output_format", "srt", "--output_dir"]),
    ];
    for (bin, args) in candidates {
        let mut child = match Command::new(bin)
            .arg(wav)
            .args(args)
            .arg(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Err(_) => continue, // binary not installed — try the next one
            Ok(c) => c,
        };
        let mut stderr = child.stderr.take().unwrap();
        let err_task = tokio::spawn(async move {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s).await;
            s
        });
        // Both CLIs print "[MM:SS.mmm --> MM:SS.mmm] text" per segment as they
        // decode; the end stamp against the mix duration is the progress.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(end) = seg_end_s(&line) {
                if total_s > 0.0 {
                    on_progress((end / total_s).min(1.0));
                }
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if !status.success() {
            let err = err_task.await.unwrap_or_default().trim().to_string();
            return Err(if err.is_empty() { format!("{bin} exited with {status}") } else { err });
        }
        let srt = wav.with_extension("srt");
        let text = std::fs::read_to_string(&srt).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&srt);
        on_progress(1.0);
        return Ok(parse_srt(&text));
    }
    Err("no transcriber found — pip install openai-whisper (or whisper-ctranslate2)".into())
}

/// Render to `out` with the chosen container, quality and size.
/// `on_progress` gets 0.0..=1.0 as ffmpeg reports out_time.
pub async fn export(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    out: &Path,
    opts: ExportOpts,
    mut on_progress: impl FnMut(f64),
) -> Result<(), String> {
    if clips.is_empty() {
        return Err("nothing to export".into());
    }
    let total = timeline_len(clips);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-v", "error"]);
    for path in clips.iter().map(|c| &c.path).chain(overlays.iter().map(|o| &o.path)) {
        // Same trick the titles use below: -loop 1 turns a still into a stream
        // with timestamps, and the graph's trim= bounds it back to its span.
        if is_still(path) {
            cmd.args(["-loop", "1"]);
        }
        cmd.args(["-i", path]);
    }
    for t in titles {
        // -loop 1 turns the still into a timestamped stream so the filter
        // graph's alpha fades have time to fade against; trim= bounds it.
        cmd.args(["-loop", "1", "-i", &t.png]);
    }
    for a in audio {
        cmd.args(["-i", &a.path]);
    }
    let (speed, crf) = opts.quality.encode(opts.format);
    let crf = crf.to_string();
    cmd.args(["-filter_complex", &build_filter(clips, overlays, titles, audio, opts)]);
    // GIF's palette pass renames the video output; everything else maps [vout].
    cmd.args(["-map", if opts.format == Format::Gif { "[gout]" } else { "[vout]" }]);
    if opts.format.has_audio() {
        cmd.args(["-map", "[aout]"]);
    }
    match opts.format {
        Format::Mp4 => {
            cmd.args(["-c:v", "libx264", "-preset", speed, "-crf", &crf, "-pix_fmt", "yuv420p"])
                .args(["-c:a", "aac", "-b:a", "192k"])
                // faststart puts the index first so it plays before it finishes
                // downloading — the difference between a reel that starts and
                // one that spins.
                .args(["-movflags", "+faststart"]);
        }
        Format::WebM => {
            cmd.args(["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", &crf, "-row-mt", "1"])
                .args(["-cpu-used", speed, "-pix_fmt", "yuv420p"])
                .args(["-c:a", "libopus", "-b:a", "128k"]);
        }
        Format::Gif => {
            cmd.args(["-loop", "0"]);
        }
    }
    cmd.args(["-progress", "pipe:1", "-nostats"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    let mut stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });

    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // out_time_us on modern ffmpeg; out_time_ms is also microseconds (historical quirk).
        let us = line
            .strip_prefix("out_time_us=")
            .or_else(|| line.strip_prefix("out_time_ms="))
            .and_then(|v| v.parse::<f64>().ok());
        if let Some(us) = us {
            if total > 0.0 && us > 0.0 {
                on_progress((us / 1e6 / total).min(1.0));
            }
        }
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;
    if !status.success() {
        let err = err_task.await.unwrap_or_default();
        return Err(if err.trim().is_empty() {
            format!("ffmpeg exited with {status}")
        } else {
            err.trim().to_string()
        });
    }
    on_progress(1.0);
    Ok(())
}

/// Hand playback to a dedicated player, smplayer-style: mpv if installed,
/// else ffplay (ships with ffmpeg). Detached — the editor stays responsive.
pub fn launch_player(path: &Path) -> Result<&'static str, String> {
    let candidates: [(&str, &[&str]); 2] = [
        ("mpv", &["--force-window=yes", "--keep-open=yes"]),
        ("ffplay", &["-autoexit", "-window_title", "MorReel preview"]),
    ];
    for (bin, args) in candidates {
        if std::process::Command::new(bin)
            .args(args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(bin);
        }
    }
    Err("no player found — install mpv or ffplay".into())
}

// ponytail: hand-rolled base64 — 15 lines beats a dependency for one data-URI use.
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | (*chunk.get(2).unwrap_or(&0) as u32);
        s.push(T[(n >> 18 & 63) as usize] as char);
        s.push(T[(n >> 12 & 63) as usize] as char);
        s.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        s.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_matches_rfc4648() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"M"), "TQ==");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"Man"), "TWFu");
    }

    #[test]
    fn filter_graph_shape() {
        let clips = [
            ClipSpec { path: "a.mp4".into(), in_s: 0.5, out_s: 2.0, has_audio: true, effect: "hue=s=0".into(), ..Default::default() },
            ClipSpec { path: "b.mp4".into(), in_s: 0.0, out_s: 1.0, has_audio: false, ..Default::default() },
        ];
        let overlays = [OverlaySpec { path: "c.mp4".into(), in_s: 0.0, out_s: 1.0, at: 0.5, ..Default::default() }];
        let titles = [TitleSpec { png: "t.png".into(), at: 0.2, dur: 2.0, ..Default::default() }];
        let audio = [AudioSpec {
            path: "m.mp3".into(),
            in_s: 0.0,
            out_s: 2.0,
            at: 1.0,
            volume: 0.5,
            ..Default::default()
        }];
        let f = build_filter(&clips, &overlays, &titles, &audio, ExportOpts::default());
        assert!(f.contains("[0:v]trim=start=0.500:end=2.000"));
        assert!(f.contains("setsar=1,hue=s=0[v0]"));
        assert!(f.contains("crop=1080:1920"));
        assert!(!f.contains("[1:a]") && f.contains("anullsrc"));
        assert!(f.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"));
        // input order: clips 0-1, overlay 2, title 3, audio 4
        assert!(f.contains("[2:v]trim=start=0.000:end=1.000,setpts=(PTS-STARTPTS)/1.0000+0.500/TB"));
        assert!(f.contains("[vc][ov0]overlay=eof_action=pass:enable='between(t,0.500,1.500)'[vx0]"));
        // title: looped still trimmed to its duration, alpha-faded both ends,
        // shifted to its timeline spot
        assert!(f.contains(
            "[3:v]format=rgba,trim=duration=2.000,\
             fade=t=in:st=0:d=0.300:alpha=1,fade=t=out:st=1.700:d=0.300:alpha=1,\
             setpts=PTS+0.200/TB[ti0]"
        ));
        assert!(f.contains("[vx0][ti0]overlay=enable='between(t,0.200,2.200)'[vt0]"));
        assert!(f.contains("[4:a]") && f.contains("volume=0.500") && f.contains("adelay=1000:all=1[au0]"), "{f}");
        assert!(f.contains("[ac][au0]amix=inputs=2:duration=first:normalize=0[am]"));
        assert!(f.ends_with("[vt0]null[vout];[am]anull[aout]"));

        // no overlays / titles / audio degenerates to plain concat
        let f = build_filter(&clips, &[], &[], &[], ExportOpts::default());
        assert!(f.ends_with("[vc]null[vout];[ac]anull[aout]"));
    }

    #[test]
    fn silence_log_parses_start_end_pairs() {
        let log = "\
[silencedetect @ 0x1] silence_start: 0.5
[silencedetect @ 0x1] silence_end: 1.25 | silence_duration: 0.75
noise
[silencedetect @ 0x1] silence_start: 3.0
[silencedetect @ 0x1] silence_end: 4.0 | silence_duration: 1.0
";
        assert_eq!(parse_silence_log(log), vec![(0.5, 1.25), (3.0, 4.0)]);
        assert!(parse_silence_log("nothing here").is_empty());
    }

    #[test]
    fn keep_loud_ranges_drops_silence_and_scraps() {
        // 0—10 with silence 2—5 and 7—8 → keep 0—2, 5—7, 8—10
        let sil = [(2.0, 5.0), (7.0, 8.0)];
        let k = keep_loud_ranges(0.0, 10.0, &sil, 0.0, 0.2);
        assert_eq!(k, vec![(0.0, 2.0), (5.0, 7.0), (8.0, 10.0)]);
        // Pad 0.25 shrinks silence so keep edges breathe.
        let k = keep_loud_ranges(0.0, 10.0, &sil, 0.25, 0.2);
        assert_eq!(k, vec![(0.0, 2.25), (4.75, 7.25), (7.75, 10.0)]);
        // Whole span silent → nothing to keep.
        assert!(keep_loud_ranges(0.0, 5.0, &[(0.0, 5.0)], 0.0, 0.2).is_empty());
        // No silence → one full keep.
        assert_eq!(keep_loud_ranges(1.0, 4.0, &[], 0.0, 0.2), vec![(1.0, 4.0)]);
    }

    #[tokio::test]
    async fn detect_silence_finds_a_gap_between_tones() {
        let dir = std::env::temp_dir().join("morreel-sil-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("gap.m4a").display().to_string();
        // 0.5s tone, 0.8s silence, 0.5s tone.
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=0.5",
            "-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=0.5",
            "-filter_complex",
            "[1:a]atrim=0:0.8,asetpts=PTS-STARTPTS[s];\
             [0:a][s][2:a]concat=n=3:v=0:a=1[a]",
            "-map", "[a]", "-t", "1.8", "-c:a", "aac", &src,
        ])
        .await
        .unwrap();
        let sil = detect_silence(&src, -40.0, 0.25).await.unwrap();
        assert!(!sil.is_empty(), "expected a silence gap, got {sil:?}");
        // Gap should sit around 0.5–1.3.
        let (s, e) = sil[0];
        assert!(s > 0.3 && s < 0.8, "silence_start {s}");
        assert!(e > 1.0 && e < 1.6, "silence_end {e}");
    }

    #[tokio::test]
    async fn waveform_renders_as_png_data_uri() {
        let dir = std::env::temp_dir().join("morreel-wave-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("tone.m4a").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=1", "-c:a", "aac", &src,
        ]).await.unwrap();
        let uri = waveform_data_uri(&src).await.unwrap();
        assert!(uri.starts_with("data:image/png;base64,"), "not a png data uri");
        assert!(uri.len() > 100, "suspiciously empty waveform");

        // A video's own audio draws the same way — that is what puts a waveform
        // strip on a V1 clip. ffmpeg picks the audio stream out of the container.
        let av = dir.join("av.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=30",
            "-f", "lavfi", "-i", "sine=duration=1",
            "-c:v", "libx264", "-c:a", "aac", "-shortest", &av,
        ]).await.unwrap();
        let uri = waveform_data_uri(&av).await.unwrap();
        assert!(uri.starts_with("data:image/png;base64,") && uri.len() > 100, "no waveform from a video");

        // A silent source has nothing to draw and errors rather than returning
        // an empty image — callers gate on has_audio and never ask.
        let silent = dir.join("silent.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=30",
            "-c:v", "libx264", &silent,
        ]).await.unwrap();
        assert!(waveform_data_uri(&silent).await.is_err(), "silent source yielded a waveform");
    }

    #[test]
    fn seg_end_parses() {
        assert_eq!(seg_end_s("[00:03.400 --> 00:07.120]  hi"), Some(7.12));
        assert_eq!(seg_end_s("[01:00:01.500 --> 01:00:02.000] x"), Some(3602.0));
        assert_eq!(seg_end_s("Detected language 'English'"), None);
    }

    // Needs a Whisper CLI + espeak-ng on PATH: cargo test transcribe -- --ignored
    #[tokio::test]
    #[ignore]
    async fn transcribe_streams_progress_and_segments() {
        let dir = std::env::temp_dir().join("morreel-stt-test");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("speech.wav");
        capture("espeak-ng", &["-w", &wav.display().to_string(), "Hello world, this is a test."])
            .await
            .unwrap();
        let mut ticks = 0;
        let segs = transcribe(&wav, 3.0, |_| ticks += 1).await.unwrap();
        assert!(!segs.is_empty(), "no segments transcribed");
        assert!(ticks > 0, "progress never reported");
    }

    #[test]
    fn srt_parses() {
        let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello there\n\n\
                   2\n00:01:00.000 --> 00:01:02.000\nSecond\nline\n\n\
                   garbage block\n\n";
        let segs = parse_srt(srt);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], (1.0, 2.5, "Hello there".to_string()));
        assert_eq!(segs[1].2, "Second line");
        assert!((segs[1].0 - 60.0).abs() < 1e-9 && (segs[1].1 - 62.0).abs() < 1e-9);
    }

    /// Multiply out an atempo chain so the test checks the net rate, not the
    /// exact factorisation.
    fn atempo_product(chain: &str) -> f64 {
        if chain.is_empty() {
            return 1.0;
        }
        chain
            .split(',')
            .map(|p| p.trim_start_matches("atempo=").parse::<f64>().unwrap())
            .product()
    }

    #[test]
    fn atempo_chain_reaches_any_speed() {
        assert_eq!(atempo_chain(1.0), "", "1x needs no filter at all");
        // Every instance must stay inside atempo's own 0.5–2.0 window.
        for speed in [0.25, 0.5, 0.75, 1.5, 2.0, 3.0, 4.0, 8.0] {
            let chain = atempo_chain(speed);
            for part in chain.split(',') {
                let f: f64 = part.trim_start_matches("atempo=").parse().unwrap();
                assert!((0.5..=2.0).contains(&f), "{speed}x emitted out-of-range {f}");
            }
            assert!((atempo_product(&chain) - speed).abs() < 1e-6, "{speed}x came out wrong");
        }
    }

    #[test]
    fn speed_retimes_span_and_chains() {
        let base = ClipSpec { in_s: 1.0, out_s: 5.0, has_audio: true, ..Default::default() };
        // 4 s of source at 1x is 4 s of timeline, at 2x it is 2 s.
        assert_eq!(base.trimmed(), 4.0);
        assert_eq!(ClipSpec { speed: 2.0, ..base.clone() }.trimmed(), 2.0);
        assert_eq!(ClipSpec { speed: 0.5, ..base.clone() }.trimmed(), 8.0);
        // Default must be 1x, never a zero that divides the timeline away.
        assert_eq!(ClipSpec::default().speed, 1.0);

        let fast = clip_audio(0, &ClipSpec { speed: 2.0, ..base.clone() });
        assert!(fast.contains("atempo=2.0000"), "audio not retimed: {fast}");
        // A muted clip becomes silence of the right length, not volume=0 audio.
        let muted = clip_audio(0, &ClipSpec { volume: 0.0, ..base.clone() });
        assert!(muted.contains("anullsrc") && muted.contains("atrim=0:4.000"), "{muted}");
        assert!(clip_audio(0, &ClipSpec { volume: 0.4, ..base.clone() }).contains("volume=0.40"));

        // The video side retimes through setpts and the span shrinks to match.
        let f = build_filter(&[ClipSpec { speed: 2.0, ..base }], &[], &[], &[], ExportOpts::default());
        assert!(f.contains("setpts=(PTS-STARTPTS)/2.0000"), "video not retimed: {f}");
    }

    // Retiming has to hold up in a real encode, not just in the filter string.
    #[tokio::test]
    async fn speed_changes_exported_duration() {
        let dir = std::env::temp_dir().join("morreel-speed-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=4:size=320x240:rate=30",
            "-f", "lavfi", "-i", "sine=duration=4",
            "-c:v", "libx264", "-c:a", "aac", "-shortest", &src,
        ]).await.unwrap();

        let out = dir.join("fast.mp4");
        let clips = [ClipSpec {
            path: src,
            in_s: 0.0,
            out_s: 4.0,
            has_audio: true,
            speed: 2.0,
            ..Default::default()
        }];
        export(&clips, &[], &[], &[], &out, ExportOpts::preview(), |_| {}).await.unwrap();
        let d = capture("ffprobe", &[
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        let secs: f64 = d.trim().parse().unwrap();
        assert!((secs - 2.0).abs() < 0.25, "4 s at 2x should be ~2 s, got {secs}");
    }

    // A title card is composited over video, so everything that is not glyph
    // has to be genuinely transparent. An opaque canvas blacks out the frame
    // for the title's whole duration, and it also feeds the bevel a mask that
    // covers the entire card, so the relief lands on the frame border instead
    // of on the letters.
    #[tokio::test]
    async fn title_card_is_transparent_outside_the_glyphs() {
        let png = render_title(&TitleStyle { text: "Hi".into(), font_size: 120, ..Default::default() }).await.unwrap();
        let img = image::open(&png).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(img.get_pixel(x, y).0[3], 0, "corner ({x},{y}) is not transparent");
        }
        let opaque = img.pixels().filter(|p| p.0[3] > 250).count();
        assert!(opaque > 0, "nothing was drawn at all");
        assert!(
            (opaque as f64) < 0.2 * (w * h) as f64,
            "two glyphs should cover a sliver of the card, not {opaque} px"
        );

        // The bevel composite writes RGB and must leave alpha alone.
        let png = render_title(&TitleStyle { text: "Hi".into(), font_size: 120, bevel: "Cameo".into(), ..Default::default() }).await.unwrap();
        let img = image::open(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0[3], 0, "beveled card lost its transparency");
        assert!(
            img.pixels().filter(|p| p.0[3] > 250).count() < (0.2 * (w * h) as f64) as usize,
            "beveled card went opaque"
        );

        // A boxed caption is opaque behind its text by design, but the rest of
        // the frame still has to show the video through.
        let png = render_title(&TitleStyle { text: "Hi".into(), font_size: 64, y_frac: 0.72, boxed: true, ..Default::default() }).await.unwrap();
        let img = image::open(&png).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0).0[3], 0, "boxed card is opaque at the corner");
    }

    // The bevel shades the letters; it must never reshape them, and every knob
    // the inspector now exposes has to actually reach the renderer.
    #[tokio::test]
    async fn bevel_shades_glyphs_and_every_knob_bites() {
        let base = TitleStyle { text: "MM".into(), font_size: 200, ..Default::default() };
        let load = |p: String| image::open(p).unwrap().to_rgba8();
        let plain = load(render_title(&base).await.unwrap());
        let raised =
            load(render_title(&TitleStyle { bevel: "Cameo".into(), ..base.clone() }).await.unwrap());
        let sunken = load(
            render_title(&TitleStyle { bevel: "Intaglio".into(), ..base.clone() }).await.unwrap(),
        );

        // Shading only, never reshaping: raised and sunken run the same drawtext
        // through the same bevel with one sign flipped, so their alpha must be
        // byte-identical. (A bevel-off card is not comparable here — it also
        // gets the drop shadow that relief makes redundant.)
        let coverage = |i: &image::RgbaImage| i.pixels().map(|p| p.0[3] as u64).sum::<u64>();
        assert_eq!(coverage(&raised), coverage(&sunken), "bevel altered the glyph shape");

        // The point of the thing: white text comes out flat white, and a bevel
        // has to leave a range of light across the glyph. A bevel computed from
        // the wrong mask shades every letter uniformly and this spread stays 0.
        let spread = |i: &image::RgbaImage| {
            // Fully opaque only: antialiased edges blend text with the drop
            // shadow and would show a spread that is not the bevel's doing.
            let v: Vec<u8> = i.pixels().filter(|p| p.0[3] == 255).map(|p| p.0[0]).collect();
            v.iter().max().unwrap() - v.iter().min().unwrap()
        };
        assert!(spread(&plain) <= 2, "unbevelled white text should be flat, got {}", spread(&plain));
        assert!(spread(&raised) > 20, "raised bevel left the glyphs unshaded");
        assert!(spread(&sunken) > 20, "sunken bevel left the glyphs unshaded");
        // Raised vs sunken is a sign flip on the normals, so they cannot match.
        assert_ne!(raised.as_raw(), sunken.as_raw(), "raised and sunken render identically");

        for (what, tweak) in [
            ("size", TitleStyle { bevel_size: 60.0, ..base.clone() }),
            ("softness", TitleStyle { soften: 6.0, ..base.clone() }),
            ("depth", TitleStyle { depth: 20.0, ..base.clone() }),
            ("light angle", TitleStyle { angle: 300.0, ..base.clone() }),
            ("light height", TitleStyle { altitude: 80.0, ..base.clone() }),
            ("highlight strength", TitleStyle { hi_opacity: 0.1, ..base.clone() }),
            ("shadow strength", TitleStyle { sh_opacity: 0.1, ..base.clone() }),
        ] {
            let png = render_title(&TitleStyle { bevel: "Cameo".into(), ..tweak }).await.unwrap();
            assert_ne!(load(png).as_raw(), raised.as_raw(), "the {what} knob had no effect");
        }
    }

    #[tokio::test]
    async fn outline_thickens_the_text_without_a_backdrop() {
        let base = TitleStyle { text: "Hi".into(), font_size: 120, ..Default::default() };
        let load = |p: String| image::open(p).unwrap().to_rgba8();
        let bare = load(render_title(&base).await.unwrap());
        let outlined = load(
            render_title(&TitleStyle { outline: 8.0, outline_color: "black".into(), ..base })
                .await
                .unwrap(),
        );
        let painted = |i: &image::RgbaImage| i.pixels().filter(|p| p.0[3] > 200).count();
        assert!(
            painted(&outlined) > painted(&bare),
            "an outline should cover more of the card than bare text"
        );
        // Still a transparent card — an outline is not a backdrop.
        assert_eq!(outlined.get_pixel(0, 0).0[3], 0, "outline made the card opaque");
    }

    // Every format has its own codec branch, its own audio story and, for GIF,
    // its own output label — so each one gets encoded for real.
    #[tokio::test]
    async fn every_export_format_produces_a_playable_file() {
        let dir = std::env::temp_dir().join("morreel-format-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=30",
            "-f", "lavfi", "-i", "sine=duration=1",
            "-c:v", "libx264", "-c:a", "aac", "-shortest", &src,
        ]).await.unwrap();
        let clips = [ClipSpec {
            path: src,
            in_s: 0.0,
            out_s: 1.0,
            has_audio: true,
            ..Default::default()
        }];

        for format in Format::ALL.iter().copied() {
            let out = dir.join(format!("out.{}", format.ext()));
            let opts = ExportOpts { format, quality: Quality::Draft, ..Default::default() }
                .with_size(540);
            export(&clips, &[], &[], &[], &out, opts, |_| {})
                .await
                .unwrap_or_else(|e| panic!("{} export failed: {e}", format.label()));
            assert!(
                std::fs::metadata(&out).unwrap().len() > 0,
                "{} produced an empty file",
                format.label()
            );

            let streams = capture("ffprobe", &[
                "-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0",
                &out.display().to_string(),
            ]).await.unwrap();
            assert!(streams.contains("video"), "{} has no video", format.label());
            // GIF cannot carry sound, so the mix has to be dropped rather than
            // muxed — and dropping it must not break the filtergraph.
            assert_eq!(
                streams.contains("audio"),
                format.has_audio(),
                "{} audio presence is wrong: {streams}",
                format.label()
            );

            let dims = capture("ffprobe", &[
                "-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=width,height", "-of", "csv=p=0",
                &out.display().to_string(),
            ]).await.unwrap();
            assert_eq!(dims.trim(), "540,960", "{} ignored the size setting", format.label());
        }
    }

    #[test]
    fn export_option_labels_round_trip() {
        for f in Format::ALL {
            assert_eq!(Format::from_label(f.label()), *f);
            assert!(!f.blurb().is_empty() && !f.ext().is_empty());
        }
        for q in Quality::ALL {
            assert_eq!(Quality::from_label(q.label()), *q);
        }
        // Unknown labels fall back to something that works, never a panic.
        assert_eq!(Format::from_label("???"), Format::Mp4);
        assert_eq!(Quality::from_label("???"), Quality::Balanced);

        // Higher quality means a lower crf on every codec.
        for f in Format::ALL {
            let (_, draft) = Quality::Draft.encode(*f);
            let (_, high) = Quality::High.encode(*f);
            assert!(high < draft, "{} quality ladder is backwards", f.label());
        }

        // Size picks a real portrait pair; an unknown width falls back to full.
        assert_eq!(
            (ExportOpts::default().with_size(720).width, ExportOpts::default().with_size(720).height),
            (720, 1280)
        );
        assert_eq!(ExportOpts::default().with_size(999).width, 1080);
        assert!(ExportOpts::preview().width < 1080, "preview should render smaller");
    }

    #[test]
    fn filter_scales_only_when_the_size_differs() {
        let clips = [ClipSpec { out_s: 1.0, ..Default::default() }];
        let full = build_filter(&clips, &[], &[], &[], ExportOpts::default());
        assert!(full.contains("[vc]null[vout]"), "full size should not rescale: {full}");
        assert!(full.ends_with("[ac]anull[aout]"));

        let small = build_filter(&clips, &[], &[], &[], ExportOpts::default().with_size(720));
        assert!(small.contains("scale=720:1280"), "{small}");

        // GIF drains the mix into a sink and renames the video output, because
        // an unmapped [aout] is a hard filtergraph error.
        let gif = build_filter(
            &clips,
            &[],
            &[],
            &[],
            ExportOpts { format: Format::Gif, ..Default::default() },
        );
        assert!(gif.contains("anullsink") && !gif.contains("[aout]"), "{gif}");
        assert!(gif.contains("palettegen") && gif.ends_with("[gout]"), "{gif}");
    }

    // The point of the broadened tables plus "All files": if ffprobe can open
    // it, the editor can use it.
    #[tokio::test]
    async fn probe_handles_the_long_tail_of_containers() {
        let dir = std::env::temp_dir().join("morreel-container-test");
        std::fs::create_dir_all(&dir).unwrap();
        let at = |n: &str| dir.join(n).display().to_string();

        // An animated GIF is video, not a still — it probes with a real duration.
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "testsrc=duration=1:size=64x64:rate=10", &at("clip.gif"),
        ]).await.unwrap();
        assert!(!is_still(&at("clip.gif")), "gif must take the video path");
        let (d, a) = probe(&at("clip.gif")).await.unwrap();
        assert!(d > 0.5 && !a, "gif probed as {d}s audio={a}");

        // Ogg Vorbis and Matroska, neither of which the old lists accepted.
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi", "-i", "sine=duration=1", &at("music.ogg"),
        ]).await.unwrap();
        let (d, a) = probe(&at("music.ogg")).await.unwrap();
        assert!(d > 0.5 && a, "ogg probed as {d}s audio={a}");

        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=64x64:rate=10",
            "-f", "lavfi", "-i", "sine=duration=1",
            "-c:v", "libx264", "-c:a", "libvorbis", "-shortest", &at("movie.mkv"),
        ]).await.unwrap();
        let (d, a) = probe(&at("movie.mkv")).await.unwrap();
        assert!(d > 0.5 && a, "mkv probed as {d}s audio={a}");

        // An image whose extension the table doesn't list: no duration to read,
        // but it has a video stream, so the fallback imports it as a still.
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "testsrc=duration=1:size=64x64:rate=1", "-frames:v", "1", &at("odd.bin.png"),
        ]).await.unwrap();
        std::fs::copy(at("odd.bin.png"), at("mystery.xyz")).unwrap();
        assert!(!is_still(&at("mystery.xyz")), "unknown extension is not on the still fast path");
        assert_eq!(probe(&at("mystery.xyz")).await.unwrap(), (STILL_SOURCE, false));

        // Something that is not media at all still fails, and says so.
        std::fs::write(dir.join("notes.txt"), b"not media").unwrap();
        assert!(probe(&at("notes.txt")).await.is_err(), "a text file should not import");
    }

    #[test]
    fn extension_tables_are_disjoint_and_cover_the_obvious() {
        for e in ["mp4", "mov", "mkv", "webm", "avi", "gif"] {
            assert!(VIDEO_EXT.contains(&e), "{e} missing from the video list");
        }
        for e in ["png", "jpg", "jpeg", "webp", "heic", "avif"] {
            assert!(IMAGE_EXT.contains(&e), "{e} missing from the image list");
        }
        for e in ["mp3", "m4a", "wav", "flac", "ogg", "opus"] {
            assert!(AUDIO_EXT.contains(&e), "{e} missing from the audio list");
        }
        // A file cannot be two kinds at once, or is_still would disagree with
        // whichever lane the dialog put it on.
        for e in VIDEO_EXT {
            assert!(!IMAGE_EXT.contains(e), "{e} is listed as both video and image");
            assert!(!AUDIO_EXT.contains(e), "{e} is listed as both video and audio");
        }
        for e in IMAGE_EXT {
            assert!(!AUDIO_EXT.contains(e), "{e} is listed as both image and audio");
            assert!(is_still(&format!("x.{e}")), "{e} should take the still path");
        }
    }

    #[test]
    fn animated_transform_is_backcompat_and_samples_over_time() {
        use crate::keyframe::{Animated, Interp, Key};

        // A project written before keyframes existed stored a plain static
        // Transform. That exact JSON must still load into an AnimatedTransform…
        let legacy = r#"{"scale":0.5,"scale_x":1.0,"scale_y":1.0,"flip_h":true,
                         "flip_v":false,"x":0.1,"y":0.0,"rotation":0.0,"opacity":1.0}"#;
        let at: AnimatedTransform = serde_json::from_str(legacy).unwrap();
        // …as a fully static pose, unchanged from what was saved.
        assert_eq!(at.pose(), Transform { scale: 0.5, flip_h: true, x: 0.1, ..Default::default() });
        // A static AnimatedTransform serialises back to bare scalars, so new saves
        // are byte-identical to old ones for an un-keyframed clip.
        assert_eq!(serde_json::to_string(&at.scale).unwrap(), "0.5");

        // Sampling: a scale curve 1.0 → 1.2 is the Ken Burns zoom, and pose() is
        // the value at the clip's start.
        let mut anim = AnimatedTransform::default();
        anim.scale = Animated::curve(vec![
            Key { t: 0.0, v: 1.0, interp: Interp::Linear },
            Key { t: 2.0, v: 1.2, interp: Interp::Linear },
        ]);
        assert_eq!(anim.pose().scale, 1.0); // start
        assert_eq!(anim.at(1.0).scale, 1.1); // halfway
        assert_eq!(anim.at(2.0).scale, 1.2); // end

        // set_pose flattens any animation to constants (today's edit behaviour).
        anim.set_pose(Transform { scale: 0.8, ..Default::default() });
        assert_eq!(anim.at(0.0).scale, 0.8);
        assert_eq!(anim.at(9.0).scale, 0.8);
    }

    #[test]
    fn identity_transform_emits_nothing() {
        let t = Transform::default();
        assert!(t.is_identity());
        assert_eq!(transform_chain(&t, W, H, false), "", "an untouched clip must add no filter");
        assert_eq!(transform_chain(&t, W, H, true), "");
        // Any one knob off the default is no longer the identity.
        for t in [
            Transform { scale: 0.5, ..Default::default() },
            Transform { x: 0.1, ..Default::default() },
            Transform { y: -0.1, ..Default::default() },
            Transform { rotation: 90.0, ..Default::default() },
            Transform { opacity: 0.5, ..Default::default() },
        ] {
            assert!(!t.is_identity());
            assert!(!transform_chain(&t, W, H, false).is_empty());
        }
    }

    #[test]
    fn transform_crop_window_always_lands_inside_the_padding() {
        // The crop offsets are computed in Rust, not as ffmpeg expressions, so
        // this is the invariant that keeps ffmpeg from erroring at render time:
        // the window must sit fully inside the padded picture.
        for scale in [0.1, 0.25, 0.5, 1.0, 1.7, 4.0] {
            for x in [-1.0, -0.4, 0.0, 0.33, 1.0] {
                for y in [-1.0, 0.0, 0.75] {
                    let t = Transform { scale, x, y, ..Default::default() };
                    if t.is_identity() {
                        continue; // no chain to check
                    }
                    let chain = transform_chain(&t, W, H, false);
                    let pad = chain.split("pad=").nth(1).unwrap();
                    let (pw, ph): (i64, i64) = {
                        let mut it = pad.split(':');
                        (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap())
                    };
                    let crop = chain.split("crop=").nth(1).unwrap();
                    let n: Vec<i64> = crop
                        .split(',')
                        .next()
                        .unwrap()
                        .split(':')
                        .map(|v| v.parse().unwrap())
                        .collect();
                    let (cw, ch, cx, cy) = (n[0], n[1], n[2], n[3]);
                    assert_eq!((cw, ch), (W as i64, H as i64), "crop must end at frame size");
                    assert!(cx >= 0 && cy >= 0, "negative crop offset at {scale}/{x}/{y}: {chain}");
                    assert!(
                        cx + cw <= pw && cy + ch <= ph,
                        "crop runs off the padding at {scale}/{x}/{y}: {chain}"
                    );
                }
            }
        }
    }

    #[test]
    fn stretch_and_mirror_reach_the_chain() {
        // The master scale still moves both axes together.
        let uniform = transform_chain(
            &Transform { scale: 0.5, ..Default::default() },
            1000,
            2000,
            false,
        );
        assert!(uniform.contains("scale=500:1000"), "{uniform}");

        // Per-axis multipliers ride on top of it: half overall, then twice as
        // wide and half as tall again.
        let stretched = transform_chain(
            &Transform { scale: 0.5, scale_x: 2.0, scale_y: 0.5, ..Default::default() },
            1000,
            2000,
            false,
        );
        assert!(stretched.contains("scale=1000:500"), "{stretched}");

        // Mirroring happens before the geometry, so a flip does not fight the
        // rotation that follows it.
        let flipped = transform_chain(
            &Transform { flip_h: true, ..Default::default() },
            1000,
            2000,
            false,
        );
        assert!(flipped.starts_with("hflip,"), "{flipped}");
        let both = transform_chain(
            &Transform { flip_h: true, flip_v: true, rotation: 30.0, ..Default::default() },
            1000,
            2000,
            false,
        );
        let (h, v, rot) = (
            both.find("hflip").unwrap(),
            both.find("vflip").unwrap(),
            both.find("rotate").unwrap(),
        );
        assert!(h < v && v < rot, "mirroring must come before the rotation: {both}");

        // A mirror alone is still a transform worth emitting.
        assert!(!Transform { flip_v: true, ..Default::default() }.is_identity());
        assert!(!Transform { scale_x: 1.2, ..Default::default() }.is_identity());
    }

    // A stretched, mirrored, rotated clip has to survive ffmpeg, not just look
    // right as a string — the crop window is computed from all of it.
    #[tokio::test]
    async fn a_stretched_mirrored_clip_still_renders() {
        let dir = std::env::temp_dir().join("morreel-stretch-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "testsrc=duration=1:size=320x568:rate=30", "-c:v", "libx264", &src,
        ]).await.unwrap();

        for xf in [
            Transform { scale_x: 2.5, ..Default::default() },
            Transform { scale_y: 0.3, ..Default::default() },
            Transform { scale: 1.6, scale_x: 0.4, flip_h: true, ..Default::default() },
            Transform { scale_x: 3.0, scale_y: 0.4, rotation: 37.0, x: 0.3, y: -0.2, ..Default::default() },
        ] {
            let clips = [ClipSpec {
                path: src.clone(),
                out_s: 1.0,
                effect: transform_chain(&xf, W, H, false),
                ..Default::default()
            }];
            let out = dir.join("out.mp4");
            let opts = ExportOpts { quality: Quality::Draft, ..Default::default() }.with_size(540);
            export(&clips, &[], &[], &[], &out, opts, |_| {})
                .await
                .unwrap_or_else(|e| panic!("{xf:?} failed to render: {e}"));
            let dims = capture("ffprobe", &[
                "-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=width,height", "-of", "csv=p=0",
                &out.display().to_string(),
            ]).await.unwrap();
            assert_eq!(dims.trim(), "540,960", "{xf:?} came out the wrong size");
        }
    }

    #[test]
    fn transform_composites_only_where_it_has_something_to_composite_over() {
        let t = Transform { scale: 0.5, opacity: 0.5, ..Default::default() };
        // V1 sits on nothing, so it fills with black and ignores opacity.
        let v1 = transform_chain(&t, W, H, false);
        assert!(v1.contains("color=black") && !v1.contains("black@0"), "{v1}");
        assert!(!v1.contains("colorchannelmixer"), "opacity is meaningless on V1: {v1}");
        // V2 composites over V1, so it vacates to transparent — that is what
        // makes a scaled-down cutaway a picture-in-picture instead of a mask.
        let v2 = transform_chain(&t, W, H, true);
        assert!(v2.starts_with("format=rgba,"), "{v2}");
        assert!(v2.contains("color=black@0"), "{v2}");
        assert!(v2.contains("colorchannelmixer=aa=0.500"), "{v2}");
    }

    // The chains have to survive ffmpeg, not just look right.
    #[tokio::test]
    async fn transformed_overlay_renders_as_picture_in_picture() {
        let dir = std::env::temp_dir().join("morreel-transform-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4").display().to_string();
        let b = dir.join("b.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "color=c=red:size=320x568:duration=1:rate=30", "-c:v", "libx264", &a,
        ]).await.unwrap();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "color=c=lime:size=320x568:duration=1:rate=30", "-c:v", "libx264", &b,
        ]).await.unwrap();

        // A green cutaway shrunk into the corner over a red main track.
        let pip = Transform { scale: 0.4, x: 0.25, y: -0.3, rotation: 8.0, ..Default::default() };
        let clips = [ClipSpec { path: a, in_s: 0.0, out_s: 1.0, ..Default::default() }];
        let overlays = [OverlaySpec {
            path: b,
            in_s: 0.0,
            out_s: 1.0,
            at: 0.0,
            effect: transform_chain(&pip, W, H, true),
            framing: "Crop".into(),
            ..Default::default()
        }];
        let out = dir.join("pip.mp4");
        let opts = ExportOpts { quality: Quality::Draft, ..Default::default() };
        export(&clips, &overlays, &[], &[], &out, opts, |_| {}).await.unwrap();

        // Pull the middle frame and check both colours are on screen: the
        // cutaway is inset, so the main track must still be visible around it.
        let png = dir.join("frame.png");
        capture("ffmpeg", &[
            "-y", "-v", "error", "-ss", "0.5", "-i", &out.display().to_string(),
            "-frames:v", "1", &png.display().to_string(),
        ]).await.unwrap();
        let img = image::open(&png).unwrap().to_rgb8();
        let reddish = img.pixels().filter(|p| p.0[0] > 120 && p.0[1] < 90).count();
        let greenish = img.pixels().filter(|p| p.0[1] > 120 && p.0[0] < 90).count();
        assert!(greenish > 0, "the cutaway never made it into the frame");
        assert!(reddish > 0, "the cutaway covered everything — it did not shrink");
        // Scaled to 0.4, the cutaway should occupy well under half the frame.
        assert!(
            greenish < reddish,
            "inset cutaway should cover less than the main track: green={greenish} red={reddish}"
        );
    }

    #[test]
    fn overlay_speed_retimes_the_cutaway_and_its_window() {
        let base =
            OverlaySpec { path: "b.mp4".into(), in_s: 0.0, out_s: 4.0, at: 1.0, ..Default::default() };
        assert_eq!(base.trimmed(), 4.0);
        assert_eq!(OverlaySpec { speed: 2.0, ..base.clone() }.trimmed(), 2.0);
        assert_eq!(OverlaySpec { speed: 0.5, ..base.clone() }.trimmed(), 8.0);
        assert_eq!(OverlaySpec::default().speed, 1.0);

        // 4 s of source at 2x covers V1 for 2 s, so the window it is enabled
        // over has to shrink with it or the cutaway outstays its picture.
        let clips = [ClipSpec { path: "a.mp4".into(), out_s: 10.0, ..Default::default() }];
        let f = build_filter(
            &clips,
            &[OverlaySpec { speed: 2.0, ..base }],
            &[],
            &[],
            ExportOpts::default(),
        );
        assert!(f.contains("setpts=(PTS-STARTPTS)/2.0000+1.000/TB"), "not retimed: {f}");
        assert!(f.contains("enable='between(t,1.000,3.000)'"), "window not retimed: {f}");
    }

    #[test]
    fn transitions_shorten_the_timeline_by_what_they_overlap() {
        let c = |d: f64| ClipSpec { out_s: d, ..Default::default() };
        let cut = [c(5.0), c(4.0)];
        assert_eq!(timeline_len(&cut), 9.0, "a cut costs nothing");

        // A 1 s dissolve overlaps the two clips, so the reel is 1 s shorter and
        // the second clip starts 1 s earlier than it otherwise would.
        let faded = [
            c(5.0),
            ClipSpec { transition: "Cross dissolve".into(), trans_dur: 1.0, ..c(4.0) },
        ];
        assert_eq!(timeline_len(&faded), 8.0);
        assert_eq!(extents(&faded), vec![4.0, 4.0], "the outgoing clip owns less");

        // The first clip has nothing to blend from, so its transition is inert.
        let lead = [ClipSpec { transition: "Cross dissolve".into(), trans_dur: 1.0, ..c(5.0) }, c(4.0)];
        assert_eq!(timeline_len(&lead), 9.0);

        // "None" is a cut however long its duration says.
        let none = [c(5.0), ClipSpec { transition: "None".into(), trans_dur: 2.0, ..c(4.0) }];
        assert_eq!(timeline_len(&none), 9.0);

        // A transition longer than the clips it joins is clamped, never
        // negative — xfade's offset would go backwards and the render fail.
        let greedy = [c(1.0), ClipSpec { transition: "Wipe".into(), trans_dur: 30.0, ..c(1.0) }];
        assert!(timeline_len(&greedy) > 1.0, "clamped to something renderable");
        assert!(extents(&greedy).iter().all(|d| *d > 0.0));
    }

    #[test]
    fn the_filter_graph_only_changes_shape_when_a_transition_exists() {
        let c = |d: f64| ClipSpec { path: "a.mp4".into(), out_s: d, has_audio: true, ..Default::default() };
        // No transitions: the single concat that shipped before, untouched.
        let plain = build_filter(&[c(2.0), c(3.0)], &[], &[], &[], ExportOpts::default());
        assert!(plain.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"), "{plain}");
        assert!(!plain.contains("xfade"));

        // With one: pairwise, xfade for video and acrossfade for audio, and the
        // offset is where the incoming clip starts on the finished timeline.
        let faded = build_filter(
            &[c(2.0), ClipSpec { transition: "Cross dissolve".into(), trans_dur: 0.5, ..c(3.0) }],
            &[],
            &[],
            &[],
            ExportOpts::default(),
        );
        assert!(faded.contains("xfade=transition=fade:duration=0.500:offset=1.500"), "{faded}");
        assert!(faded.contains("acrossfade=d=0.500"), "{faded}");
        assert!(!faded.contains("concat=n=2:v=1:a=1"), "should not also concat: {faded}");

        // Mixed: a cut then a transition. The concat side has to be put back on
        // the frame clock or xfade rejects the mismatched timebase.
        let mixed = build_filter(
            &[c(2.0), c(2.0), ClipSpec { transition: "Wipe".into(), trans_dur: 0.5, ..c(2.0) }],
            &[],
            &[],
            &[],
            ExportOpts::default(),
        );
        assert!(mixed.contains("concat=n=2:v=1:a=0,settb=1/30"), "{mixed}");
        assert!(mixed.contains("xfade=transition=wiperight:duration=0.500:offset=3.500"), "{mixed}");
    }

    // Transitions have to survive a real encode, in the right length, with the
    // picture genuinely blended at the join.
    #[tokio::test]
    async fn a_crossfade_renders_blended_and_shortens_the_reel() {
        let dir = std::env::temp_dir().join("morreel-xfade-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        for colour in ["red", "lime"] {
            let p = dir.join(format!("{colour}.mp4")).display().to_string();
            capture("ffmpeg", &[
                "-y", "-v", "error", "-f", "lavfi",
                "-i", &format!("color=c={colour}:size=320x568:duration=3:rate=30"),
                "-f", "lavfi", "-i", "sine=duration=3",
                "-c:v", "libx264", "-c:a", "aac", "-shortest", &p,
            ]).await.unwrap();
            paths.push(p);
        }
        let clips = [
            ClipSpec { path: paths[0].clone(), out_s: 3.0, has_audio: true, ..Default::default() },
            ClipSpec {
                path: paths[1].clone(),
                out_s: 3.0,
                has_audio: true,
                transition: "Cross dissolve".into(),
                trans_dur: 1.0,
                ..Default::default()
            },
        ];
        let out = dir.join("xf.mp4");
        let opts = ExportOpts { quality: Quality::Draft, ..Default::default() }.with_size(540);
        export(&clips, &[], &[], &[], &out, opts, |_| {}).await.unwrap();

        let d: f64 = capture("ffprobe", &[
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap().trim().parse().unwrap();
        assert!((d - 5.0).abs() < 0.2, "3 s + 3 s with a 1 s dissolve should be 5 s, got {d}");

        // Halfway through the dissolve both colours must be present at once.
        let png = dir.join("mid.png");
        capture("ffmpeg", &[
            "-y", "-v", "error", "-ss", "2.5", "-i", &out.display().to_string(),
            "-frames:v", "1", &png.display().to_string(),
        ]).await.unwrap();
        let px = image::open(&png).unwrap().to_rgb8();
        let mid = px.get_pixel(px.width() / 2, px.height() / 2).0;
        assert!(mid[0] > 30 && mid[1] > 30, "mid-dissolve pixel is not a blend: {mid:?}");
    }

    // The app's whole promise is that a scrub shows what the export will. A
    // preview inside a transition therefore has to blend, not cut.
    #[tokio::test]
    async fn the_preview_blends_a_transition_like_the_export_does() {
        let dir = std::env::temp_dir().join("morreel-blendpreview-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut src = Vec::new();
        for colour in ["red", "lime"] {
            let p = dir.join(format!("{colour}.png")).display().to_string();
            capture("ffmpeg", &[
                "-y", "-v", "error", "-f", "lavfi",
                "-i", &format!("color=c={colour}:size=320x568:duration=1:rate=1"),
                "-frames:v", "1", &p,
            ]).await.unwrap();
            src.push(p);
        }
        let sample = |uri: String| -> [u8; 3] {
            let raw = uri.split_once(",").unwrap().1.to_string();
            let bytes = b64_decode(&raw);
            let img = image::load_from_memory(&bytes).unwrap().to_rgb8();
            img.get_pixel(img.width() / 2, img.height() / 2).0
        };

        // No blend: the base clip, unmixed.
        let base = frame_data_uri(&src[0], 0.0, 108, 192, "Crop", "", Over::default()).await.unwrap();
        let px = sample(base);
        assert!(px[0] > 200 && px[1] < 60, "base should be red, got {px:?}");

        // Halfway through, both colours are present — the same blend the
        // exported dissolve produced.
        let mixed = frame_data_uri(
            &src[0],
            0.0,
            108,
            192,
            "Crop",
            "",
            Over {
                blend: Some((src[1].clone(), 0.0, "Crop".into(), String::new(), 0.5)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let px = sample(mixed);
        assert!(px[0] > 40 && px[1] > 40, "mid-blend preview is not mixed: {px:?}");

        // Fully across, the incoming clip has replaced the outgoing one.
        let done = frame_data_uri(
            &src[0],
            0.0,
            108,
            192,
            "Crop",
            "",
            Over {
                blend: Some((src[1].clone(), 0.0, "Crop".into(), String::new(), 1.0)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let px = sample(done);
        assert!(px[1] > 200 && px[0] < 60, "end of blend should be the incoming clip: {px:?}");
    }

    /// Minimal base64 decode, for reading back what `b64` wrote in tests.
    fn b64_decode(s: &str) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = Vec::new();
        for c in s.bytes().filter(|c| *c != b'=') {
            bits.push(T.iter().position(|t| *t == c).unwrap() as u32);
        }
        let mut out = Vec::new();
        for chunk in bits.chunks(4) {
            let mut n = 0u32;
            for (i, v) in chunk.iter().enumerate() {
                n |= v << (18 - 6 * i);
            }
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..chunk.len() - 1]);
        }
        out
    }

    #[test]
    fn ducking_keys_each_bed_off_the_main_track() {
        let clips = [ClipSpec { path: "a.mp4".into(), out_s: 4.0, has_audio: true, ..Default::default() }];
        let bed = |duck: f64| AudioSpec {
            path: "m.mp3".into(),
            in_s: 0.0,
            out_s: 4.0,
            at: 0.0,
            volume: 1.0,
            duck,
            ..Default::default()
        };

        // Off: the mix is what it always was, with no sidechain anywhere.
        let plain = build_filter(&clips, &[], &[], &[bed(0.0)], ExportOpts::default());
        assert!(!plain.contains("sidechaincompress") && !plain.contains("asplit"), "{plain}");
        assert!(plain.contains("[ac][au0]amix=inputs=2"), "{plain}");

        // On: the main track is split so the compressor has something to key
        // from, and the mix takes the ducked copy rather than the raw bed.
        let ducked = build_filter(&clips, &[], &[], &[bed(0.8)], ExportOpts::default());
        assert!(ducked.contains("[ac]asplit=2[amain][akey0]"), "{ducked}");
        assert!(ducked.contains("[au0][akey0]sidechaincompress="), "{ducked}");
        assert!(ducked.contains("[amain][au0d]amix=inputs=2"), "{ducked}");

        // Two beds, one ducked: the split only serves the one that asked.
        let mixed = build_filter(&clips, &[], &[], &[bed(0.0), bed(0.5)], ExportOpts::default());
        assert!(mixed.contains("asplit=2[amain][akey1]"), "{mixed}");
        assert!(mixed.contains("[amain][au0][au1d]amix=inputs=3"), "{mixed}");

        // Harder ducking means a lower threshold and a steeper ratio.
        let (soft, hard) = (duck_chain(0.2), duck_chain(1.0));
        let thr = |c: &str| -> f64 {
            c.split("threshold=").nth(1).unwrap().split(':').next().unwrap().parse().unwrap()
        };
        assert!(thr(&hard) < thr(&soft), "harder ducking should trigger sooner");
    }

    #[test]
    fn audio_beds_get_fades_eq_denoise_and_volume_ramps() {
        let clips = [ClipSpec {
            path: "a.mp4".into(),
            out_s: 4.0,
            has_audio: true,
            ..Default::default()
        }];
        let bed = AudioSpec {
            path: "m.mp3".into(),
            in_s: 0.0,
            out_s: 4.0,
            at: 0.5,
            volume: 0.8,
            vol_end: 0.2,
            fade_in: 0.5,
            fade_out: 0.75,
            denoise: 0.5,
            compress: 0.4,
            treat: "Voice enhance".into(),
            duck: 0.0,
            lane: 1,
        };
        let f = build_filter(&clips, &[], &[], &[bed], ExportOpts::default());
        assert!(f.contains("afftdn=nr="), "denoise missing: {f}");
        assert!(f.contains("highpass=f=80"), "voice treat missing: {f}");
        assert!(f.contains("acompressor="), "compress missing: {f}");
        assert!(f.contains("volume='0.8000+(0.2000-0.8000)*t/"), "vol ramp missing: {f}");
        assert!(f.contains("afade=t=in:st=0:d=0.500"), "fade in missing: {f}");
        assert!(f.contains("afade=t=out:st=3.250:d=0.750"), "fade out missing: {f}");
        assert!(f.contains("adelay=500:all=1[au0]"), "sync delay missing: {f}");

        // Podcast already compresses — a second compressor would double-glue.
        let pod = AudioSpec {
            path: "v.m4a".into(),
            out_s: 2.0,
            treat: "Podcast".into(),
            compress: 1.0,
            ..Default::default()
        };
        let chain = a1_audio(0, 0, &pod);
        assert!(chain.contains("acompressor="));
        assert_eq!(
            chain.matches("acompressor=").count(),
            1,
            "podcast + compress slider must not stack: {chain}"
        );

        for name in AUDIO_TREATS {
            if *name == "None" {
                assert!(audio_treat_chain(name).is_empty());
            } else {
                assert!(!audio_treat_chain(name).is_empty(), "{name}");
            }
        }
    }

    // A reel with both a transition and background music: the main track's
    // label is no longer "ac" once clips are joined pairwise, and mixing
    // against a label that does not exist is a hard filtergraph error.
    #[tokio::test]
    async fn a_transition_and_a_music_bed_render_together() {
        let dir = std::env::temp_dir().join("morreel-transmix-test");
        std::fs::create_dir_all(&dir).unwrap();
        let v = dir.join("v.mp4").display().to_string();
        let m = dir.join("m.m4a").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=2:size=320x568:rate=30",
            "-f", "lavfi", "-i", "sine=duration=2",
            "-c:v", "libx264", "-c:a", "aac", "-shortest", &v,
        ]).await.unwrap();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=200:duration=4",
            "-c:a", "aac", &m,
        ]).await.unwrap();

        let clips = [
            ClipSpec { path: v.clone(), out_s: 2.0, has_audio: true, ..Default::default() },
            ClipSpec {
                path: v,
                out_s: 2.0,
                has_audio: true,
                transition: "Cross dissolve".into(),
                trans_dur: 0.5,
                ..Default::default()
            },
        ];
        let audio = [AudioSpec {
            path: m,
            in_s: 0.0,
            out_s: 3.0,
            at: 0.0,
            volume: 0.6,
            duck: 0.7,
            ..Default::default()
        }];
        let out = dir.join("out.mp4");
        let opts = ExportOpts { quality: Quality::Draft, ..Default::default() }.with_size(540);
        export(&clips, &[], &[], &audio, &out, opts, |_| {}).await.unwrap();

        let streams = capture("ffprobe", &[
            "-v", "error", "-show_entries", "stream=codec_type", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        assert!(streams.contains("audio") && streams.contains("video"), "{streams}");
        let d: f64 = capture("ffprobe", &[
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap().trim().parse().unwrap();
        assert!((d - 3.5).abs() < 0.2, "2 s + 2 s less a 0.5 s dissolve is 3.5 s, got {d}");
    }

    #[test]
    fn only_an_animated_title_gets_overlay_coordinates() {
        let still = TitleSpec { png: "t.png".into(), at: 1.0, dur: 3.0, anim: "None".into(), ..Default::default() };
        assert_eq!(still.overlay_xy(), "", "a still card needs no x/y at all");
        // An unknown name sits still rather than breaking the graph.
        assert_eq!(TitleSpec { anim: "wat".into(), ..still.clone() }.overlay_xy(), "");

        // Vertical slides move y and leave x alone, and vice versa.
        let up = TitleSpec { anim: "Slide up".into(), ..still.clone() }.overlay_xy();
        assert!(up.starts_with("x='0':y='") && up.ends_with(':'), "{up}");
        let left = TitleSpec { anim: "Slide in left".into(), ..still.clone() }.overlay_xy();
        assert!(left.contains("y='0'") && !left.contains("x='0'"), "{left}");

        // Up and down travel in opposite directions.
        let down = TitleSpec { anim: "Slide down".into(), ..still }.overlay_xy();
        assert!(up.contains("480.0*") && down.contains("-480.0*"), "up={up} down={down}");
    }

    // The expressions have to survive ffmpeg and actually move the card.
    #[tokio::test]
    async fn a_sliding_title_is_somewhere_else_at_the_start() {
        let dir = std::env::temp_dir().join("morreel-anim-test");
        std::fs::create_dir_all(&dir).unwrap();
        let bg = dir.join("bg.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error", "-f", "lavfi",
            "-i", "color=c=black:size=320x568:duration=3:rate=30", "-c:v", "libx264", &bg,
        ]).await.unwrap();
        let png = render_title(&TitleStyle {
            text: "SLIDE".into(),
            font_size: 150,
            ..Default::default()
        })
        .await
        .unwrap();

        let clips = [ClipSpec { path: bg, out_s: 3.0, ..Default::default() }];
        let mut frames = Vec::new();
        for anim in ["None", "Slide up"] {
            let titles = [TitleSpec { png: png.clone(), at: 0.0, dur: 3.0, anim: anim.into(), ..Default::default() }];
            let out = dir.join(format!("{}.mp4", anim.replace(' ', "_")));
            let opts = ExportOpts { quality: Quality::Draft, ..Default::default() }.with_size(540);
            export(&clips, &[], &titles, &[], &out, opts, |_| {}).await.unwrap();
            // Early in the card, where the slide has not yet settled.
            let f = dir.join(format!("{}.png", anim.replace(' ', "_")));
            capture("ffmpeg", &[
                "-y", "-v", "error", "-ss", "0.20", "-i", &out.display().to_string(),
                "-frames:v", "1", &f.display().to_string(),
            ]).await.unwrap();
            frames.push(image::open(&f).unwrap().to_rgb8());
        }
        // Text is white on black: the row it occupies tells us where it sits.
        let row_of_text = |img: &image::RgbImage| -> Option<u32> {
            (0..img.height()).find(|y| (0..img.width()).any(|x| img.get_pixel(x, *y).0[0] > 100))
        };
        let (fixed, slid) = (row_of_text(&frames[0]), row_of_text(&frames[1]));
        assert!(fixed.is_some(), "the still title never drew");
        assert_ne!(fixed, slid, "the sliding title is in the same place as the still one");
    }

    #[test]
    fn the_font_picker_offers_real_families_and_no_broken_ones() {
        let fams = font_families();
        // The generics stay first: they resolve on any machine.
        assert_eq!(&fams[..3], &["Sans".to_string(), "Serif".to_string(), "Mono".to_string()]);
        // Colour emoji faces are bitmap strikes drawtext cannot size, and
        // offering one would fail the whole render.
        assert!(font_is_unusable("Noto Color Emoji"));
        assert!(!font_is_unusable("DejaVu Sans"));
        assert!(
            !fams.iter().any(|f| font_is_unusable(f)),
            "an unusable face reached the picker"
        );
        assert!(fams.iter().all(|f| !f.trim().is_empty()));
    }

    // A shape has to actually paint alpha. drawbox writes colour and leaves
    // alpha alone, which on a transparent card is a fully coloured, completely
    // invisible box — this is the test that would catch that regression.
    #[tokio::test]
    async fn shapes_paint_alpha_where_they_should_and_nowhere_else() {
        let base = TitleStyle {
            kind: "Box".into(),
            color: "#E8C060".into(),
            shape_w: 0.5,
            shape_h: 0.2,
            y_frac: 0.5,
            ..Default::default()
        };
        let load = |p: String| image::open(p).unwrap().to_rgba8();

        let solid = load(render_title(&base).await.unwrap());
        let (w, h) = solid.dimensions();
        let centre = solid.get_pixel(w / 2, h / 2).0;
        assert_eq!(centre[3], 255, "the middle of a filled box must be opaque");
        assert_eq!((centre[0], centre[1], centre[2]), (232, 192, 96), "wrong colour: {centre:?}");
        assert_eq!(solid.get_pixel(2, 2).0[3], 0, "outside the box must stay transparent");

        // Roughly the right area: half the width by a fifth of the height.
        let painted = solid.pixels().filter(|p| p.0[3] > 200).count() as f64;
        let expected = 0.5 * 0.2 * (w * h) as f64;
        assert!((painted / expected - 1.0).abs() < 0.1, "box covers {painted}, wanted ~{expected}");

        // An outline hollows it out: the edge stays, the middle goes.
        let ring = load(render_title(&TitleStyle { outline: 12.0, ..base.clone() }).await.unwrap());
        assert_eq!(ring.get_pixel(w / 2, h / 2).0[3], 0, "an outlined box must be hollow");
        assert!(ring.pixels().filter(|p| p.0[3] > 200).count() > 0, "the ring itself vanished");

        // An ellipse fills its middle but not the corners of its bounding box.
        let ell = load(render_title(&TitleStyle { kind: "Ellipse".into(), ..base.clone() }).await.unwrap());
        assert_eq!(ell.get_pixel(w / 2, h / 2).0[3], 255);
        let corner_x = (w as f64 / 2.0 + 0.5 * w as f64 / 2.0 * 0.97) as u32;
        let corner_y = (h as f64 / 2.0 + 0.2 * h as f64 / 2.0 * 0.97) as u32;
        assert_eq!(ell.get_pixel(corner_x, corner_y).0[3], 0, "an ellipse should not fill corners");

        // A line ignores the outline and stays solid.
        let line = load(
            render_title(&TitleStyle { kind: "Line".into(), outline: 12.0, ..base }).await.unwrap(),
        );
        assert_eq!(line.get_pixel(w / 2, h / 2).0[3], 255, "a line is always solid");
    }

    #[test]
    fn curve_compiles_to_a_clamped_piecewise_ffmpeg_expression() {
        use crate::keyframe::{Animated, Interp, Key};

        // A constant is just its number — no if() machinery.
        assert_eq!(curve_expr(&Animated::Const(1.2), "it"), "1.20000");

        // A 1.0 → 1.2 linear zoom over 0..2s: clamps below t0 and above the last
        // key, and interpolates the segment in between.
        let z = curve_expr(
            &Animated::curve(vec![
                Key { t: 0.0, v: 1.0, interp: Interp::Linear },
                Key { t: 2.0, v: 1.2, interp: Interp::Linear },
            ]),
            "it",
        );
        assert!(z.contains("if(lt(it,0.00000),1.00000"), "missing pre-clamp: {z}");
        assert!(z.contains("if(lt(it,2.00000)"), "missing segment split: {z}");
        assert!(z.contains("1.20000"), "missing end value: {z}");

        // A static AnimatedTransform emits the proven static chain byte-for-byte.
        let s = AnimatedTransform::from(Transform { scale: 1.3, ..Default::default() });
        assert_eq!(s.chain(W, H, false), transform_chain(&s.pose(), W, H, false));
    }

    #[tokio::test]
    async fn a_keyframed_zoom_animates_in_the_shared_preview_export_chain() {
        let dir = std::env::temp_dir().join("morreel-kb-test");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("photo.png").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=800x600:rate=1",
            "-frames:v", "1", &png,
        ])
        .await
        .unwrap();

        // A Ken Burns zoom authored as a scale curve, 1.0 → 1.6 over 3s.
        let mut kb = AnimatedTransform::default();
        kb.scale = Animated::curve(vec![
            Key { t: 0.0, v: 1.0, interp: Interp::Linear },
            Key { t: 3.0, v: 1.6, interp: Interp::Linear },
        ]);
        let chain = kb.chain(W, H, false);
        assert!(chain.contains("zoompan"), "animated scale must compile to zoompan: {chain}");

        // The one chain feeds preview; rendered at two playhead times it must
        // differ (the zoom moved) — the whole point of preview == export.
        let early = frame_data_uri(&png, 0.0, 108, 192, "Crop", &chain, Over::default()).await.unwrap();
        let later = frame_data_uri(&png, 2.5, 108, 192, "Crop", &chain, Over::default()).await.unwrap();
        assert_ne!(early, later, "keyframed zoom froze at its opening pose");
    }

    #[test]
    fn shape_colours_come_out_as_numbers_geq_understands() {
        assert_eq!(rgb_of("black"), (0, 0, 0));
        assert_eq!(rgb_of("white"), (255, 255, 255));
        assert_eq!(rgb_of("#E8C060"), (232, 192, 96));
        assert_eq!(rgb_of("#3DD6D0"), (61, 214, 208));
        assert_eq!(rgb_of("nonsense"), (255, 255, 255), "fall back rather than break the render");
    }

    #[test]
    fn alignment_maps_to_drawtext_flags() {
        assert_eq!(align_flag("Centre"), "center");
        assert_eq!(align_flag("Left"), "left");
        assert_eq!(align_flag("Right"), "right");
        assert_eq!(align_flag("nonsense"), "center", "fall back rather than break the render");
    }

    #[test]
    fn still_classification() {
        for p in ["a.png", "A.JPG", "/x/y/photo.jpeg", "shot.webp", "s.TIFF", "b.bmp"] {
            assert!(is_still(p), "{p} should be a still");
        }
        for p in ["a.mp4", "b.mov", "c.mkv", "clip.webm", "noext", "trailing."] {
            assert!(!is_still(p), "{p} should not be a still");
        }
    }

    // A photo on V1: probes without a duration, skips the proxy, seeks to an
    // arbitrary time, and exports as a real span of video with a moranima
    // camera move over it.
    #[tokio::test]
    async fn still_clip_exports_as_animated_span() {
        let dir = std::env::temp_dir().join("morreel-still-test");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("photo.png").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=800x600:rate=1",
            "-frames:v", "1", &png,
        ]).await.unwrap();

        // No duration to read: it reports the nominal source span and no audio.
        assert_eq!(probe(&png).await.unwrap(), (STILL_SOURCE, false));
        // Proxying a still would just be a slower copy of it.
        assert_eq!(ensure_proxy(&png).await.unwrap(), png);
        // Seeking past the single frame still yields a frame (-loop 1).
        let uri = frame_data_uri(&png, 2.5, 108, 192, "Crop", "", Over::default()).await.unwrap();
        assert!(uri.starts_with("data:image/jpeg;base64,") && uri.len() > 100);

        let drift = "scale=1210:2150,crop=1080:1920:x='65+54*sin(0.628*t)':y='115+58*cos(0.408*t)',setsar=1";
        let clips = [ClipSpec {
            path: png,
            in_s: 0.0,
            out_s: 3.0,
            has_audio: false,
            effect: drift.into(),
            framing: "Crop".into(),
            ..Default::default()
        }];
        let out = dir.join("still.mp4");
        let mut last = 0.0;
        // Draft quality but full size: this test is about the still becoming a
        // real span of portrait video, not about the preview's half-size path.
        let opts = ExportOpts { quality: Quality::Draft, ..Default::default() };
        export(&clips, &[], &[], &[], &out, opts, |p| last = p).await.unwrap();
        assert_eq!(last, 1.0);

        let info = capture("ffprobe", &[
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height,nb_frames", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        // The looped still became 3 s of 30 fps portrait video, not one frame.
        assert!(info.trim().starts_with("1080,1920,"), "unexpected stream: {info}");
        let frames: u32 = info.trim().rsplit(',').next().unwrap().parse().unwrap();
        assert!((88..=92).contains(&frames), "expected ~90 frames, got {frames}");

        // The playback mix must not hang on an unused image input.
        let wav = dir.join("mix.wav");
        render_audio_mix(&clips, &[], &wav).await.unwrap();
        assert!(std::fs::metadata(&wav).unwrap().len() > 0);
    }

    // Scrubbing a time-based effect must show its motion, not its t=0 pose.
    // On a still there is no other cue that the playhead moved at all.
    #[tokio::test]
    async fn motion_effect_preview_follows_the_playhead() {
        let dir = std::env::temp_dir().join("morreel-motion-test");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("photo.png").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=800x600:rate=1",
            "-frames:v", "1", &png,
        ]).await.unwrap();

        let sway = "scale=1188:2112,rotate=0.035*sin(0.628*t):ow=1080:oh=1920,setsar=1";
        let early = frame_data_uri(&png, 0.0, 108, 192, "Crop", sway, Over::default()).await.unwrap();
        let later = frame_data_uri(&png, 2.5, 108, 192, "Crop", sway, Over::default()).await.unwrap();
        assert_ne!(early, later, "Sway froze at its opening pose");

        // With no effect the still is genuinely the same frame at any time.
        let plain_a = frame_data_uri(&png, 0.0, 108, 192, "Crop", "", Over::default()).await.unwrap();
        let plain_b = frame_data_uri(&png, 2.5, 108, 192, "Crop", "", Over::default()).await.unwrap();
        assert_eq!(plain_a, plain_b);

        // Effect + title together: the clock shift must not desync the overlay,
        // so the composite has to differ from the same frame without a title.
        let title = render_title(&TitleStyle { text: "Hi".into(), font_size: 90, ..Default::default() }).await.unwrap();
        let composed = frame_data_uri(&png, 2.5, 108, 192, "Crop", sway, Over { title: Some((title.clone(), 1.0)), ..Default::default() })
            .await
            .unwrap();
        assert!(composed.starts_with("data:image/jpeg;base64,"));
        assert_ne!(composed, later, "title never composited onto the effect frame");
    }

    #[tokio::test]
    async fn blur_framing_renders_through_the_real_graph() {
        let dir = std::env::temp_dir().join("morreel-blur-test");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("wide.png").display().to_string();
        // A landscape source — exactly what Blur is for.
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=1280x720:rate=1",
            "-frames:v", "1", &png,
        ])
        .await
        .unwrap();
        // Proves the split/overlay blur graph is valid ffmpeg once embedded in the
        // shared filter_complex — the whole reason the labels carry a tag.
        let uri = frame_data_uri(&png, 0.0, 540, 960, "Blur", "", Over::default()).await.unwrap();
        assert!(uri.starts_with("data:image/jpeg;base64,"), "blur framing produced no frame");
    }

    #[test]
    fn background_fills_the_pad_behind_a_banded_clip() {
        // A band (scaled short) on V1 pads with the chosen colour.
        let band = Transform { scale_y: 0.34, bg: Bg::White, ..Default::default() };
        let chain = transform_chain(&band, W, H, false);
        assert!(chain.contains("color=white"), "white background not in the pad: {chain}");
        // Default (and every pre-Bg project) stays black.
        assert!(transform_chain(&Transform { scale_y: 0.34, ..Default::default() }, W, H, false).contains("color=black"));
        // A composited overlay ignores bg — it must pad transparent, or a PiP
        // would black out the V1 clip beneath it.
        assert!(transform_chain(&Transform { scale: 0.5, bg: Bg::White, ..Default::default() }, W, H, true).contains("color=black@0"));
        // Background alone (full-frame clip) is still the identity — no padding,
        // nothing to fill, so no filter is emitted.
        assert!(Transform { bg: Bg::White, ..Default::default() }.is_identity());
    }

    #[test]
    fn cover_band_crops_undistorted_instead_of_squishing() {
        // A plain short band stretches the whole frame into the strip.
        let squished = transform_chain(&Transform { scale_y: 0.34, ..Default::default() }, W, H, false);
        assert!(squished.contains(&format!("scale={W}:{}", (H as f64 * 0.34).round() as u32 & !1)));
        assert!(!squished.contains("force_original_aspect_ratio"), "stretch must not crop");
        // The same band with cover keeps the picture's aspect: scale-to-cover the
        // box, then crop it — the frame_chain "Crop" idiom, so no vertical squish.
        let covered = transform_chain(&Transform { scale_y: 0.34, cover: true, ..Default::default() }, W, H, false);
        let sh = (H as f64 * 0.34).round() as u32 & !1;
        assert!(covered.contains(&format!("scale={W}:{sh}:force_original_aspect_ratio=increase,crop={W}:{sh}")), "{covered}");
    }

    #[test]
    fn align_snaps_the_box_by_its_own_height() {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        // The point of the whole thing: two bands of different heights, both
        // aligned to the bottom of the frame, land at *different* y — each half
        // its own height up from the edge. A fixed preset would put both at one y
        // and float the taller one off the bottom.
        let mut short = Transform { scale_y: 0.34, ..Default::default() };
        short.align_to(Align::Bottom, AlignBox::FRAME);
        assert!(near(short.y, 0.5 - 0.17), "short band bottom: {}", short.y);
        let mut tall = Transform { scale_y: 0.50, ..Default::default() };
        tall.align_to(Align::Bottom, AlignBox::FRAME);
        assert!(near(tall.y, 0.5 - 0.25), "tall band bottom: {}", tall.y);
        assert!(short.y != tall.y, "different heights must give different offsets");

        // Top to the safe area sits below the 8% header inset, not the frame edge.
        let mut t = Transform { scale_y: 0.34, ..Default::default() };
        t.align_to(Align::Top, AlignBox::SAFE);
        assert!(near(t.y, -0.5 + 0.08 + 0.17), "safe top: {}", t.y);

        // Centres are pure midpoints, independent of size; the untouched axis is
        // left exactly where it was.
        let mut c = Transform { scale: 0.5, x: 0.3, y: 0.3, ..Default::default() };
        c.align_to(Align::VCenter, AlignBox::FRAME);
        assert!(near(c.y, 0.0) && near(c.x, 0.3), "vcenter moved x: {c:?}");
        c.align_to(Align::HCenter, AlignBox::SAFE);
        assert!(near(c.x, (-0.5 + (0.5 - 0.18)) / 2.0), "safe hcenter: {}", c.x);

        // A right-aligned PiP tucks its right edge against the reference.
        let mut p = Transform { scale: 0.4, ..Default::default() };
        p.align_to(Align::Right, AlignBox::SAFE);
        assert!(near(p.x, (0.5 - 0.18) - 0.2), "safe right: {}", p.x);
    }

    #[test]
    fn framing_chains() {
        assert!(frame_chain("Fit", 1080, 1920, "c0").contains("pad=1080:1920"));
        assert!(frame_chain("Zoom", 1080, 1920, "c0").starts_with("scale=1620:2880"));
        // default and unknown names center-crop
        assert_eq!(frame_chain("", 1080, 1920, "c0"), frame_chain("Crop", 1080, 1920, "c0"));
        assert!(frame_chain("Crop", 1080, 1920, "c0").ends_with("crop=1080:1920"));

        // Blur fits over a blurred zoomed copy; the split labels carry the tag so
        // two clips in one filter_complex don't collide, and blur scales to size.
        let b0 = frame_chain("Blur", 1080, 1920, "c0");
        assert!(b0.contains("split=2[bgc0][fgc0]") && b0.contains("gblur=sigma=24.0"), "{b0}");
        assert!(b0.trim_end().ends_with("overlay=(W-w)/2:(H-h)/2"), "{b0}");
        let b1 = frame_chain("Blur", 1080, 1920, "c1");
        assert!(!b1.contains("[bgc0]"), "labels must be unique per item");
        // Smaller frame → gentler blur, so a thumbnail isn't a smear.
        assert!(frame_chain("Blur", 108, 192, "m").contains("gblur=sigma=2.4"));
    }

    #[test]
    fn audio_filter_shape() {
        let clips = [
            ClipSpec { path: "a.mp4".into(), in_s: 0.5, out_s: 2.0, has_audio: true, ..Default::default() },
            ClipSpec { path: "b.mp4".into(), in_s: 0.0, out_s: 1.0, has_audio: false, ..Default::default() },
        ];
        let audio = [AudioSpec {
            path: "m.mp3".into(),
            in_s: 0.0,
            out_s: 2.0,
            at: 1.0,
            volume: 0.5,
            ..Default::default()
        }];
        let f = build_audio_filter(&clips, &audio);
        assert!(f.contains("[0:a]atrim=start=0.500:end=2.000"));
        assert!(f.contains("anullsrc"));
        assert!(f.contains("[a0][a1]concat=n=2:v=0:a=1[ac]"));
        // audio inputs follow the clips: index 2, not 4 as in the full export graph
        assert!(f.contains("[2:a]") && f.contains("volume=0.500") && f.contains("adelay=1000:all=1[au0]"), "{f}");
        assert!(f.ends_with("amix=inputs=2:duration=first:normalize=0[aout]"));

        // no A1 degenerates to the concat fed straight through
        assert!(build_audio_filter(&clips, &[]).ends_with("[ac]anull[aout]"));
    }

    #[tokio::test]
    async fn proxy_builds_at_480p_and_caches() {
        let dir = std::env::temp_dir().join("morreel-proxy-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=640x360:rate=30",
            "-c:v", "libx264", &src,
        ]).await.unwrap();

        let p1 = ensure_proxy(&src).await.unwrap();
        let dims = capture("ffprobe", &[
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0", &p1,
        ]).await.unwrap();
        assert_eq!(dims.trim(), "854,480");
        // second call is a cache hit — same path, no rebuild
        assert_eq!(ensure_proxy(&src).await.unwrap(), p1);
        let _ = std::fs::remove_file(&p1);
    }

    // End-to-end: two generated clips (one silent, one landscape) -> portrait mp4.
    #[tokio::test]
    async fn export_smoke() {
        let dir = std::env::temp_dir().join("morreel-smoke");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.mp4").display().to_string();
        let b = dir.join("b.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=2:size=640x360:rate=30",
            "-f", "lavfi", "-i", "sine=duration=2",
            "-c:v", "libx264", "-c:a", "aac", "-shortest", &a,
        ]).await.unwrap();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=360x640:rate=30",
            "-c:v", "libx264", &b,
        ]).await.unwrap();

        let out = dir.join("out.mp4");
        let clips = [
            // landscape source letterboxed via Fit — export must stay 1080x1920
            ClipSpec { path: a.clone(), in_s: 0.5, out_s: 1.5, has_audio: true, effect: "hue=s=0".into(), framing: "Fit".into(), ..Default::default() },
            ClipSpec { path: b.clone(), in_s: 0.0, out_s: 1.0, has_audio: false, ..Default::default() },
        ];
        let overlays = [OverlaySpec { path: b, in_s: 0.0, out_s: 0.5, at: 0.2, effect: "vignette".into(), ..Default::default() }];
        let audio = [AudioSpec {
            path: a,
            in_s: 0.0,
            out_s: 1.0,
            at: 0.5,
            volume: 0.6,
            ..Default::default()
        }];
        // a beveled title card over the first second
        let png = render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, bevel: "Cameo".into(), ..Default::default() }).await.unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 0);
        // second call must be a cache hit
        assert_eq!(render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, bevel: "Cameo".into(), ..Default::default() }).await.unwrap(), png);
        // a boxed caption is a distinct render (backdrop baked in)
        let boxed = render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, boxed: true, ..Default::default() }).await.unwrap();
        assert_ne!(boxed, png);
        assert!(std::fs::metadata(&boxed).unwrap().len() > 0);
        let titles = [TitleSpec { png, at: 0.0, dur: 1.0, ..Default::default() }];
        let mut last = 0.0;
        export(&clips, &overlays, &titles, &audio, &out, ExportOpts::default(), |p| last = p).await.unwrap();
        assert_eq!(last, 1.0);

        // fast preview render (playback path) produces a playable file too
        let fast_out = dir.join("preview.mp4");
        export(&clips, &overlays, &titles, &audio, &fast_out, ExportOpts::preview(), |_| {}).await.unwrap();
        assert!(std::fs::metadata(&fast_out).unwrap().len() > 0);

        // in-app playback audio mix renders with the V1 timeline's duration
        let wav = dir.join("mix.wav");
        render_audio_mix(&clips, &audio, &wav).await.unwrap();
        let d = capture("ffprobe", &[
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
            &wav.display().to_string(),
        ]).await.unwrap();
        assert!((d.trim().parse::<f64>().unwrap() - 2.0).abs() < 0.15, "mix duration {d}");

        let dims = capture("ffprobe", &[
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        assert_eq!(dims.trim(), "1080,1920");
    }
}




