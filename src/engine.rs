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
pub const FPS: u32 = 30;

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
    /// The pivot that rotation turns around, as a fraction offset from the picture
    /// centre (same units as `x`/`y`). `(0,0)` = centre, so every existing project
    /// is unchanged. Like Final Cut's Anchor, Position places the anchor, so moving
    /// it shifts the picture; it is a set-once reference, not keyframed.
    #[serde(default)]
    pub anchor_x: f64,
    #[serde(default)]
    pub anchor_y: f64,
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
            anchor_x: 0.0,
            anchor_y: 0.0,
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
    /// Rotation pivot offset from centre — not animated, a pivot is a reference.
    #[serde(default)]
    pub anchor_x: f64,
    #[serde(default)]
    pub anchor_y: f64,
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
            anchor_x: p.anchor_x,
            anchor_y: p.anchor_y,
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
            anchor_x: self.anchor_x,
            anchor_y: self.anchor_y,
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
        // Opacity keyframes on a composited layer (alpha) animate independently of
        // geometry: colorchannelmixer can't take a time expression, so the tail
        // below multiplies the alpha plane by the opacity curve via geq. The
        // export now composes this look on a clip-local clock (see build_filter's
        // overlay reorder), so frame time `T` here is 0-based clip time — the same
        // clock the preview shifts to, keeping preview == export. Force the pose's
        // opacity to 1 in the geometry so it isn't also baked in as a constant.
        let fade = alpha && self.opacity.is_animated();
        let with_fade = |mut c: String| {
            if !fade {
                return c;
            }
            let a = curve_expr(&self.opacity, "T");
            if !c.is_empty() {
                c += ",";
            }
            c += &format!("format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='alpha(X,Y)*clip({a},0,1)'");
            c
        };
        // A keyframed rotation without a keyframed scale still takes the static
        // geometry (no zoompan), with the spin threaded through as an angle
        // expression — clip-local `t`, the same clock the fade above uses.
        let rot_expr =
            self.rotation.is_animated().then(|| format!("({})*PI/180", curve_expr(&self.rotation, "t")));
        if !self.scale.is_animated() {
            let p = if fade { Transform { opacity: 1.0, ..self.pose() } } else { self.pose() };
            return with_fade(transform_chain_rot(&p, w, h, alpha, rot_expr.as_deref()));
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
        // z clamped ≥1: zoompan can't zoom out past the source frame. The crop
        // window (iw/zoom × ih/zoom) is centred by default; an animated or offset
        // x/y pans it, following the same convention as the static path — a
        // positive `x` moves the picture right, so the window slides left. One
        // output pixel is (iw/zoom)/w source pixels, so a frame-fraction offset is
        // that fraction of the window, `off*(iw/zoom)`. zoompan clamps the window
        // inside the source, so an over-pan stops at the edge rather than reading
        // past it — the crop-inside-the-picture safety, kept.
        let pan = |axis: &Animated<f64>, dim: &str| -> String {
            let centre = format!("{dim}/2-({dim}/zoom/2)");
            if axis.is_animated() || axis.sample(0.0).abs() > 1e-6 {
                format!("{centre}-({})*({dim}/zoom)", curve_expr(axis, "it"))
            } else {
                centre
            }
        };
        c += &format!(
            "zoompan=z='max({z},1)':d=1:x='{}':y='{}':s={w}x{h}:fps=30",
            pan(&self.x, "iw"),
            pan(&self.y, "ih")
        );
        // ponytail: the anchor pivot isn't applied in the zoompan branch — this
        // rotate turns about the frame centre. Anchor + a keyframed *scale* at once
        // is the rare case; it waits on the same zoompan-geometry migration as the
        // rest. Anchor with keyframed *rotation* (the "swing in") takes the static
        // branch above, where it is honoured.
        match &rot_expr {
            Some(e) => c += &format!(",rotate=a='{e}':c=black"),
            None if p.rotation.abs() > 1e-6 => c += &format!(",rotate={:.5}:c=black", p.rotation.to_radians()),
            None => {}
        }
        if alpha && !fade && p.opacity < 0.999 {
            c += &format!(",colorchannelmixer=aa={:.3}", p.opacity.clamp(0.0, 1.0));
        }
        with_fade(c + ",setsar=1")
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
            // A moved anchor shifts the picture (Position places the anchor), so
            // it is not identity even with no rotation.
            && self.anchor_x.abs() < 1e-6
            && self.anchor_y.abs() < 1e-6
    }
}

/// ffmpeg chain for a transform, or "" when it is the identity.
///
/// `alpha` builds it for a layer that composites over something else: the area
/// the picture no longer covers has to be transparent, not black, or a
/// scaled-down V2 cutaway would black out the V1 clip it is supposed to sit on
/// top of. That is what makes picture-in-picture work.
pub fn transform_chain(t: &Transform, w: u32, h: u32, alpha: bool) -> String {
    transform_chain_rot(t, w, h, alpha, None)
}

/// As [`transform_chain`], but `rot_expr` — an ffmpeg angle expression in radians
/// using `t` (clip-local seconds) — overrides the constant rotation for a
/// keyframed spin. The `rotate` filter stays in the same mid-chain position, so
/// the geometry is byte-identical to the static path; only the angle varies with
/// time. `None` reproduces `transform_chain` exactly.
fn transform_chain_rot(t: &Transform, w: u32, h: u32, alpha: bool, rot_expr: Option<&str>) -> String {
    // An animated rotation is never "identity" even from a 0° start pose, so only
    // short-circuit when there's no spin to add.
    if rot_expr.is_none() && t.is_identity() {
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
    // Anchor: pad the scaled picture so the rotation pivot sits at the canvas
    // centre; the `rotate` below then turns about it. `bw`/`bh` is that canvas.
    // Position places the canvas centre (the anchor), so anchor (0,0) leaves
    // bw,bh == sw,sh and the chain stays byte-identical to before.
    let ax = (t.anchor_x * w as f64).round() as i64;
    let ay = (t.anchor_y * h as f64).round() as i64;
    let (bw, bh) = (sw + 2 * ax.unsigned_abs() as u32, sh + 2 * ay.unsigned_abs() as u32);
    // A rotation opens out to its full bounding box (the hypot ow/oh on the
    // rotate below), so the pads after it need room for the diagonal.
    let rotating = rot_expr.is_some() || t.rotation.abs() > 1e-6;
    let (need_w, need_h) = if rotating {
        let diag = (bw as f64).hypot(bh as f64) + 2.0;
        (diag, diag)
    } else {
        (bw as f64, bh as f64)
    };
    // Pad out to whatever the offset crop needs, so the picture can be moved
    // clean off the edge of the frame instead of jamming against it.
    let pw = even(need_w.max(w as f64 + 2.0 * dx.abs() as f64)).max(bw).max(w);
    let ph = even(need_h.max(h as f64 + 2.0 * dy.abs() as f64)).max(bh).max(h);
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
    // Anchor pad: extra room on the side away from the pivot, so the anchor lands
    // at the bw×bh centre and `rotate` (next) turns about it. Skipped at (0,0).
    if ax != 0 || ay != 0 {
        let pad_l = ax.unsigned_abs() as i64 - ax; // |ax|-ax → 0 (ax≥0) or 2|ax| (ax<0)
        let pad_t = ay.unsigned_abs() as i64 - ay;
        c += &format!(",pad={bw}:{bh}:{pad_l}:{pad_t}:color={fill}");
    }
    // The hypot output box keeps the turned picture's corners — clipping to the
    // input size would show an upright window of rotated content, visibly out of
    // line with the rotated box the on-screen transform handles draw.
    match rot_expr {
        Some(e) => c += &format!(",rotate=a='{e}':ow='hypot(iw,ih)':oh=ow:c={fill}"),
        None if t.rotation.abs() > 1e-6 => {
            c += &format!(",rotate={:.5}:ow='hypot(iw,ih)':oh=ow:c={fill}", t.rotation.to_radians())
        }
        None => {}
    }
    // Offsets as expressions, not numbers: after a rotate the input here is the
    // hypot box, not bw×bh, and centring must follow whatever arrived.
    c += &format!(",pad={pw}:{ph}:(ow-iw)/2:(oh-ih)/2:color={fill}");
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

/// A light primary colour grade — GIMP's Curves/Levels reduced to the four knobs
/// a phone reel actually reaches for, each mapped to what ffmpeg does cheaply in
/// one pass. It rides the same [`Clip::look`] chain as the effect presets — the
/// grade runs *before* the look, so you correct the exposure and then stylise on
/// top — which keeps it WYSIWYG and back-compatible: an old project with no
/// `grade` loads as the identity (every field `#[serde(default)]`), and the
/// identity emits no filter at all.
///
/// ponytail: static, not [`Animated`] — a graded reel wants one steady look, not
/// a colour that drifts. Make the fields curves the day a fade-to-mono needs it.
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Grade {
    /// `eq` brightness: -1..1, 0 = untouched.
    #[serde(default)]
    pub exposure: f64,
    /// `eq` contrast: 0..2, 1 = untouched.
    #[serde(default = "unit")]
    pub contrast: f64,
    /// `eq` saturation: 0..3, 1 = untouched.
    #[serde(default = "unit")]
    pub saturation: f64,
    /// White balance in Kelvin: below 6500 warms, above cools, 6500 = untouched.
    #[serde(default = "neutral_k")]
    pub warmth: f64,
}

fn neutral_k() -> f64 {
    6500.0
}

impl Default for Grade {
    fn default() -> Self {
        Self { exposure: 0.0, contrast: 1.0, saturation: 1.0, warmth: 6500.0 }
    }
}

impl Grade {
    /// True when every knob is at neutral — the whole grade emits nothing.
    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < 1e-4
            && (self.contrast - 1.0).abs() < 1e-4
            && (self.saturation - 1.0).abs() < 1e-4
            && (self.warmth - 6500.0).abs() < 1.0
    }

    /// ffmpeg chain for the grade, or "" when it is the identity. `eq` carries
    /// exposure/contrast/saturation together; `colortemperature` carries warmth.
    /// Each half is dropped at neutral, so a one-knob grade stays a one-filter
    /// chain.
    pub fn chain(&self) -> String {
        if self.is_identity() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.exposure.abs() >= 1e-4
            || (self.contrast - 1.0).abs() >= 1e-4
            || (self.saturation - 1.0).abs() >= 1e-4
        {
            parts.push(format!(
                "eq=brightness={:.3}:contrast={:.3}:saturation={:.3}",
                self.exposure, self.contrast, self.saturation
            ));
        }
        if (self.warmth - 6500.0).abs() >= 1.0 {
            parts.push(format!("colortemperature={:.0}", self.warmth));
        }
        parts.join(",")
    }
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

/// Still formats that take the `-loop 1` path (one frame, held for the Out span).
///
/// **Primary** (reel-native — phone photos, design exports, web stills): JPEG,
/// PNG, HEIF/HEIC, TIFF, BMP, WebP, AVIF. These are the formats worth naming
/// in UI copy and dialogs.
///
/// **Long tail**: single-frame containers ffmpeg already decodes (TGA, netpbm,
/// EXR, …). Kept because every dialog also offers **All files** and `probe`
/// imports anything with a video stream and no duration.
///
/// **Not product targets:** PDF (multi-page document), PSD (flatten to PNG
/// externally), camera RAW (export JPEG/HEIF first — demosaic is not our job).
/// GIF is video, not a still — see `VIDEO_EXT`.
pub const IMAGE_EXT: &[&str] = &[
    // Primary
    "png", "jpg", "jpeg", "jfif", "heic", "heif", "tif", "tiff", "bmp", "webp", "avif",
    // Long tail (free via ffmpeg; not marketed)
    "tga", "ppm", "pgm", "pbm", "pnm", "dds", "ico", "jp2", "j2k", "exr", "hdr", "qoi",
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
    #[cfg(target_os = "android")]
    {
        static FONTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        return FONTS.get_or_init(|| {
            let mut out = vec!["Sans".to_string(), "Serif".to_string(), "Mono".to_string()];
            let mut fams: Vec<String> =
                android_fonts().iter().map(|(fam, _)| fam.clone()).collect();
            fams.sort_by_key(|f| f.to_ascii_lowercase());
            fams.dedup();
            out.extend(fams);
            out
        });
    }
    #[cfg(not(target_os = "android"))]
    {
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
}

/// Android: no fontconfig — the system fonts are files in /system/fonts, and
/// the family shown in the UI is derived from the file name (CamelCase split
/// into words), which is also the name libass resolves when handed
/// `fontsdir=/system/fonts`. Returns (family, path), one entry per family,
/// preferring the -Regular face.
#[cfg(target_os = "android")]
fn android_fonts() -> &'static Vec<(String, String)> {
    static FONTS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let mut best: std::collections::BTreeMap<String, (i32, String)> = Default::default();
        let Ok(rd) = std::fs::read_dir("/system/fonts") else { return Vec::new() };
        for e in rd.flatten() {
            let path = e.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(stem) = name.strip_suffix(".ttf").or_else(|| name.strip_suffix(".otf"))
            else {
                continue;
            };
            // "NotoSerif[wght]" → "NotoSerif"; "Roboto-Regular" → base + face.
            let stem = stem.split('[').next().unwrap_or(stem);
            let (base, face) = match stem.split_once('-') {
                Some((b, f)) => (b, f),
                None => (stem, "Regular"),
            };
            if base.is_empty() || font_is_unusable(base) {
                continue;
            }
            // Prefer Regular, then unadorned variable files, then anything.
            let rank = match face {
                "Regular" => 0,
                _ if face.eq_ignore_ascii_case("VF") => 1,
                _ => 2,
            };
            let family = camel_words(base);
            let path = path.display().to_string();
            match best.get(&family) {
                Some((r, _)) if *r <= rank => {}
                _ => {
                    best.insert(family, (rank, path));
                }
            }
        }
        best.into_iter().map(|(fam, (_, p))| (fam, p)).collect()
    })
}

