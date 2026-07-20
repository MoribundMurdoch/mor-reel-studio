// SPDX-License-Identifier: GPL-3.0-or-later
// engine.rs — MorReel's media engine: the ffmpeg/ffprobe CLIs.
// Same split kdenlive (MLT) and openshot (libopenshot) use, minus the C++ library.

use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Portrait-only output. Every clip is center-cropped to fill this frame.
const W: u32 = 1080;
const H: u32 = 1920;
const FPS: u32 = 30;

/// A still photo on the timeline. ffmpeg gets `-loop 1` for these, so the one
/// frame becomes a stream the trim can bound and the Motion effects (moranima's
/// camera moves) have real timestamps to animate against — a photo with Drift
/// or Pulse zoom is the whole point of putting one on a reel.
pub fn is_still(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff")
    )
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

/// Rasterize a title card: ffmpeg drawtext onto a transparent canvas, then an
/// optional cameo/intaglio bevel (the mor_cameo_emboss algorithm) baked in.
/// Content-addressed in the cache, so identical params never re-render.
pub async fn render_title(
    text: &str,
    font_size: u32,
    color: &str,
    y_frac: f64,
    bevel: &str, // "Off" | "Cameo" | "Intaglio"
    bevel_size: u32,
    font: &str,  // fontconfig family ("Sans", "Serif", "Mono"); "" = drawtext default
    boxed: bool, // semi-opaque backdrop box (caption legibility over busy video)
) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (text, font_size, color, (y_frac * 1000.0) as u64, bevel, bevel_size, font, boxed).hash(&mut h);
    let dir = cache_dir("titles");
    let png = dir.join(format!("{:016x}.png", h.finish()));
    if png.exists() {
        return Ok(png.display().to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // textfile= sidesteps drawtext's escaping rules entirely.
    let txt = png.with_extension("txt");
    std::fs::write(&txt, text).map_err(|e| e.to_string())?;
    // A backdrop box carries legibility on its own; otherwise plain titles get
    // a drop shadow and beveled ones carry their own relief.
    let shadow = if bevel == "Off" && !boxed { ":shadowcolor=black@0.5:shadowx=3:shadowy=3" } else { "" };
    let boxp = if boxed { ":box=1:boxcolor=black@0.45:boxborderw=18" } else { "" };
    let fontp = if font.is_empty() { String::new() } else { format!(":font='{font}'") };
    let vf = format!(
        "format=rgba,drawtext=textfile={}{fontp}:fontsize={font_size}:fontcolor={color}\
         :x=(w-text_w)/2:y=(h-text_h)*{y_frac:.3}{shadow}{boxp}",
        txt.display()
    );
    let out = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("color=c=black@0.0:s={W}x{H}")])
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

    if bevel != "Off" {
        let img = image::open(&png).map_err(|e| e.to_string())?;
        let mut rgba = img.to_rgba8();
        let (w, h_px) = rgba.dimensions();
        let result = crate::bevel::compute_bevel(
            rgba.as_raw(),
            w,
            h_px,
            &crate::bevel::BevelParams {
                size: bevel_size.max(1),
                soften: 2,
                angle: 120.0,
                altitude: 30.0,
                depth: 60,
                hi_opacity: 0.8,
                sh_opacity: 0.7,
                cameo: bevel == "Cameo",
            },
        );
        // Screen the white highlight over the text, multiply the black shadow.
        let buf = rgba.as_mut();
        for i in 0..(w * h_px) as usize {
            let hi_a = result.hi_rgba[i * 4 + 3] as f32 / 255.0;
            let sh_a = result.sh_rgba[i * 4 + 3] as f32 / 255.0;
            for c in 0..3 {
                let v = buf[i * 4 + c] as f32;
                buf[i * 4 + c] = ((v + hi_a * (255.0 - v)) * (1.0 - sh_a)) as u8;
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
    let dur: f64 = dur
        .trim()
        .parse()
        .map_err(|_| format!("no duration found in {path}"))?;
    let audio = capture(
        "ffprobe",
        &["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", path],
    )
    .await?;
    Ok((dur, !audio.trim().is_empty()))
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

    f += &format!("[{vl}]null[vout];[{al}]anull[aout]");
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

/// Render to `out`. `on_progress` gets 0.0..=1.0 as ffmpeg reports out_time.
/// `fast` trades quality for render speed (ultrafast preview files for playback).
pub async fn export(
    clips: &[ClipSpec],
    overlays: &[OverlaySpec],
    titles: &[TitleSpec],
    audio: &[AudioSpec],
    out: &Path,
    fast: bool,
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
    let (preset, crf) = if fast { ("ultrafast", "32") } else { ("veryfast", "20") };
    cmd.args(["-filter_complex", &build_filter(clips, overlays, titles, audio)])
        .args(["-map", "[vout]", "-map", "[aout]"])
        .args(["-c:v", "libx264", "-preset", preset, "-crf", crf, "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-b:a", "192k"])
        .args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats"])
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
        let f = build_filter(&clips, &overlays, &titles, &audio);
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
        let f = build_filter(&clips, &[], &[], &[]);
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
        let f = build_filter(&[ClipSpec { speed: 2.0, ..base }], &[], &[], &[]);
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
        export(&clips, &[], &[], &[], &out, true, |_| {}).await.unwrap();
        let d = capture("ffprobe", &[
            "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        let secs: f64 = d.trim().parse().unwrap();
        assert!((secs - 2.0).abs() < 0.25, "4 s at 2x should be ~2 s, got {secs}");
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
        export(&clips, &[], &[], &[], &out, true, |p| last = p).await.unwrap();
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
        let title = render_title("Hi", 90, "white", 0.45, "Off", 8, "Sans", false).await.unwrap();
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
        let png = render_title("MorReel", 120, "white", 0.45, "Cameo", 8, "Sans", false).await.unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 0);
        // second call must be a cache hit
        assert_eq!(render_title("MorReel", 120, "white", 0.45, "Cameo", 8, "Sans", false).await.unwrap(), png);
        // a boxed caption is a distinct render (backdrop baked in)
        let boxed = render_title("MorReel", 120, "white", 0.45, "Off", 8, "Sans", true).await.unwrap();
        assert_ne!(boxed, png);
        assert!(std::fs::metadata(&boxed).unwrap().len() > 0);
        let titles = [TitleSpec { png, at: 0.0, dur: 1.0 }];
        let mut last = 0.0;
        export(&clips, &overlays, &titles, &audio, &out, false, |p| last = p).await.unwrap();
        assert_eq!(last, 1.0);

        // fast preview render (playback path) produces a playable file too
        let fast_out = dir.join("preview.mp4");
        export(&clips, &overlays, &titles, &audio, &fast_out, true, |_| {}).await.unwrap();
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
