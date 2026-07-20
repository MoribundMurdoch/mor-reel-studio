// SPDX-License-Identifier: GPL-3.0-or-later
// engine.rs — MorReel's media engine: the ffmpeg/ffprobe CLIs.
// Same split kdenlive (MLT) and openshot (libopenshot) use, minus the C++ library.

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
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Transform {
    /// 1.0 fills the frame, 0.5 is half size.
    #[serde(default = "unit")]
    pub scale: f64,
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
    /// V2 cutaway over V1. On V1 there is nothing underneath but black.
    #[serde(default = "unit")]
    pub opacity: f64,
}

fn unit() -> f64 {
    1.0
}

impl Default for Transform {
    fn default() -> Self {
        Self { scale: 1.0, x: 0.0, y: 0.0, rotation: 0.0, opacity: 1.0 }
    }
}

impl Transform {
    /// Untouched: emit no filter at all rather than a chain that scales by 1.
    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-6
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
    let sw = even(w as f64 * t.scale);
    let sh = even(h as f64 * t.scale);
    let dx = (t.x * w as f64).round() as i64;
    let dy = (t.y * h as f64).round() as i64;
    // Pad out to whatever the offset crop needs, so the picture can be moved
    // clean off the edge of the frame instead of jamming against it.
    let pw = even((sw as f64).max(w as f64 + 2.0 * dx.abs() as f64)).max(sw).max(w);
    let ph = even((sh as f64).max(h as f64 + 2.0 * dy.abs() as f64)).max(sh).max(h);
    let fill = if alpha { "black@0" } else { "black" };

    let mut c = String::new();
    if alpha {
        c += "format=rgba,";
    }
    c += &format!("scale={sw}:{sh}");
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
        }
    }
}

impl ClipSpec {
    /// Seconds this clip occupies on the timeline — source span divided by the
    /// speed, so a 4 s clip at 2× fills 2 s.
    pub fn trimmed(&self) -> f64 {
        (self.out_s - self.in_s) / self.speed.max(0.01)
    }
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
#[derive(Clone, PartialEq, Debug, Default)]
pub struct OverlaySpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    pub effect: String,
    pub framing: String,
}

/// A1: audio mixed under the main track starting at global time `at`.
#[derive(Clone, PartialEq, Debug)]
pub struct AudioSpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub at: f64,
    pub volume: f64,
}