/// "DancingScript" → "Dancing Script", "CarroisGothicSC" → "Carrois Gothic SC".
#[cfg(target_os = "android")]
fn camel_words(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0
            && c.is_ascii_uppercase()
            && (chars[i - 1].is_ascii_lowercase()
                || (chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())
                    && chars[i - 1].is_ascii_uppercase()))
        {
            out.push(' ');
        }
        out.push(*c);
    }
    out
}

/// The generic families every project can name, resolved to what this phone
/// actually ships; anything else looked up as-is.
#[cfg(target_os = "android")]
fn android_family(family: &str) -> &str {
    match family {
        "" | "Sans" => "Roboto",
        "Serif" => "Noto Serif",
        "Mono" => "Cutive Mono",
        other => other,
    }
}

/// The font file for a family — drawtext takes `fontfile=` because there is
/// no fontconfig to resolve `font=` names. Falls back to Roboto.
#[cfg(target_os = "android")]
fn android_font_file(family: &str) -> String {
    let want = android_family(family);
    android_fonts()
        .iter()
        .find(|(fam, _)| fam == want)
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| "/system/fonts/Roboto-Regular.ttf".to_string())
}

/// What a T-lane card actually is. Shapes ride the title lane because they
/// need exactly what a title needs — a rasterized card, a place on the
/// timeline, a fade — and nothing a title does not.
pub const TITLE_KINDS: &[&str] = &["Text", "Box", "Ellipse", "Line"];

/// An ffmpeg colour name or `#RRGGBB` as an `(r, g, b)` triple — for geq and ASS,
/// which cannot take the names drawtext accepts. Only what the colour picker can
/// produce is covered; anything else falls back to white rather than break a render.
fn hex_rgb(color: &str) -> (u32, u32, u32) {
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
    let (r, g, b) = hex_rgb(&s.color);
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
    /// Colour behind the picture where it doesn't fill 9:16 — fills the "Fit"
    /// letterbox. (A banded/shrunk clip's surround is filled by its transform
    /// pad separately, from the same `Bg`.)
    pub bg: Bg,
    /// Playback rate: 0.5 is half speed (slow motion), 2.0 is double.
    pub speed: f64,
    /// Play the trimmed span backwards (video and its audio).
    pub reverse: bool,
    /// Gain on this clip's own audio; 0.0 mutes it.
    pub volume: f64,
    /// Spectral denoise strength 0..=1 (`afftdn`), 0 = off. iMovie's "Reduce
    /// background noise" for the clip's own audio.
    pub denoise: f64,
    /// EQ / voice treatment label from [`AUDIO_TREATS`]; "None" = flat.
    pub treat: String,
    /// Transition *into* this clip, by menu label. Ignored on the first clip —
    /// there is nothing before it to blend from.
    pub transition: String,
    /// How long that transition runs. 0 means a straight cut.
    pub trans_dur: f64,
    /// When false, the clip keeps its timeline span but contributes black
    /// video and silence (FCP Clip › Disable). Still occupies duration so
    /// neighbours do not ripple.
    pub enabled: bool,
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
            bg: Bg::Black,
            speed: 1.0,
            reverse: false,
            volume: 1.0,
            denoise: 0.0,
            treat: "None".to_string(),
            transition: String::new(),
            trans_dur: 0.0,
            enabled: true,
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

    /// Rough output-size guess for the share dialog's info strip — bitrate
    /// heuristics per format/quality, labelled "est." in the UI.
    /// ponytail: CRF output swings ±2× with content; a guess beats silence.
    pub fn est_bytes(&self, secs: f64) -> u64 {
        let px_per_s = self.width as f64 * self.height as f64 * FPS as f64;
        // Bits per pixel per frame. GIF ignores quality (palette, no CRF) and
        // is honestly huge at full fps — the estimate is the warning.
        let bpp = match (self.format, self.quality) {
            (Format::Gif, _) => 1.6,
            (Format::WebM, Quality::Draft) => 0.035,
            (Format::WebM, Quality::Balanced) => 0.07,
            (Format::WebM, Quality::High) => 0.14,
            (_, Quality::Draft) => 0.05,
            (_, Quality::Balanced) => 0.10,
            (_, Quality::High) => 0.20,
        };
        let audio_bps = match self.format {
            Format::Mp4 => 192_000.0,
            Format::WebM => 128_000.0,
            Format::Gif => 0.0,
        };
        ((px_per_s * bpp + audio_bps) / 8.0 * secs.max(0.0)) as u64
    }
}

/// Cooperative cancel for the long renders (export / preview / transcribe).
/// The UI's Cancel button sets it; each job clears it when it starts.
static RENDER_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cancel_render() {
    RENDER_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn render_cancelled() -> bool {
    RENDER_CANCEL.load(std::sync::atomic::Ordering::Relaxed)
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
    /// Play the trimmed span backwards.
    pub reverse: bool,
    /// ffmpeg `blend` mode ("screen", "addition", …) to composite this layer with
    /// instead of the default alpha-over. Empty = normal overlay. For light leaks
    /// and particle plates on black, "screen"/"addition" brighten V1 through them.
    pub blend: String,
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
            reverse: false,
            blend: String::new(),
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
    "Chipmunk",
    "Deep voice",
    "Robot",
    "Megaphone",
    "Echo",
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
        // Voice-changer pair: relabel the rate to shift pitch, then atempo the
        // duration back so the item still lines up on the timeline. The leading
        // aresample pins the rate the asetrate math assumes.
        "Chipmunk" => "aresample=48000,asetrate=72000,aresample=48000,atempo=0.66667",
        "Deep voice" => "aresample=48000,asetrate=36000,aresample=48000,atempo=1.33333",
        // Frequency shift breaks the harmonic series — metallic, robotic ring.
        "Robot" => "afreqshift=shift=250",
        "Megaphone" => {
            "highpass=f=500,lowpass=f=2200,\
             acompressor=threshold=-18dB:ratio=8:attack=2:release=60:makeup=6"
        }
        "Echo" => "aecho=0.8:0.7:120:0.35",
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
    /// Spectral denoise strength 0..=1 (`afftdn` `nr`, mapped to 4..=24 dB).
    pub denoise: f64,
    /// Noise floor in dB, afftdn `nf` (−80..=−20). The sensitivity knob:
    /// higher (closer to −20) treats more of the signal as noise.
    pub noise_floor: f64,
    /// Adaptively track the noise floor over the clip (afftdn `tn`). ffmpeg's
    /// stand-in for a learned noise profile — no fixed noise-only region needed.
    pub track_noise: bool,
    /// Broadband compression amount 0..=1 (skipped when the treatment already
    /// bakes a compressor, e.g. Podcast).
    pub compress: f64,
    /// Noise gate amount 0..=1 (`agate`). Silences everything below a threshold
    /// that rises with the knob — kills room tone between words on phone VO.
    pub gate: f64,
    /// De-click strength 0..=1 (`adeclick`). Repairs pops/clicks in field audio.
    pub declick: f64,
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
            noise_floor: -25.0,
            track_noise: false,
            compress: 0.0,
            gate: 0.0,
            declick: 0.0,
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
    // De-click first: repair transients before denoise smears them. Lower
    // threshold catches more clicks, so aggressive = lower.
    let dc = a.declick.clamp(0.0, 1.0);
    if dc > 0.001 {
        parts.push(format!("adeclick=threshold={:.2}", 3.5 - 2.0 * dc));
    }
    let d = a.denoise.clamp(0.0, 1.0);
    if d > 0.001 {
        // afftdn: nr is noise reduction in dB (gentle → aggressive); nf is the
        // noise floor (sensitivity); tn adaptively tracks the noise over time.
        let mut fx = format!(
            "afftdn=nr={:.1}:nf={:.0}",
            4.0 + 20.0 * d,
            a.noise_floor.clamp(-80.0, -20.0)
        );
        if a.track_noise {
            fx.push_str(":tn=1");
        }
        parts.push(fx);
    }
    // Gate after denoise, before EQ/compress — so the compressor never pumps on
    // room tone the gate is about to remove. Threshold rises from −50 dB (gentle)
    // to −20 dB (aggressive); a moderate ratio + slow release keeps word tails.
    let g = a.gate.clamp(0.0, 1.0);
    if g > 0.001 {
        let thr_db = -50.0 + 30.0 * g;
        parts.push(format!(
            "agate=threshold={:.5}:ratio=3:attack=5:release=150",
            10f64.powf(thr_db / 20.0)
        ));
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
        // `len`: how long the card takes to arrive and leave, tied to the alpha
        // fade so the movement and the fade finish together.
        let (a, d, len) = (self.at, self.dur, title_fade(self.dur).max(0.01));
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
    /// Backdrop-box opacity, 0..1 — the caption plate's punch.
    pub box_opacity: f64,
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
            box_opacity: 0.45,
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
            self.box_opacity,
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
    // re-rendered instead of served stale. v5: adds shapes. v6: supersampled
    // emboss + soft cast shadow + specular sheen.
    const CACHE_VER: u32 = 6;
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
        let out = Command::new(ffmpeg_bin())
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
        // Shapes are straight-edged, so they don't need the text supersample.
        return finish_title(&png, s, 1);
    }

    // textfile= sidesteps drawtext's escaping rules entirely.
    let txt = png.with_extension("txt");
    // A literal \n in the text box is a line break. The kit's text input is a
    // single line, so this is the only way to type one, and drawtext reads the
    // file verbatim.
    std::fs::write(&txt, s.text.replace("\\n", "\n")).map_err(|e| e.to_string())?;
    // Anything that already carries legibility — a backdrop box, an outline,
    // the bevel's own relief — makes the drop shadow redundant.
    // Supersample beveled text so the emboss antialiases: draw at 2x, emboss at
    // 2x, then downscale in finish_title. Thin strokes then survive the medial-
    // axis relief instead of breaking into striations.
    let ss: u32 = if s.bevel != "Off" { 2 } else { 1 };
    let (cw, ch) = (W * ss, H * ss);
    let fs = s.font_size * ss;
    let plain = s.bevel == "Off" && !s.boxed && s.outline <= 0.0;
    let shadow = if plain { ":shadowcolor=black@0.5:shadowx=3:shadowy=3" } else { "" };
    let boxp = if s.boxed {
        format!(":box=1:boxcolor=black@{:.3}:boxborderw={}", s.box_opacity.clamp(0.0, 1.0), 18 * ss)
    } else {
        String::new()
    };
    let border = if s.outline > 0.0 {
        format!(":borderw={:.0}:bordercolor={}", s.outline * ss as f64, s.outline_color)
    } else {
        String::new()
    };
    #[cfg(not(target_os = "android"))]
    let fontp = if s.font.is_empty() { String::new() } else { format!(":font='{}'", s.font) };
    // No fontconfig on Android: drawtext must be handed the file itself.
    #[cfg(target_os = "android")]
    let fontp = format!(":fontfile={}", android_font_file(&s.font));
    let vf = format!(
        "drawtext=textfile={}{fontp}:fontsize={fs}:fontcolor={}:text_align={}\
         :x=(w-text_w)/2:y=(h-text_h)*{:.3}{shadow}{boxp}{border}",
        txt.display(),
        s.color,
        align_flag(&s.align),
        s.y_frac
    );
    let out = Command::new(ffmpeg_bin())
        // format=rgba has to be part of the *input* chain. Left to itself the
        // lavfi color source negotiates an opaque pixel format, the @0.0 alpha
        // is thrown away, and a later format=rgba refills alpha at 255 — which
        // turns every title card into a black rectangle over the whole frame.
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black@0.0:s={cw}x{ch},format=rgba"))
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

    finish_title(&png, s, ss)
}

/// Bake the bevel, if any, into a rasterized card. Shared by text and shapes —
/// an embossed box is as reasonable as embossed type.
fn finish_title(png: &std::path::Path, s: &TitleStyle, ss: u32) -> Result<String, String> {
    if s.bevel == "Off" {
        return Ok(png.display().to_string());
    }
    let img = image::open(png).map_err(|e| e.to_string())?;
    let mut rgba = img.to_rgba8();
    let (w, h_px) = rgba.dimensions();
    let n = (w * h_px) as usize;
    // Emboss at the supersampled scale: rim and softening thicken with `ss` so the
    // downscaled result keeps the thickness the user dialed in.
    let result = crate::bevel::compute_bevel(
        rgba.as_raw(),
        w,
        h_px,
        &crate::bevel::BevelParams {
            size: s.bevel_size.max(1.0) as u32 * ss,
            soften: s.soften.max(0.0) as u32 * ss,
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
    // Alpha is untouched here: the bevel shades the glyphs, it never grows them.
    {
        let buf = rgba.as_mut();
        for i in 0..n {
            let hi_a = result.hi_rgba[i * 4 + 3] as f32 / 255.0;
            let sh_a = result.sh_rgba[i * 4 + 3] as f32 / 255.0;
            for c in 0..3 {
                let shadowed = buf[i * 4 + c] as f32 * (1.0 - sh_a);
                buf[i * 4 + c] = (shadowed + hi_a * (255.0 - shadowed)) as u8;
            }
        }
    }
    // Soft cast shadow *under* the glyph — a blurred, offset copy of its own
    // alpha, black at ~0.5 — the lift the reference cards sit on. Turning the
    // bevel on used to drop the shadow entirely. Skipped when boxed: the backdrop
    // plate already lifts the text. This one grows alpha, on purpose.
    if !s.boxed {
        let (wu, hu) = (w as usize, h_px as usize);
        let off = (3 * ss) as isize;
        let mut sha = vec![0.0_f32; n];
        {
            let buf = rgba.as_raw();
            for y in 0..h_px as isize {
                for x in 0..w as isize {
                    let (sx, sy) = (x - off, y - off);
                    if sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h_px {
                        sha[(y * w as isize + x) as usize] =
                            buf[((sy as u32 * w + sx as u32) as usize) * 4 + 3] as f32 / 255.0;
                    }
                }
            }
        }
        crate::bevel::gaussian_blur(&mut sha, wu, hu, 3.0 * ss as f32);
        let buf = rgba.as_mut();
        for i in 0..n {
            let ga = buf[i * 4 + 3] as f32 / 255.0;
            let sa = (sha[i] * 0.5).min(1.0) * (1.0 - ga); // hidden where the glyph covers
            let out_a = (ga + sa).min(1.0);
            if out_a > 0.0 {
                // glyph over black shadow, premultiplied then un-premultiplied
                for c in 0..3 {
                    let gp = buf[i * 4 + c] as f32 / 255.0 * ga;
                    buf[i * 4 + c] = (gp / out_a * 255.0) as u8;
                }
            }
            buf[i * 4 + 3] = (out_a * 255.0) as u8;
        }
    }
    // Back down to 1080×1920 — the downscale is where the supersampled emboss
    // resolves into clean antialiased edges.
    if ss > 1 {
        image::imageops::resize(&rgba, W, H, image::imageops::FilterType::Lanczos3)
            .save(png)
            .map_err(|e| e.to_string())?;
    } else {
        rgba.save(png).map_err(|e| e.to_string())?;
    }
    Ok(png.display().to_string())
}

/// An ffmpeg colour ("white", "black", "#RRGGBB") as an ASS `&HAABBGGRR&`
/// literal. ASS packs the bytes reversed (BGR) and reads the leading byte as
/// *transparency* — 00 is fully opaque. Only the handful of names the colour
/// picker can produce, plus hex, need covering.
fn ass_color(c: &str, alpha: u8) -> String {
    let (r, g, b) = hex_rgb(c);
    format!("&H{alpha:02X}{b:02X}{g:02X}{r:02X}&")
}

/// Rasterize one karaoke frame: the whole caption line with word `active`
/// recoloured to `hi_color`, drawn through libass — which lays the type out and
/// wraps it on its own. One PNG per word-state, cached and content-addressed
/// exactly like [`render_title`], so a karaoke card rides the same per-card
/// overlay path a word-by-word reveal already uses. The custom bevel is not
/// available here — libass owns the glyph rendering.
// ponytail: vertical placement approximates drawtext's — an ASS top-anchor at
// (H-text_h)*y_frac with text_h estimated from font size and line count, since
// the exact laid-out height isn't known until libass runs. Off by well under a
// line; upgrade to a two-pass measure only if a caption ever visibly clips.
pub async fn render_karaoke(s: &TitleStyle, active: usize, hi_color: &str) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    active.hash(&mut hasher);
    hi_color.hash(&mut hasher);
    "kara-v1".hash(&mut hasher);
    let dir = cache_dir("titles");
    let png = dir.join(format!("kara-{:016x}.png", hasher.finish()));
    if png.exists() {
        return Ok(png.display().to_string());
    }

    // Colour every word: each opens with an explicit colour override, so the
    // active one is highlighted and the rest carry the base colour regardless of
    // order. Whitespace runs collapse to a single space; newlines force a break.
    let base = ass_color(&s.color, 0);
    let hi = ass_color(hi_color, 0);
    let mut body = String::new();
    let mut idx = 0usize;
    let mut prev_ws = true;
    let mut pending_break = false;
    for ch in s.text.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                idx += 1;
            }
            if ch == '\n' {
                pending_break = true;
            }
            prev_ws = true;
            continue;
        }
        if prev_ws {
            if !body.is_empty() {
                body.push_str(if pending_break { "\\N" } else { " " });
            }
            pending_break = false;
            body.push_str(if idx == active { &hi } else { &base });
        }
        prev_ws = false;
        // Braces open an override block and would swallow the text; drop them.
        // Caption text otherwise carries no ASS metacharacters.
        if ch != '{' && ch != '}' {
            body.push(ch);
        }
    }

    let lines = body.matches("\\N").count() + 1;
    let text_h = s.font_size as f64 * 1.35 * lines as f64;
    let top = ((H as f64 - text_h) * s.y_frac).max(0.0);
    // Top-anchored so only the block's top position is needed (no laid-out
    // height). Horizontal component also sets multi-line justification.
    let (an, x) = match s.align.as_str() {
        "Left" => (7, 60.0),
        "Right" => (9, 1020.0),
        _ => (8, W as f64 / 2.0),
    };

    // Box wins over outline: an opaque plate (BorderStyle=3) is the caption look,
    // and a plain outline (BorderStyle=1) is the transparent-friendly one.
    let (border_style, outline, outline_col, back_col) = if s.boxed {
        let a = ((1.0 - s.box_opacity.clamp(0.0, 1.0)) * 255.0) as u8;
        (3, 12.0, ass_color("black", 0), ass_color("black", a))
    } else if s.outline > 0.0 {
        (1, s.outline, ass_color(&s.outline_color, 0), ass_color("black", 0))
    } else {
        (1, 0.0, ass_color("black", 0), ass_color("black", 0))
    };

    let ass = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {W}\nPlayResY: {H}\nWrapStyle: 0\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, \
         Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, \
         Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: D,{font},{size},{base},&H000000FF&,{outline_col},{back_col},1,0,0,0,100,100,0,0,\
         {border_style},{outline},0,{an},40,40,40,1\n\n\
         [Events]\n\
         Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,0:00:00.00,0:00:10.00,D,,0,0,0,,{{\\an{an}\\pos({x:.0},{top:.0})}}{body}\n",
        font = {
            let f = if s.font.is_empty() { "Sans" } else { &s.font };
            #[cfg(target_os = "android")]
            let f = android_family(f);
            f
        },
        size = s.font_size,
    );
    let assp = png.with_extension("ass");
    std::fs::write(&assp, ass).map_err(|e| e.to_string())?;
    let out = Command::new(ffmpeg_bin())
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black@0.0:s={W}x{H},format=rgba"))
        .arg("-vf")
        .arg(format!(
            "ass=filename={}:original_size={W}x{H}:alpha=1{}",
            assp.display(),
            // libass has no fontconfig on Android; point it at the system fonts.
            if cfg!(target_os = "android") { ":fontsdir=/system/fonts" } else { "" }
        ))
        .args(["-frames:v", "1", "-pix_fmt", "rgba"])
        .arg(&png)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    let _ = std::fs::remove_file(&assp);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(png.display().to_string())
}