/// T: a pre-rendered 1080×1920 transparent PNG shown from `at` for `dur`.
#[derive(Clone, PartialEq, Debug)]
pub struct TitleSpec {
    pub png: String,
    pub at: f64,
    pub dur: f64,
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
    /// fontconfig family ("Sans", "Serif", "Mono"); "" = drawtext default.
    pub font: String,
    /// Outline width in px, 0 = none. Carries legibility over busy video
    /// without the opaque plate a backdrop box needs.
    pub outline: f64,
    pub outline_color: String,
    /// Semi-opaque backdrop box behind the text.
    pub boxed: bool,
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
            outline: 0.0,
            outline_color: "black".into(),
            boxed: false,
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
        self.outline_color.hash(h);
        self.boxed.hash(h);
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
    // re-rendered instead of served stale. v3: transparent canvas, outline,
    // full bevel parameter set, designer composite order.
    const CACHE_VER: u32 = 3;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    CACHE_VER.hash(&mut h);
    s.hash(&mut h);
    let dir = cache_dir("titles");
    let png = dir.join(format!("{:016x}.png", h.finish()));
    if png.exists() {
        return Ok(png.display().to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // textfile= sidesteps drawtext's escaping rules entirely.
    let txt = png.with_extension("txt");
    std::fs::write(&txt, &s.text).map_err(|e| e.to_string())?;
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
        "drawtext=textfile={}{fontp}:fontsize={}:fontcolor={}\
         :x=(w-text_w)/2:y=(h-text_h)*{:.3}{shadow}{boxp}{border}",
        txt.display(),
        s.font_size,
        s.color,
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

    if s.bevel != "Off" {
        let img = image::open(&png).map_err(|e| e.to_string())?;
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
        rgba.save(&png).map_err(|e| e.to_string())?;
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
pub async fn frame_data_uri(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    framing: &str,
    effect: &str,
    title: Option<(&str, f64)>,
) -> Result<String, String> {
    let mut chain = frame_chain(framing, w, h);
    if !effect.is_empty() {
        // Seeking restarts the filter clock at 0, which would freeze every
        // time-based effect at its t=0 pose — most visible on a still, where
        // the frame itself never changes either. Run the effect on a clock
        // shifted to the seek point, then shift back so the title overlay
        // downstream still lines up on PTS 0.
        // ponytail: zoompan looks (Slow zoom, Pulse zoom) key on output frame
        // index, not PTS, so they still preview as their opening frame.
        chain = format!("{chain},setpts=PTS+{t:.3}/TB,{effect},setpts=PTS-{t:.3}/TB");
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if is_still(path) {
        cmd.args(["-loop", "1"]); // a lone frame has nothing to seek to
    }
    cmd.args(["-ss", &format!("{t:.3}"), "-i", path]);
    if let Some((png, alpha)) = title {
        cmd.args(["-i", png]);
        cmd.args([
            "-filter_complex",
            &format!(
                "[0:v]{chain}[b];[1:v]scale={w}:{h},format=rgba,\
                 colorchannelmixer=aa={:.3}[t];[b][t]overlay",
                alpha.clamp(0.0, 1.0)
            ),
        ]);
    } else {
        cmd.args(["-vf", &chain]);
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
/// "Crop" (default) covers and center-crops; "Fit" letterboxes on black;
/// "Zoom" punches in 1.5× then crops.
// ponytail: fixed 1.5× zoom — a per-clip zoom slider when someone asks.
fn frame_chain(framing: &str, w: u32, h: u32) -> String {
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

/// A1 chain for item `k` at input `idx`: trim, volume, delay to its timeline spot.
fn a1_audio(idx: usize, k: usize, a: &AudioSpec) -> String {
    format!(
        "[{idx}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,\
         aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,\
         volume={:.2},adelay={}:all=1[au{k}];",
        a.in_s,
        a.out_s,
        a.volume,
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
            frame_chain(&c.framing, W, H),
            eff(&c.effect)
        );
        f += &clip_audio(i, c);
    }
    for i in 0..clips.len() {
        f += &format!("[v{i}][a{i}]");
    }
    f += &format!("concat=n={}:v=1:a=1[vc][ac];", clips.len());

    let mut vl = "vc".to_string();
    for (j, o) in overlays.iter().enumerate() {
        let idx = clips.len() + j;
        f += &format!(
            "[{idx}:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS+{:.3}/TB,fps={FPS},\
             {},setsar=1{}[ov{j}];",
            o.in_s,
            o.out_s,
            o.at,
            frame_chain(&o.framing, W, H),
            eff(&o.effect)
        );
        f += &format!(
            "[{vl}][ov{j}]overlay=eof_action=pass:enable='between(t,{:.3},{:.3})'[vx{j}];",
            o.at,
            o.at + (o.out_s - o.in_s)
        );
        vl = format!("vx{j}");
    }

    for (j, t) in titles.iter().enumerate() {
        let idx = clips.len() + overlays.len() + j;
        // Title PNGs are fed with -loop 1 (see export), so the stream has real
        // timestamps to fade against: alpha in/out over `fade`, then shifted to
        // its timeline spot. Short titles shrink the fade so they still read.
        let fade = title_fade(t.dur);
        f += &format!(
            "[{idx}:v]format=rgba,trim=duration={:.3},\
             fade=t=in:st=0:d={fade:.3}:alpha=1,fade=t=out:st={:.3}:d={fade:.3}:alpha=1,\
             setpts=PTS+{:.3}/TB[ti{j}];",
            t.dur,
            t.dur - fade,
            t.at
        );
        f += &format!(
            "[{vl}][ti{j}]overlay=enable='between(t,{:.3},{:.3})'[vt{j}];",
            t.at,
            t.at + t.dur
        );
        vl = format!("vt{j}");
    }

    let mut al = "ac".to_string();
    if !audio.is_empty() {
        for (k, a) in audio.iter().enumerate() {
            f += &a1_audio(clips.len() + overlays.len() + titles.len() + k, k, a);
        }
        f += "[ac]";
        for k in 0..audio.len() {
            f += &format!("[au{k}]");
        }
        f += &format!("amix=inputs={}:duration=first:normalize=0[am];", audio.len() + 1);
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
    if audio.is_empty() {
        f += "[ac]anull[aout]";
    } else {
        for (k, a) in audio.iter().enumerate() {
            f += &a1_audio(clips.len() + k, k, a);
        }
        f += "[ac]";
        for k in 0..audio.len() {
            f += &format!("[au{k}]");
        }
        f += &format!("amix=inputs={}:duration=first:normalize=0[aout]", audio.len() + 1);
    }
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

/// Waveform strip of a file's audio (teal on transparent) as a data: URI.
/// Drawn once for the whole source — timeline items window into it with
/// background-size/position, so trims and splits need no re-render.
pub async fn waveform_data_uri(path: &str) -> Result<String, String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", path])
        .args(["-filter_complex", "aformat=channel_layouts=mono,showwavespic=s=1200x56:colors=0x3dd6d0"])
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
    let total: f64 = clips.iter().map(ClipSpec::trimmed).sum();
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
        let titles = [TitleSpec { png: "t.png".into(), at: 0.2, dur: 2.0 }];
        let audio = [AudioSpec { path: "m.mp3".into(), in_s: 0.0, out_s: 2.0, at: 1.0, volume: 0.5 }];
        let f = build_filter(&clips, &overlays, &titles, &audio, ExportOpts::default());
        assert!(f.contains("[0:v]trim=start=0.500:end=2.000"));
        assert!(f.contains("setsar=1,hue=s=0[v0]"));
        assert!(f.contains("crop=1080:1920"));
        assert!(!f.contains("[1:a]") && f.contains("anullsrc"));
        assert!(f.contains("[v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"));
        // input order: clips 0-1, overlay 2, title 3, audio 4
        assert!(f.contains("[2:v]trim=start=0.000:end=1.000,setpts=PTS-STARTPTS+0.500/TB"));
        assert!(f.contains("[vc][ov0]overlay=eof_action=pass:enable='between(t,0.500,1.500)'[vx0]"));
        // title: looped still trimmed to its duration, alpha-faded both ends,
        // shifted to its timeline spot
        assert!(f.contains(
            "[3:v]format=rgba,trim=duration=2.000,\
             fade=t=in:st=0:d=0.300:alpha=1,fade=t=out:st=1.700:d=0.300:alpha=1,\
             setpts=PTS+0.200/TB[ti0]"
        ));
        assert!(f.contains("[vx0][ti0]overlay=enable='between(t,0.200,2.200)'[vt0]"));
        assert!(f.contains("[4:a]") && f.contains("volume=0.50,adelay=1000:all=1[au0]"));
        assert!(f.contains("[ac][au0]amix=inputs=2:duration=first:normalize=0[am]"));
        assert!(f.ends_with("[vt0]null[vout];[am]anull[aout]"));

        // no overlays / titles / audio degenerates to plain concat
        let f = build_filter(&clips, &[], &[], &[], ExportOpts::default());
        assert!(f.ends_with("[vc]null[vout];[ac]anull[aout]"));
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
        let uri = frame_data_uri(&png, 2.5, 108, 192, "Crop", "", None).await.unwrap();
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
        let early = frame_data_uri(&png, 0.0, 108, 192, "Crop", sway, None).await.unwrap();
        let later = frame_data_uri(&png, 2.5, 108, 192, "Crop", sway, None).await.unwrap();
        assert_ne!(early, later, "Sway froze at its opening pose");

        // With no effect the still is genuinely the same frame at any time.
        let plain_a = frame_data_uri(&png, 0.0, 108, 192, "Crop", "", None).await.unwrap();
        let plain_b = frame_data_uri(&png, 2.5, 108, 192, "Crop", "", None).await.unwrap();
        assert_eq!(plain_a, plain_b);

        // Effect + title together: the clock shift must not desync the overlay,
        // so the composite has to differ from the same frame without a title.
        let title = render_title(&TitleStyle { text: "Hi".into(), font_size: 90, ..Default::default() }).await.unwrap();
        let composed = frame_data_uri(&png, 2.5, 108, 192, "Crop", sway, Some((&title, 1.0)))
            .await
            .unwrap();
        assert!(composed.starts_with("data:image/jpeg;base64,"));
        assert_ne!(composed, later, "title never composited onto the effect frame");
    }

    #[test]
    fn framing_chains() {
        assert!(frame_chain("Fit", 1080, 1920).contains("pad=1080:1920"));
        assert!(frame_chain("Zoom", 1080, 1920).starts_with("scale=1620:2880"));
        // default and unknown names center-crop
        assert_eq!(frame_chain("", 1080, 1920), frame_chain("Crop", 1080, 1920));
        assert!(frame_chain("Crop", 1080, 1920).ends_with("crop=1080:1920"));
    }

    #[test]
    fn audio_filter_shape() {
        let clips = [
            ClipSpec { path: "a.mp4".into(), in_s: 0.5, out_s: 2.0, has_audio: true, ..Default::default() },
            ClipSpec { path: "b.mp4".into(), in_s: 0.0, out_s: 1.0, has_audio: false, ..Default::default() },
        ];
        let audio = [AudioSpec { path: "m.mp3".into(), in_s: 0.0, out_s: 2.0, at: 1.0, volume: 0.5 }];
        let f = build_audio_filter(&clips, &audio);
        assert!(f.contains("[0:a]atrim=start=0.500:end=2.000"));
        assert!(f.contains("anullsrc"));
        assert!(f.contains("[a0][a1]concat=n=2:v=0:a=1[ac]"));
        // audio inputs follow the clips: index 2, not 4 as in the full export graph
        assert!(f.contains("[2:a]") && f.contains("volume=0.50,adelay=1000:all=1[au0]"));
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
        let audio = [AudioSpec { path: a, in_s: 0.0, out_s: 1.0, at: 0.5, volume: 0.6 }];
        // a beveled title card over the first second
        let png = render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, bevel: "Cameo".into(), ..Default::default() }).await.unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 0);
        // second call must be a cache hit
        assert_eq!(render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, bevel: "Cameo".into(), ..Default::default() }).await.unwrap(), png);
        // a boxed caption is a distinct render (backdrop baked in)
        let boxed = render_title(&TitleStyle { text: "MorReel".into(), font_size: 120, boxed: true, ..Default::default() }).await.unwrap();
        assert_ne!(boxed, png);
        assert!(std::fs::metadata(&boxed).unwrap().len() > 0);
        let titles = [TitleSpec { png, at: 0.0, dur: 1.0 }];
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