/// Resolve a media tool to something spawnable. Desktop: the PATH name,
/// unchanged. Android: the APK ships each tool as `lib<name>.so` in the native
/// library dir — the one place Android still allows exec — so resolve there.
pub fn ffmpeg_bin() -> String {
    tool_bin("ffmpeg")
}

#[cfg(not(target_os = "android"))]
fn tool_bin(name: &str) -> String {
    name.to_string()
}

// ponytail: find the native lib dir from any of our own loaded .so mappings in
// /proc/self/maps — one file read, no jni dependency. Swap to a JNI
// nativeLibraryDir lookup if a device ever breaks the assumption.
#[cfg(target_os = "android")]
fn tool_bin(name: &str) -> String {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return name.to_string();
    };
    for line in maps.lines() {
        let Some(i) = line.find('/') else { continue };
        let p = &line[i..];
        if p.ends_with(".so") && p.contains("/lib/") {
            if let Some(dir) = Path::new(p).parent() {
                let cand = dir.join(format!("lib{name}.so"));
                if cand.exists() {
                    return cand.display().to_string();
                }
            }
        }
    }
    name.to_string()
}

async fn capture(bin: &str, args: &[&str]) -> Result<String, String> {
    let bin = tool_bin(bin);
    let out = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run {bin}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // Surfaces in logcat on Android (RustStdoutStderr) where the status
        // bar truncates; harmless noise on a desktop terminal.
        eprintln!("capture {bin} failed ({}): {err}", out.status);
        return Err(err);
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
    /// Extra picture layers stacked after the base (and transition blend),
    /// bottom → top — V2, V3, … cutaways so multi-track preview matches export.
    /// Each is (path, source time, framing, effect/look chain).
    pub layers: Vec<(String, f64, String, String)>,
    /// A rendered title card and its opacity at this instant.
    /// Prefer [`Self::titles`] when more than one card is active.
    pub title: Option<(String, f64)>,
    /// Multiple title cards stacked bottom → top (T1 under T2 under …).
    /// When non-empty, takes precedence over [`Self::title`].
    pub titles: Vec<(String, f64)>,
    /// A full-frame adjustment-layer look active at this instant (grade + effect
    /// chain), applied to the composited picture below any title. Empty/None
    /// when no adjustment covers the playhead — same look the export bakes in.
    pub adjust: Option<String>,
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
    frame_data_uri_fill(path, t, w, h, framing, effect, over, "black").await
}

/// [`frame_data_uri`] with an explicit letterbox `fill` for "Fit" framing, so a
/// scrub of the composed reel shows the chosen Background behind a landscape
/// clip — matching what the export bakes in (preview == export).
#[allow(clippy::too_many_arguments)]
pub async fn frame_data_uri_fill(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    framing: &str,
    effect: &str,
    over: Over,
    fill: &str,
) -> Result<String, String> {
    let bytes = render_frame_bytes(path, t, w, h, framing, effect, over, fill, "mjpeg").await?;
    Ok(format!("data:image/jpeg;base64,{}", b64(&bytes)))
}

/// Write one composed timeline frame (same stack as the monitor) to `out` as PNG.
/// Size is the export canvas (`w`×`h`); overlays, titles, FX and transitions are
/// baked in, so what you scrub is what the file holds.
#[allow(clippy::too_many_arguments)]
pub async fn export_frame_png(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    framing: &str,
    effect: &str,
    over: Over,
    fill: &str,
    out: &std::path::Path,
) -> Result<(), String> {
    // Atomic write: a killed extract must not leave a half PNG at the final path.
    let tmp = out.with_extension("part.png");
    let bytes = render_frame_bytes(path, t, w, h, framing, effect, over, fill, "png").await?;
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })?;
    Ok(())
}

/// One composed frame as raw image bytes. `codec` is the ffmpeg image codec
/// (`mjpeg` for the scrub data-URI, `png` for Export frame).
#[allow(clippy::too_many_arguments)]
async fn render_frame_bytes(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    framing: &str,
    effect: &str,
    over: Over,
    fill: &str,
    codec: &str,
) -> Result<Vec<u8>, String> {
    let mut chain = frame_chain_fill(framing, w, h, "m", fill);
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
    let mut cmd = Command::new(ffmpeg_bin());
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
        let mut bchain = frame_chain_fill(bframing, w, h, "b", fill);
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
    // Multi-track cutaways (V2, V3, …) over the base — same stack as export.
    for (li, (lpath, lt, lframing, leffect)) in over.layers.iter().enumerate() {
        idx += 1;
        if is_still(lpath) {
            cmd.args(["-loop", "1"]);
        }
        cmd.args(["-ss", &format!("{lt:.3}"), "-i", lpath]);
        let mut lchain = frame_chain_fill(lframing, w, h, &format!("l{li}"), fill);
        if !leffect.is_empty() {
            lchain = format!("{lchain},setpts=PTS+{lt:.3}/TB,{leffect},setpts=PTS-{lt:.3}/TB");
        }
        graph += &format!(
            "[{idx}:v]{lchain},format=rgba[ly{idx}];\
             [{top}][ly{idx}]overlay[x{idx}];"
        );
        top = format!("x{idx}");
    }
    // Adjustment layer: a full-frame look over the composited picture, below any
    // title (captions stay clean, matching the export's placement). Shift the
    // clock to the seek point so a time-based look poses at the playhead.
    if let Some(look) = over.adjust.as_deref().filter(|s| !s.is_empty()) {
        graph += &format!("[{top}]setpts=PTS+{t:.3}/TB,{look},setpts=PTS-{t:.3}/TB[adj];");
        top = "adj".to_string();
    }
    // Title stack: multi-track cards when present, else the single-title field.
    let title_stack: Vec<(String, f64)> = if !over.titles.is_empty() {
        over.titles.clone()
    } else if let Some((png, alpha)) = &over.title {
        vec![(png.clone(), *alpha)]
    } else {
        Vec::new()
    };
    for (png, alpha) in &title_stack {
        idx += 1;
        cmd.args(["-i", png]);
        graph += &format!(
            "[{idx}:v]scale={w}:{h},format=rgba,colorchannelmixer=aa={:.3}[ttl{idx}];\
             [{top}][ttl{idx}]overlay[x{idx}];",
            alpha.clamp(0.0, 1.0)
        );
        top = format!("x{idx}");
    }
    if top == "base" {
        cmd.args(["-vf", &chain]);
    } else {
        graph += &format!("[{top}]null[out]");
        cmd.args(["-filter_complex", &graph, "-map", "[out]"]);
    }
    let out = cmd
        .args(["-frames:v", "1", "-f", "image2pipe", "-c:v", codec, "-"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
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

/// Grab one source frame at `t` seconds and write it to the freeze-frame cache
/// as a PNG. Content-addressed by path + time so repeating the same freeze is
/// free. Stills just return their own path — there is nothing to grab.
pub async fn extract_still(path: &str, t: f64) -> Result<String, String> {
    if is_still(path) {
        return Ok(path.to_string());
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    // Quantise to the millisecond so near-identical seeks share a file.
    ((t * 1000.0).round() as i64).hash(&mut h);
    if let Ok(m) = std::fs::metadata(path) {
        m.len().hash(&mut h);
        if let Ok(mt) = m.modified() {
            mt.hash(&mut h);
        }
    }
    let dir = cache_dir("freezes");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dst = dir.join(format!("{:016x}.png", h.finish()));
    if dst.exists() {
        return Ok(dst.display().to_string());
    }
    // Write to a temp name then rename so a killed extract never leaves a
    // half-written PNG that would poison the next cache hit.
    let tmp = dir.join(format!("{:016x}.part.png", h.finish()));
    let out = Command::new(ffmpeg_bin())
        .args(["-y", "-v", "error"])
        .args(["-ss", &format!("{t:.3}"), "-i", path])
        .args(["-frames:v", "1", "-update", "1"])
        .arg(tmp.as_os_str())
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if !out.status.success() || !tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(dst.display().to_string())
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
    let out = Command::new(ffmpeg_bin())
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
    frame_chain_fill(framing, w, h, tag, "black")
}

/// Like [`frame_chain`] but the "Fit" letterbox takes `fill` instead of black,
/// so a chosen Background shows behind a letterboxed landscape clip — the same
/// colour `transform_chain` pads a *banded* clip with. Every other framing
/// covers the frame, so `fill` is a no-op there.
fn frame_chain_fill(framing: &str, w: u32, h: u32, tag: &str, fill: &str) -> String {
    match framing {
        "Fit" => format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color={fill}"
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
    // Disabled or muted clips become silence of the right length rather than a
    // volume=0 stream, so a bad source can't stall the mix.
    if c.enabled && c.has_audio && c.volume > 0.0 {
        let tempo = atempo_chain(c.speed);
        let tempo = if tempo.is_empty() { String::new() } else { format!(",{tempo}") };
        let rev = if c.reverse { "areverse," } else { "" };
        // Reduce background noise + EQ preset, same building blocks as the A1 lane.
        let mut proc: Vec<String> = Vec::new();
        let dn = c.denoise.clamp(0.0, 1.0);
        if dn > 0.001 {
            proc.push(format!("afftdn=nr={:.1}:nf=-25", 4.0 + 20.0 * dn));
        }
        let treat = audio_treat_chain(&c.treat);
        if !treat.is_empty() {
            proc.push(treat.to_string());
        }
        let proc = if proc.is_empty() { String::new() } else { format!(",{}", proc.join(",")) };
        format!(
            "[{i}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,{rev}\
             aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo{proc}{tempo},\
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

/// A full-frame adjustment layer: a grade/effect `look` applied to everything
/// composited beneath it, only inside `[at, at+dur]`. Carries no media, so it
/// adds no ffmpeg input — it's a pure filter on the running composite.
#[derive(Clone, Default)]
pub struct AdjustSpec {
    pub at: f64,
    pub dur: f64,
    pub look: String,
    pub enabled: bool,
}

/// The whole edit as one filter graph: V1 clips trim + portrait crop + effect,
/// concat; V2 overlays composited on top; FX adjustment looks over that; T
/// titles above those; A1 audio mixed under. Ends [vout][aout]. Input order:
/// clips, overlays, titles, audio (adjustments add no inputs).
pub fn build_filter(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    adjustments: &[AdjustSpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    opts: ExportOpts,
) -> String {
    let mut f = String::new();
    for (i, c) in clips.iter().enumerate() {
        // Disabled: black of the right length, silence — still owns its extent
        // so the magnetic timeline does not collapse under neighbours.
        if !c.enabled {
            f += &format!(
                "color=c=black:s={W}x{H}:d={:.3}:r={FPS},format=yuv420p,setsar=1[v{i}];",
                c.trimmed()
            );
            f += &clip_audio(i, c);
            continue;
        }
        // Dividing PTS by the speed is what retimes the video; fps= after it
        // resamples so slow motion gets duplicated frames instead of a stutter.
        f += &format!(
            "[{i}:v]trim=start={:.3}:end={:.3},{}setpts=(PTS-STARTPTS)/{:.4},fps={FPS},\
             {},setsar=1{}[v{i}];",
            c.in_s,
            c.out_s,
            if c.reverse { "reverse," } else { "" },
            c.speed.max(0.01),
            frame_chain_fill(&c.framing, W, H, &format!("c{i}"), c.bg.color()),
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
        // Compose the layer's own look on a clip-local clock, THEN shift it into
        // its timeline slot. Order matters: a time-based filter inside look() (a
        // keyframed opacity's alpha curve, a motion preset) must see 0-based clip
        // time — the preview composes the look clip-locally too, so anything else
        // would animate at a different moment there than in the export. The final
        // PTS is identical to the old combined shift, so a static overlay (whose
        // look holds no clock) exports byte-for-byte as before.
        f += &format!(
            "[{idx}:v]trim=start={:.3}:end={:.3},{}setpts=(PTS-STARTPTS)/{:.4},fps={FPS},\
             {},setsar=1{},setpts=PTS+{:.3}/TB[ov{j}];",
            o.in_s,
            o.out_s,
            if o.reverse { "reverse," } else { "" },
            o.speed.max(0.01),
            frame_chain(&o.framing, W, H, &format!("o{j}")),
            eff(&o.effect),
            o.at,
        );
        let span = (o.at, o.at + o.trimmed());
        if o.blend.is_empty() {
            f += &format!(
                "[{vl}][ov{j}]overlay=eof_action=pass:enable='between(t,{:.3},{:.3})'[vx{j}];",
                span.0, span.1
            );
        } else {
            // Blend modes (screen/add/…) aren't `overlay` options, so screen the
            // layer over a copy of the base for the whole timeline, then let the
            // outer `overlay` show that blended copy only inside the layer's window
            // and the clean base outside it — reusing the same enable-gate as above.
            f += &format!(
                "[{vl}]split[bl{j}a][bl{j}b];\
                 [bl{j}b][ov{j}]blend=all_mode={}:shortest=0[bl{j}c];\
                 [bl{j}a][bl{j}c]overlay=enable='between(t,{:.3},{:.3})'[vx{j}];",
                o.blend, span.0, span.1
            );
        }
        vl = format!("vx{j}");
    }

    // Adjustment layers grade/stylise the composited picture over a window,
    // above V1+V2 but below the titles. Split the running composite, run the
    // look on one copy, then let the outer overlay show that processed copy only
    // inside the window and the clean base outside it — the same enable-gate the
    // blend path uses. A disabled or empty adjustment is a no-op.
    for (j, a) in adjustments.iter().enumerate() {
        if !a.enabled || a.look.is_empty() {
            continue;
        }
        f += &format!(
            "[{vl}]split[aj{j}a][aj{j}b];\
             [aj{j}b]{}[aj{j}c];\
             [aj{j}a][aj{j}c]overlay=enable='between(t,{:.3},{:.3})'[ax{j}];",
            a.look,
            a.at,
            a.at + a.dur,
        );
        vl = format!("ax{j}");
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
    let mut cmd = Command::new(ffmpeg_bin());
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

/// Integrated loudness (LUFS) of `path`'s span `[in_s, out_s]`, measured with
/// ffmpeg's `loudnorm` first pass. This is what "Normalize" keys off: measure a
/// clip, then set its gain so it lands on a target level (the reference level
/// for the platform, e.g. −14 LUFS). No encode — a null muxer, so it's quick.
pub async fn measure_loudness(path: &str, in_s: f64, out_s: f64) -> Result<f64, String> {
    let span = (out_s - in_s).max(0.05);
    let ss = format!("{:.3}", in_s.max(0.0));
    let t = format!("{span:.3}");
    let out = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-nostats", "-ss", &ss, "-i", path, "-t", &t])
        .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    // loudnorm prints its JSON block on stderr, after the log. Grab the value on
    // the "input_i" line rather than parsing the whole object.
    let log = String::from_utf8_lossy(&out.stderr);
    parse_loudnorm_i(&log).ok_or_else(|| "could not measure loudness".to_string())
}

/// Pull `input_i` (integrated LUFS) out of a loudnorm JSON dump on stderr.
pub fn parse_loudnorm_i(log: &str) -> Option<f64> {
    let line = log.lines().find(|l| l.contains("\"input_i\""))?;
    let val = line.split(':').nth(1)?.trim().trim_matches(|c| c == '"' || c == ',');
    val.parse::<f64>().ok()
}

/// Linear gain that moves a clip measured at `measured_lufs` onto `target_lufs`.
/// Clamped so near-silent material (very low LUFS) can't be boosted into a wall
/// of noise, and so an already-hot clip is pulled down rather than clipped.
pub fn normalize_gain(measured_lufs: f64, target_lufs: f64) -> f64 {
    let db = target_lufs - measured_lufs;
    10f64.powf(db / 20.0).clamp(0.05, 4.0)
}

/// Path for a new voiceover take under the cache dir (unique per call).
pub fn voiceover_out_path() -> std::path::PathBuf {
    let dir = cache_dir("voiceovers");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    dir.join(format!("vo-{stamp}.wav"))
}

/// Speak `text` into a wav under the cache dir (TikTok-style text-to-speech on
/// a text card). Shells to `espeak-ng` — same shell-over-engine stance as
/// ffmpeg/curl. Returns the wav path for the caller to probe and insert.
pub async fn tts(text: &str) -> Result<std::path::PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("This card has no text to read.".to_string());
    }
    let dir = cache_dir("tts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let out = dir.join(format!("tts-{stamp}.wav"));
    // ponytail: one fixed voice; add a voice picker if anyone asks.
    capture("espeak-ng", &["-v", "en+f3", "-s", "165", "-w", &out.display().to_string(), text])
        .await
        .map_err(|e| format!("Text-to-speech needs espeak-ng on PATH ({e})"))?;
    Ok(out)
}

/// Start capturing the default microphone into `path` (WAV, mono 48 kHz).
/// Tries PulseAudio first, then ALSA. The child stays running until
/// [`stop_mic_record`] writes `q` to its stdin (graceful so the WAV header is
/// finalized). Returns the child so the UI task can hold it.
pub async fn start_mic_record(path: &Path) -> Result<tokio::process::Child, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // (ffmpeg demuxer, device name) — default source on each backend.
    let backends: [(&str, &str); 2] = [("pulse", "default"), ("alsa", "default")];
    let mut last_err = String::from("no microphone backend responded");
    for (fmt, dev) in backends {
        let mut child = match Command::new(ffmpeg_bin())
            .args(["-y", "-hide_banner", "-nostats", "-loglevel", "error"])
            .args(["-f", fmt, "-i", dev])
            // Mono is enough for VO and halves file size; 48 kHz matches the mix.
            .args(["-ac", "1", "-ar", "48000", "-c:a", "pcm_s16le"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("ffmpeg ({fmt}): {e}");
                continue;
            }
        };
        // If the device is missing, ffmpeg exits almost immediately.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        match child.try_wait() {
            Ok(None) => return Ok(child), // still capturing
            Ok(Some(status)) => {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let mut buf = Vec::new();
                    let _ = s.read_to_end(&mut buf).await;
                    err = String::from_utf8_lossy(&buf).into_owned();
                }
                let _ = std::fs::remove_file(path);
                last_err = format!(
                    "{fmt}/{dev} exited ({status}): {}",
                    err.trim().lines().last().unwrap_or("no device")
                );
            }
            Err(e) => {
                last_err = format!("ffmpeg ({fmt}): {e}");
                let _ = child.start_kill();
            }
        }
    }
    Err(format!(
        "Could not open a microphone ({last_err}). Check that a mic is connected and not exclusively used by another app."
    ))
}

/// Stop a capture started by [`start_mic_record`]. Prefers a graceful `q` so
/// the WAV container is closed cleanly; falls back to kill after a short wait.
pub async fn stop_mic_record(mut child: tokio::process::Child) -> Result<(), String> {
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(b"q\n").await;
        drop(stdin);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("mic capture wait failed: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Ok(())
        }
    }
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
    let out = Command::new(ffmpeg_bin())
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

/// Pull a BPM out of a filename like `house_128bpm_master.wav` or `track 90 BPM.mp3`
/// — the digits immediately before "bpm". Reel music libraries name files this way,
/// so it's a free, exact tempo when present.
fn bpm_from_filename(name: &str) -> Option<f64> {
    let low = name.to_ascii_lowercase();
    let idx = low.find("bpm")?;
    let head = low[..idx].trim_end();
    let rev: String = head.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let n: f64 = rev.chars().rev().collect::<String>().parse().ok()?;
    (40.0..=300.0).contains(&n).then_some(n)
}

/// Fold a tempo into the [70, 160) BPM octave — kills the half/double-time
/// ambiguity autocorrelation can land on.
fn fold_bpm(mut b: f64) -> f64 {
    while b < 70.0 {
        b *= 2.0;
    }
    while b >= 160.0 {
        b /= 2.0;
    }
    b
}

/// Tempo from the dominant lag in an onset envelope's autocorrelation, searched
/// over 70–160 BPM.
fn autocorr_bpm(env: &[f32], env_hz: f64) -> f64 {
    let lag_min = ((env_hz * 60.0 / 160.0).floor() as usize).max(1);
    let lag_max = ((env_hz * 60.0 / 70.0).ceil() as usize).min(env.len().saturating_sub(1));
    let mut best_lag = lag_min;
    let mut best = f64::MIN;
    for lag in lag_min..=lag_max {
        let mut sum = 0.0;
        for i in lag..env.len() {
            sum += env[i] as f64 * env[i - lag] as f64;
        }
        let score = sum / (env.len() - lag) as f64; // normalize so long lags aren't penalized
        if score > best {
            best = score;
            best_lag = lag;
        }
    }
    env_hz * 60.0 / best_lag as f64
}

/// Estimate tempo (BPM) and beat positions from an onset-strength envelope
/// sampled at `env_hz`. `hint_bpm` (e.g. from the filename) pins the tempo when
/// present; otherwise it's found by autocorrelation. Returns (bpm, beat times in
/// seconds from the envelope's start).
// ponytail: naive single-tempo tracker — one global BPM, autocorr peak, best
// constant phase. Enough to seed markers you then nudge; upgrade to a
// dynamic-programming beat tracker only if drift or octave errors actually bite.
pub fn beats_from_envelope(env: &[f32], env_hz: f64, hint_bpm: Option<f64>) -> (f64, Vec<f64>) {
    if env.len() < 8 || env_hz <= 0.0 {
        return (0.0, Vec::new());
    }
    let bpm = hint_bpm
        .filter(|b| (40.0..=300.0).contains(b))
        .map(fold_bpm)
        .unwrap_or_else(|| autocorr_bpm(env, env_hz));
    if bpm <= 0.0 {
        return (0.0, Vec::new());
    }
    let period = env_hz * 60.0 / bpm; // frames per beat
    // Best constant phase: the pulse-train offset that collects the most onset.
    let steps = (env.len() as f64 / period).floor().max(1.0) as usize;
    let mut best_off = 0usize;
    let mut best_score = f64::MIN;
    for off in 0..period.ceil() as usize {
        let mut score = 0.0;
        for k in 0..=steps {
            let idx = (off as f64 + k as f64 * period).round() as usize;
            if idx < env.len() {
                score += env[idx] as f64;
            }
        }
        if score > best_score {
            best_score = score;
            best_off = off;
        }
    }
    let mut beats = Vec::new();
    let mut t = best_off as f64;
    while (t as usize) < env.len() {
        beats.push(t / env_hz);
        t += period;
    }
    (bpm, beats)
}

/// Detect the beat grid of a music bed. Decodes the [`in_s`, `out_s`] span to a
/// mono onset envelope and returns (bpm, beat times in *source* seconds). A
/// "<n>bpm" in the filename pins the tempo; the phase is always found from the
/// signal so beat 1 lands on a real onset.
pub async fn detect_beats(path: &str, in_s: f64, out_s: f64) -> Result<(f64, Vec<f64>), String> {
    const SR: u32 = 11025;
    const HOP: usize = 110; // ~100 Hz onset envelope
    let span = (out_s - in_s).max(0.5);
    let ss = format!("{:.3}", in_s.max(0.0));
    let t = format!("{span:.3}");
    let out = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-nostats", "-ss", &ss, "-i", path, "-t", &t])
        .args(["-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if out.stdout.is_empty() {
        return Err("could not decode audio for beat detection".into());
    }
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    if samples.len() < HOP * 8 {
        return Err("clip too short to detect beats".into());
    }
    // Per-hop rectified-energy onset envelope: a beat is a rise in energy.
    let env_hz = SR as f64 / HOP as f64;
    let mut env = Vec::with_capacity(samples.len() / HOP);
    let mut prev = 0.0f32;
    for frame in samples.chunks(HOP) {
        let e: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        env.push((e - prev).max(0.0));
        prev = e;
    }
    let hint = Path::new(path).file_name().and_then(|n| n.to_str()).and_then(bpm_from_filename);
    let (bpm, beats) = beats_from_envelope(&env, env_hz, hint);
    if beats.is_empty() {
        return Err("no beat detected".into());
    }
    Ok((bpm, beats.into_iter().map(|b| in_s + b).collect()))
}

/// Waveform strip of a file's audio (bright teal on transparent) as a data: URI.
/// Drawn once for the whole source — timeline items window into it with
/// background-size/position, so trims and splits need no re-render.
///
/// `scale=sqrt` + a mild pre-gain lifts quiet speech so the strip reads as a
/// real envelope rather than a thin mid-line; `draw=full` fills to the peaks.
pub async fn waveform_data_uri(path: &str) -> Result<String, String> {
    let out = Command::new(ffmpeg_bin())
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
fn parse_srt(srt: &str) -> Vec<(f64, f64, String)> {
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
    RENDER_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
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
            if render_cancelled() {
                let _ = child.kill().await;
                return Err("cancelled".into());
            }
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
/// Does the bundled ffmpeg carry Android's hardware H.264 encoder? True only
/// in an APK whose ffmpeg was built with --enable-mediacodec (see
/// packaging/android/bundle-ffmpeg.sh); desktop builds and the current
/// prebuilt probe false and change nothing.
fn has_hw_h264() -> bool {
    static HW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HW.get_or_init(|| {
        std::process::Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("h264_mediacodec"))
            .unwrap_or(false)
    })
}

pub async fn export(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    adjustments: &[AdjustSpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    out: &Path,
    opts: ExportOpts,
    mut on_progress: impl FnMut(f64),
) -> Result<(), String> {
    let hw = opts.format == Format::Mp4 && has_hw_h264();
    match export_pass(clips, overlays, adjustments, titles, audio, out, opts, hw, &mut on_progress).await {
        // LibreCuts-style fallback: mediacodec init fails per-device/per-size —
        // rerun the whole export in software rather than surface the error.
        Err(e) if hw && e != "cancelled" => {
            export_pass(clips, overlays, adjustments, titles, audio, out, opts, false, &mut on_progress).await
        }
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
async fn export_pass(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    adjustments: &[AdjustSpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    out: &Path,
    opts: ExportOpts,
    hw: bool,
    on_progress: &mut impl FnMut(f64),
) -> Result<(), String> {
    if clips.is_empty() {
        return Err("nothing to export".into());
    }
    RENDER_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    let total = timeline_len(clips);
    let mut cmd = Command::new(ffmpeg_bin());
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
    cmd.args(["-filter_complex", &build_filter(clips, overlays, adjustments, titles, audio, opts)]);
    // GIF's palette pass renames the video output; everything else maps [vout].
    cmd.args(["-map", if opts.format == Format::Gif { "[gout]" } else { "[vout]" }]);
    if opts.format.has_audio() {
        cmd.args(["-map", "[aout]"]);
    }
    match opts.format {
        Format::Mp4 => {
            if hw {
                // mediacodec has no CRF — bitrate by short edge, LibreCuts's
                // table; Quality still steers the software fallback.
                let br = if opts.width >= 1080 { "8M" } else if opts.width >= 720 { "5M" } else { "2M" };
                cmd.args(["-c:v", "h264_mediacodec", "-b:v", br, "-pix_fmt", "nv12"])
            } else {
                cmd.args(["-c:v", "libx264", "-preset", speed, "-crf", &crf, "-pix_fmt", "yuv420p"])
            }
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
        if render_cancelled() {
            let _ = child.kill().await;
            return Err("cancelled".into());
        }
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

    // finish_title (bevel path) must downscale the card to 1080×1920 and, when
    // not boxed, cast a soft shadow that grows the card's alpha. Differential:
    // same disc, boxed (shadow off) vs unboxed (shadow on) — no ffmpeg needed.
    #[test]
    fn bevel_card_downscales_and_casts_shadow() {
        let (cw, ch) = (400u32, 400u32);
        let (cx, cy, r) = (cw as f32 / 2.0, ch as f32 / 2.0, cw as f32 * 0.3);
        let mut img = image::RgbaImage::new(cw, ch);
        for y in 0..ch {
            for x in 0..cw {
                if ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() < r {
                    img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
                }
            }
        }
        let run = |boxed: bool, name: &str| -> ((u32, u32), u64) {
            let p = std::env::temp_dir().join(name);
            img.save(&p).unwrap();
            let st = TitleStyle { bevel: "Intaglio".into(), boxed, ..Default::default() };
            finish_title(&p, &st, 2).unwrap();
            let out = image::open(&p).unwrap().to_rgba8();
            let dims = out.dimensions();
            let asum: u64 = out.as_raw().chunks(4).map(|px| px[3] as u64).sum();
            let _ = std::fs::remove_file(&p);
            (dims, asum)
        };
        let (d_boxed, no_shadow) = run(true, "mr_bevel_boxed.png");
        let (d_unboxed, with_shadow) = run(false, "mr_bevel_shadow.png");
        assert_eq!(d_boxed, (W, H), "card must downscale to 1080x1920");
        assert_eq!(d_unboxed, (W, H));
        assert!(with_shadow > no_shadow, "cast shadow should add alpha ({with_shadow} vs {no_shadow})");
    }

    #[test]
    fn b64_matches_rfc4648() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"M"), "TQ==");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"Man"), "TWFu");
    }

    #[test]
    fn voiceover_out_path_is_under_cache_and_wav() {
        let a = voiceover_out_path();
        let b = voiceover_out_path();
        assert!(a.to_string_lossy().contains("voiceovers"));
        assert!(a.extension().and_then(|e| e.to_str()) == Some("wav"));
        // Same-millisecond calls can collide; distinct paths are preferred but
        // not required for correctness of a single take.
        let _ = b;
    }

    /// Smoke: TTS lands a real wav with audio in it. Skips when espeak-ng
    /// isn't installed (CI / headless).
    #[tokio::test]
    async fn tts_speaks_a_wav() {
        assert!(tts("   ").await.is_err(), "blank text must not shell out");
        let p = match tts("Hello from the timeline.").await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skip tts smoke (no espeak-ng): {e}");
                return;
            }
        };
        let (d, has_audio) = probe(&p.display().to_string()).await.unwrap();
        assert!(has_audio && d > 0.3, "spoken wav should have audible length, got {d}");
        let _ = std::fs::remove_file(&p);
    }

    /// Smoke: open the default mic briefly and land a valid WAV. Skips when
    /// no input device is available (CI / headless).
    #[tokio::test]
    async fn mic_record_round_trip_when_device_exists() {
        let path = voiceover_out_path();
        let child = match start_mic_record(&path).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skip mic smoke (no device): {e}");
                return;
            }
        };
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        stop_mic_record(child).await.expect("stop mic");
        let (dur, has_audio) = probe(path.to_str().unwrap()).await.expect("probe vo");
        assert!(has_audio, "voiceover wav should have audio");
        assert!(dur >= 0.15, "expected a short take, got {dur}");
        let _ = std::fs::remove_file(&path);
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
        let f = build_filter(&clips, &overlays, &[], &titles, &audio, ExportOpts::default());
        assert!(f.contains("[0:v]trim=start=0.500:end=2.000"));
        assert!(f.contains("setsar=1,hue=s=0[v0]"));
        assert!(f.contains("crop=1080:1920"));
        assert!(!f.contains("[1:a]") && f.contains("anullsrc"));
        assert!(f.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"));
        // input order: clips 0-1, overlay 2, title 3, audio 4
        // Look composed on a clip-local clock, then shifted to the timeline slot.
        assert!(f.contains("[2:v]trim=start=0.000:end=1.000,setpts=(PTS-STARTPTS)/1.0000,fps="));
        assert!(f.contains("setsar=1,setpts=PTS+0.500/TB[ov0]"));
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
        let f = build_filter(&clips, &[], &[], &[], &[], ExportOpts::default());
        assert!(f.ends_with("[vc]null[vout];[ac]anull[aout]"));
    }

    #[test]
    fn blend_mode_overlay_screens_over_the_base_only_in_its_window() {
        let clips = [ClipSpec { path: "a.mp4".into(), in_s: 0.0, out_s: 3.0, ..Default::default() }];
        let overlays =
            [OverlaySpec { path: "leak.mp4".into(), in_s: 0.0, out_s: 1.0, at: 0.5, blend: "screen".into(), ..Default::default() }];
        let f = build_filter(&clips, &overlays, &[], &[], &[], ExportOpts::default());
        // Base split, screen-blended full length, gated to the layer's span by the
        // outer overlay — not the plain alpha-over path.
        assert!(f.contains("[vc]split[bl0a][bl0b];"), "{f}");
        assert!(f.contains("[bl0b][ov0]blend=all_mode=screen:shortest=0[bl0c];"), "{f}");
        assert!(f.contains("[bl0a][bl0c]overlay=enable='between(t,0.500,1.500)'[vx0];"), "{f}");
        assert!(!f.contains("[ov0]overlay=eof_action=pass"), "blend path must skip alpha-over: {f}");
    }

    #[test]
    fn adjustment_layer_grades_the_composite_only_in_its_window() {
        let clips = [ClipSpec { path: "a.mp4".into(), in_s: 0.0, out_s: 3.0, ..Default::default() }];
        let adj = [AdjustSpec { at: 0.5, dur: 1.0, look: "hue=s=0".into(), enabled: true }];
        let f = build_filter(&clips, &[], &adj, &[], &[], ExportOpts::default());
        // Split the composite, run the look on one copy, show it only in-window.
        assert!(f.contains("[vc]split[aj0a][aj0b];"), "{f}");
        assert!(f.contains("[aj0b]hue=s=0[aj0c];"), "{f}");
        assert!(f.contains("[aj0a][aj0c]overlay=enable='between(t,0.500,1.500)'[ax0];"), "{f}");
        // Disabled or empty adjustments emit nothing.
        let off = [AdjustSpec { at: 0.5, dur: 1.0, look: "hue=s=0".into(), enabled: false }];
        assert!(!build_filter(&clips, &[], &off, &[], &[], ExportOpts::default()).contains("aj0"));
        let empty = [AdjustSpec { at: 0.5, dur: 1.0, look: String::new(), enabled: true }];
        assert!(!build_filter(&clips, &[], &empty, &[], &[], ExportOpts::default()).contains("aj0"));
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

    #[test]
    fn bpm_from_filename_reads_common_shapes() {
        assert_eq!(bpm_from_filename("house_128bpm_master.wav"), Some(128.0));
        assert_eq!(bpm_from_filename("track 90 BPM.mp3"), Some(90.0));
        assert_eq!(bpm_from_filename("no tempo here.wav"), None);
        assert_eq!(bpm_from_filename("bpm_only.wav"), None); // no digits before "bpm"
    }

    #[test]
    fn beats_from_envelope_recovers_a_known_tempo() {
        // Onset pulses every 50 frames at 100 Hz = 120 BPM, phase offset 7.
        let env_hz = 100.0;
        let period = env_hz * 60.0 / 120.0; // 50
        let mut env = vec![0.0f32; 2000];
        let mut i = 7.0;
        while (i as usize) < env.len() {
            env[i as usize] = 1.0;
            i += period;
        }
        let (bpm, beats) = beats_from_envelope(&env, env_hz, None);
        assert!((bpm - 120.0).abs() < 3.0, "bpm {bpm}");
        // Beat 1 lands on the planted phase, spacing matches the period.
        assert!((beats[0] * env_hz - 7.0).abs() <= 1.0, "phase {}", beats[0] * env_hz);
        assert!(((beats[1] - beats[0]) * env_hz - period).abs() < 2.0, "spacing off");
        // A filename hint pins tempo even against a noisier signal.
        let (bpm, _) = beats_from_envelope(&env, env_hz, Some(100.0));
        assert!((bpm - 100.0).abs() < 0.001, "hint ignored: {bpm}");
    }

    #[tokio::test]
    async fn detect_beats_finds_the_tempo_of_a_click_track() {
        let dir = std::env::temp_dir().join("morreel-beat-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("clicks.m4a").display().to_string();
        // A 1 kHz click every 0.5s for 8s = 120 BPM.
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi",
            "-i", "aevalsrc=exprs='sin(2*PI*1000*t)*lt(mod(t,0.5),0.02)':d=8:s=44100",
            "-c:a", "aac", &src,
        ])
        .await
        .unwrap();
        let (bpm, beats) = detect_beats(&src, 0.0, 8.0).await.unwrap();
        assert!((bpm - 120.0).abs() < 6.0, "expected ~120 BPM, got {bpm}");
        assert!(beats.len() >= 12, "expected a beat per click, got {}", beats.len());
        // Beats step by ~0.5s.
        let dt = beats[2] - beats[1];
        assert!((dt - 0.5).abs() < 0.08, "beat spacing {dt}");
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
        let f = build_filter(&[ClipSpec { speed: 2.0, ..base }], &[], &[], &[], &[], ExportOpts::default());
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
        export(&clips, &[], &[], &[], &[], &out, ExportOpts::preview(), |_| {}).await.unwrap();
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
            export(&clips, &[], &[], &[], &[], &out, opts, |_| {})
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
        let full = build_filter(&clips, &[], &[], &[], &[], ExportOpts::default());
        assert!(full.contains("[vc]null[vout]"), "full size should not rescale: {full}");
        assert!(full.ends_with("[ac]anull[aout]"));

        let small = build_filter(&clips, &[], &[], &[], &[], ExportOpts::default().with_size(720));
        assert!(small.contains("scale=720:1280"), "{small}");

        // GIF drains the mix into a sink and renames the video output, because
        // an unmapped [aout] is a hard filtergraph error.
        let gif = build_filter(
            &clips,
            &[],
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
    fn disabled_clip_exports_black_and_silent() {
        let clips = [ClipSpec {
            path: "a.mp4".into(),
            in_s: 0.0,
            out_s: 2.0,
            has_audio: true,
            volume: 1.0,
            enabled: false,
            ..Default::default()
        }];
        let f = build_filter(&clips, &[], &[], &[], &[], ExportOpts::default());
        assert!(
            f.contains("color=c=black") && f.contains("anullsrc"),
            "disabled clip should be black + silence: {f}"
        );
        assert!(!f.contains("[0:v]"), "disabled clip must not sample the source video");
    }

    #[test]
    fn extension_tables_are_disjoint_and_cover_the_obvious() {
        for e in ["mp4", "mov", "mkv", "webm", "avi", "gif"] {
            assert!(VIDEO_EXT.contains(&e), "{e} missing from the video list");
        }
        // Primary reel stills (JPEG/PNG/HEIF/TIFF/BMP + modern web stills).
        // GIF is video; PDF/PSD/RAW stay off these tables on purpose.
        for e in ["png", "jpg", "jpeg", "jfif", "heic", "heif", "tif", "tiff", "bmp", "webp", "avif"] {
            assert!(IMAGE_EXT.contains(&e), "{e} missing from the image list");
            assert!(is_still(&format!("x.{e}")), "{e} should take the still path");
        }
        for e in ["pdf", "psd", "cr2", "nef", "arw", "dng", "raw"] {
            assert!(!IMAGE_EXT.contains(&e), "{e} must stay out of stills (not a reel target)");
            assert!(!is_still(&format!("x.{e}")), "{e} must not take the still fast path");
        }
        assert!(!is_still("x.gif"), "gif is video, not a still");
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
            export(&clips, &[], &[], &[], &[], &out, opts, |_| {})
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
        export(&clips, &overlays, &[], &[], &[], &out, opts, |_| {}).await.unwrap();

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
            &[],
            ExportOpts::default(),
        );
        assert!(f.contains("setpts=(PTS-STARTPTS)/2.0000,"), "not retimed: {f}");
        assert!(f.contains("setpts=PTS+1.000/TB[ov0]"), "not shifted to its slot: {f}");
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
        let plain = build_filter(&[c(2.0), c(3.0)], &[], &[], &[], &[], ExportOpts::default());
        assert!(plain.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"), "{plain}");
        assert!(!plain.contains("xfade"));

        // With one: pairwise, xfade for video and acrossfade for audio, and the
        // offset is where the incoming clip starts on the finished timeline.
        let faded = build_filter(
            &[c(2.0), ClipSpec { transition: "Cross dissolve".into(), trans_dur: 0.5, ..c(3.0) }],
            &[],
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
        export(&clips, &[], &[], &[], &[], &out, opts, |_| {}).await.unwrap();

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
        let plain = build_filter(&clips, &[], &[], &[], &[bed(0.0)], ExportOpts::default());
        assert!(!plain.contains("sidechaincompress") && !plain.contains("asplit"), "{plain}");
        assert!(plain.contains("[ac][au0]amix=inputs=2"), "{plain}");

        // On: the main track is split so the compressor has something to key
        // from, and the mix takes the ducked copy rather than the raw bed.
        let ducked = build_filter(&clips, &[], &[], &[], &[bed(0.8)], ExportOpts::default());
        assert!(ducked.contains("[ac]asplit=2[amain][akey0]"), "{ducked}");
        assert!(ducked.contains("[au0][akey0]sidechaincompress="), "{ducked}");
        assert!(ducked.contains("[amain][au0d]amix=inputs=2"), "{ducked}");

        // Two beds, one ducked: the split only serves the one that asked.
        let mixed = build_filter(&clips, &[], &[], &[], &[bed(0.0), bed(0.5)], ExportOpts::default());
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
            noise_floor: -25.0,
            track_noise: false,
            compress: 0.4,
            gate: 0.0,
            declick: 0.0,
            treat: "Voice enhance".into(),
            duck: 0.0,
            lane: 1,
        };
        let f = build_filter(&clips, &[], &[], &[], &[bed], ExportOpts::default());
        assert!(f.contains("afftdn=nr="), "denoise missing: {f}");
        assert!(f.contains("nf=-25"), "noise floor missing: {f}");
        assert!(!f.contains(":tn=1"), "track_noise off should omit tn: {f}");
        // Floor + adaptive tracking flow through to the afftdn args.
        let bed2 = AudioSpec {
            denoise: 0.5,
            noise_floor: -55.0,
            track_noise: true,
            ..AudioSpec::default()
        };
        let f2 = build_filter(&[], &[], &[], &[], &[bed2], ExportOpts::default());
        assert!(f2.contains("nf=-55") && f2.contains(":tn=1"), "adaptive floor missing: {f2}");
        // Gate + de-click appear only when their knobs are up, and de-click sits
        // ahead of the denoiser so clicks are repaired before it smears them.
        assert!(!f.contains("agate=") && !f.contains("adeclick="), "fx present at zero: {f}");
        let bed3 = AudioSpec { gate: 0.5, declick: 1.0, denoise: 0.5, ..AudioSpec::default() };
        let f3 = build_filter(&[], &[], &[], &[], &[bed3], ExportOpts::default());
        assert!(f3.contains("agate=threshold="), "gate missing: {f3}");
        assert!(f3.contains("adeclick=threshold=1.50"), "declick missing/miscalibrated: {f3}");
        assert!(
            f3.find("adeclick=").unwrap() < f3.find("afftdn=").unwrap(),
            "de-click must precede denoise: {f3}"
        );
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
        export(&clips, &[], &[], &[], &audio, &out, opts, |_| {}).await.unwrap();

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
            export(&clips, &[], &[], &titles, &[], &out, opts, |_| {}).await.unwrap();
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

    #[test]
    fn keyframed_rotation_animates_in_both_branches() {
        let spin = Animated::curve(vec![
            Key { t: 0.0, v: 0.0, interp: Interp::Smooth },
            Key { t: 1.0, v: 90.0, interp: Interp::Smooth },
        ]);

        // Rotation-only: takes the static geometry branch, but the angle must be a
        // time expression (rotate=a='…'), not a baked number.
        let mut at = AnimatedTransform::default();
        at.rotation = spin.clone();
        let c = at.chain(W, H, false);
        assert!(c.contains("rotate=a='"), "static branch must emit a time-varying angle: {c}");
        assert!(c.contains("*PI/180"), "degrees curve must convert to radians: {c}");

        // Rotation + a keyframed zoom: takes the zoompan branch, which must animate
        // the spin too rather than freezing at the start pose.
        at.scale = Animated::curve(vec![
            Key { t: 0.0, v: 1.0, interp: Interp::Smooth },
            Key { t: 1.0, v: 1.5, interp: Interp::Smooth },
        ]);
        let c2 = at.chain(W, H, false);
        assert!(c2.contains("zoompan") && c2.contains("rotate=a='"), "zoompan branch must animate rotation: {c2}");

        // A constant rotation stays a plain number — no regression, no expression.
        let stat = AnimatedTransform::from(Transform { rotation: 45.0, ..Default::default() });
        let cs = stat.chain(W, H, false);
        assert!(cs.contains("rotate=") && !cs.contains("rotate=a='"), "constant rotation must stay static: {cs}");
    }

    #[test]
    fn anchor_relocates_the_pivot_without_touching_the_default() {
        // Anchor (0,0) is byte-identical to no anchor at all.
        let plain = Transform { rotation: 30.0, ..Default::default() };
        let anchored0 = Transform { rotation: 30.0, anchor_x: 0.0, anchor_y: 0.0, ..Default::default() };
        assert_eq!(transform_chain(&plain, W, H, false), transform_chain(&anchored0, W, H, false));

        // anchor_x 0.3 on a full-frame picture (sw = 1080) pads to a 1080+2*324 =
        // 1728-wide canvas before the rotate, so the pivot moves off-centre.
        let anchored = Transform { rotation: 30.0, anchor_x: 0.3, ..Default::default() };
        let ca = transform_chain(&anchored, W, H, false);
        assert_ne!(transform_chain(&plain, W, H, false), ca);
        assert!(ca.contains("pad=1728:"), "anchor must pad to relocate the pivot: {ca}");

        // Anchor alone (no rotation) still shifts the picture, so it isn't identity.
        let anchor_only = Transform { anchor_x: 0.25, ..Default::default() };
        assert!(!anchor_only.is_identity());
        assert!(!transform_chain(&anchor_only, W, H, false).is_empty());

        // A keyframed swing about an off-centre anchor: static branch, so it must
        // BOTH pad for the pivot AND animate the angle.
        let mut swing = AnimatedTransform::from(Transform { anchor_x: 0.3, ..Default::default() });
        swing.rotation = Animated::curve(vec![
            Key { t: 0.0, v: 0.0, interp: Interp::Smooth },
            Key { t: 1.0, v: 90.0, interp: Interp::Smooth },
        ]);
        let cw = swing.chain(W, H, false);
        assert!(cw.contains("pad=1728:") && cw.contains("rotate=a='"), "anchored swing must pad and animate: {cw}");
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
    fn a_transform_the_mcp_server_writes_loads_and_samples() {
        // Exactly the shape src/bin/mcp.rs emits: static fields as bare numbers,
        // a tracked axis as a key array. If the editor can't load this, the MCP
        // round-trip is broken — so pin it here against the real type.
        let json = serde_json::json!({
            "scale": 1.0, "scale_x": 0.5, "scale_y": 0.5, "cover": true, "y": -0.15,
            "x": [
                {"t": 0.0, "v": 0.3, "interp": "Smooth"},
                {"t": 2.0, "v": -0.3, "interp": "Smooth"}
            ]
        });
        let xf: AnimatedTransform = serde_json::from_value(json).unwrap();
        assert!(xf.x.is_animated());
        assert!((xf.x.sample(0.0) - 0.3).abs() < 1e-9);
        assert!((xf.pose().scale_x - 0.5).abs() < 1e-9);
        assert!(xf.cover);
        // And it compiles to a panning zoompan (scale const>? no — scale is const
        // 1.0 here, so the static branch runs and honours x via the crop offset).
        assert!(!xf.chain(W, H, false).is_empty());
    }

    #[test]
    fn an_animated_pan_offsets_the_zoompan_crop() {
        // Zoom headroom (engages the animated branch) plus an x pan curve.
        let mut xf = AnimatedTransform::default();
        xf.scale = Animated::curve(vec![
            Key { t: 0.0, v: 1.5, interp: Interp::Smooth },
            Key { t: 2.0, v: 1.5, interp: Interp::Smooth },
        ]);
        xf.x = Animated::curve(vec![
            Key { t: 0.0, v: 0.3, interp: Interp::Smooth },
            Key { t: 2.0, v: -0.3, interp: Interp::Smooth },
        ]);
        let chain = xf.chain(W, H, false);
        assert!(chain.contains("x='iw/2-(iw/zoom/2)-("), "pan term not applied: {chain}");
        assert!(chain.contains("(iw/zoom)"), "pan scale factor missing: {chain}");

        // A pure centred zoom (no pan) keeps the bare centre expression, unchanged.
        let mut z = AnimatedTransform::default();
        z.scale = xf.scale.clone();
        assert!(z.chain(W, H, false).contains("x='iw/2-(iw/zoom/2)':"), "{}", z.chain(W, H, false));
    }

    #[test]
    fn keyframed_opacity_animates_the_alpha_plane_only_on_a_composited_layer() {
        let mut xf = AnimatedTransform::default(); // geometry is the identity
        xf.opacity = Animated::curve(vec![
            Key { t: 0.0, v: 0.0, interp: Interp::Linear },
            Key { t: 1.0, v: 1.0, interp: Interp::Linear },
        ]);
        // On a composited layer the curve drives the alpha plane at clip-local
        // frame time T, never a constant colorchannelmixer.
        let over = xf.chain(W, H, true);
        assert!(over.contains("geq=") && over.contains("a='alpha(X,Y)*clip("), "no animated alpha: {over}");
        assert!(!over.contains("colorchannelmixer"), "opacity must not bake a constant: {over}");
        // V1 (no alpha) ignores opacity entirely — it fills black, so no alpha
        // filter is ever emitted there, animated or not.
        let v1 = xf.chain(W, H, false);
        assert!(!v1.contains("geq=") && !v1.contains("colorchannelmixer"), "no alpha work on V1: {v1}");
    }

    #[test]
    fn shape_colours_come_out_as_numbers_geq_understands() {
        assert_eq!(hex_rgb("black"), (0, 0, 0));
        assert_eq!(hex_rgb("white"), (255, 255, 255));
        assert_eq!(hex_rgb("#E8C060"), (232, 192, 96));
        assert_eq!(hex_rgb("#3DD6D0"), (61, 214, 208));
        assert_eq!(hex_rgb("nonsense"), (255, 255, 255), "fall back rather than break the render");
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
        export(&clips, &[], &[], &[], &[], &out, opts, |p| last = p).await.unwrap();
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

    // Export-frame must land a real PNG of the composed canvas (titles stacked),
    // not a zero-byte file or a JPEG with a .png name.
    #[tokio::test]
    async fn export_frame_png_writes_a_portrait_png() {
        let dir = std::env::temp_dir().join("morreel-frame-png-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4").display().to_string();
        capture("ffmpeg", &[
            "-y", "-v", "error",
            "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=30",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", &src,
        ]).await.unwrap();
        let title = render_title(&TitleStyle {
            text: "Frame".into(),
            font_size: 72,
            ..Default::default()
        })
        .await
        .unwrap();
        let out = dir.join("frame.png");
        export_frame_png(
            &src,
            0.25,
            540,
            960,
            "Crop",
            "",
            Over { titles: vec![(title, 1.0)], ..Default::default() },
            "black",
            &out,
        )
        .await
        .unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 100, "empty PNG");
        // PNG magic number — proves we didn't write MJPEG under a .png name.
        let magic = std::fs::read(&out).unwrap();
        assert_eq!(&magic[..8], b"\x89PNG\r\n\x1a\n");
        let dims = capture("ffprobe", &[
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0",
            &out.display().to_string(),
        ])
        .await
        .unwrap();
        assert_eq!(dims.trim(), "540,960");
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
    fn grade_is_identity_by_default_and_loads_old_projects() {
        // The whole back-compat contract: an element saved before grades existed
        // has no `grade`, so it deserialises to the default, which emits nothing.
        let g: Grade = serde_json::from_str("{}").unwrap();
        assert_eq!(g, Grade::default());
        assert!(g.is_identity());
        assert_eq!(g.chain(), "");

        // A neutral grade set field-by-field still emits nothing.
        assert!(Grade { exposure: 0.0, contrast: 1.0, saturation: 1.0, warmth: 6500.0 }
            .chain()
            .is_empty());
    }

    #[test]
    fn grade_emits_only_the_knobs_that_moved() {
        // Warmth alone is one filter, not an eq with default args riding along.
        let warm = Grade { warmth: 4500.0, ..Default::default() };
        assert_eq!(warm.chain(), "colortemperature=4500");

        // eq carries the three tone knobs in one pass; warmth stays out of it.
        let tone = Grade { exposure: 0.1, contrast: 1.2, saturation: 1.3, ..Default::default() };
        assert_eq!(tone.chain(), "eq=brightness=0.100:contrast=1.200:saturation=1.300");

        // Both halves present join with a comma, tone before temperature.
        let both = Grade { saturation: 0.5, warmth: 8000.0, ..Default::default() };
        assert_eq!(both.chain(), "eq=brightness=0.000:contrast=1.000:saturation=0.500,colortemperature=8000");
    }

    #[test]
    fn framing_chains() {
        assert!(frame_chain("Fit", 1080, 1920, "c0").contains("pad=1080:1920"));
        // A "Fit" letterbox is black by default but takes the chosen Background,
        // so a landscape clip's bands honour it the same way a banded clip's do.
        assert!(frame_chain("Fit", 1080, 1920, "c0").contains(":color=black"));
        assert!(frame_chain_fill("Fit", 1080, 1920, "c0", "white").contains(":color=white"));
        // Covering framings ignore the fill — there's nothing to letterbox.
        assert_eq!(
            frame_chain_fill("Crop", 1080, 1920, "c0", "white"),
            frame_chain("Crop", 1080, 1920, "c0")
        );
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

    #[test]
    fn loudness_measure_and_gain() {
        // parse the value off loudnorm's JSON dump, ignoring the log above it
        let log = "  size=N/A time=00:00:01\n{\n\t\"input_i\" : \"-23.45\",\n\t\"input_tp\" : \"-3.0\"\n}";
        assert_eq!(parse_loudnorm_i(log), Some(-23.45));
        assert_eq!(parse_loudnorm_i("nothing here"), None);
        // quieter-than-target clip is boosted (>1x); already at target ≈ unity
        assert!(normalize_gain(-23.0, -14.0) > 1.0);
        assert!((normalize_gain(-14.0, -14.0) - 1.0).abs() < 1e-9);
        // a near-silent clip can't be boosted past the ceiling
        assert_eq!(normalize_gain(-90.0, -14.0), 4.0);
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
        export(&clips, &overlays, &[], &titles, &audio, &out, ExportOpts::default(), |p| last = p).await.unwrap();
        assert_eq!(last, 1.0);

        // fast preview render (playback path) produces a playable file too
        let fast_out = dir.join("preview.mp4");
        export(&clips, &overlays, &[], &titles, &audio, &fast_out, ExportOpts::preview(), |_| {}).await.unwrap();
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




