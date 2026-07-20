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

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ClipSpec {
    pub path: String,
    pub in_s: f64,
    pub out_s: f64,
    pub has_audio: bool,
    /// ffmpeg filter snippet appended to the video chain; empty = no effect.
    pub effect: String,
}

impl ClipSpec {
    pub fn trimmed(&self) -> f64 {
        self.out_s - self.in_s
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
) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (text, font_size, color, (y_frac * 1000.0) as u64, bevel, bevel_size).hash(&mut h);
    let dir = cache_dir("titles");
    let png = dir.join(format!("{:016x}.png", h.finish()));
    if png.exists() {
        return Ok(png.display().to_string());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // textfile= sidesteps drawtext's escaping rules entirely.
    let txt = png.with_extension("txt");
    std::fs::write(&txt, text).map_err(|e| e.to_string())?;
    // Plain titles get a drop shadow for legibility; beveled ones carry their own relief.
    let shadow = if bevel == "Off" { ":shadowcolor=black@0.5:shadowx=3:shadowy=3" } else { "" };
    let vf = format!(
        "format=rgba,drawtext=textfile={}:fontsize={font_size}:fontcolor={color}\
         :x=(w-text_w)/2:y=(h-text_h)*{y_frac:.3}{shadow}",
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
/// title PNG) is composited on top, so preview = export.
pub async fn frame_data_uri(
    path: &str,
    t: f64,
    w: u32,
    h: u32,
    effect: &str,
    title: Option<&str>,
) -> Result<String, String> {
    let mut chain = format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}");
    if !effect.is_empty() {
        chain = format!("{chain},{effect}");
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-ss", &format!("{t:.3}"), "-i", path]);
    if let Some(png) = title {
        cmd.args(["-i", png]);
        cmd.args([
            "-filter_complex",
            &format!("[0:v]{chain}[b];[1:v]scale={w}:{h}[t];[b][t]overlay"),
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
        f += &format!(
            "[{i}:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS,fps={FPS},\
             scale={W}:{H}:force_original_aspect_ratio=increase,crop={W}:{H},setsar=1{}[v{i}];",
            c.in_s, c.out_s, eff(&c.effect)
        );
        if c.has_audio {
            f += &format!(
                "[{i}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,\
                 aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[a{i}];",
                c.in_s, c.out_s
            );
        } else {
            f += &format!("anullsrc=r=48000:cl=stereo,atrim=0:{:.3}[a{i}];", c.trimmed());
        }
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
             scale={W}:{H}:force_original_aspect_ratio=increase,crop={W}:{H},setsar=1{}[ov{j}];",
            o.in_s, o.out_s, o.at, eff(&o.effect)
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
        // Single PNG frame: overlay's default eof_action=repeat holds it,
        // enable= gates when it is visible.
        f += &format!("[{idx}:v]format=rgba[ti{j}];");
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
            let idx = clips.len() + overlays.len() + titles.len() + k;
            f += &format!(
                "[{idx}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,\
                 aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,\
                 volume={:.2},adelay={}:all=1[au{k}];",
                a.in_s,
                a.out_s,
                a.volume,
                (a.at * 1000.0).round() as u64
            );
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
    for path in clips
        .iter()
        .map(|c| &c.path)
        .chain(overlays.iter().map(|o| &o.path))
        .chain(titles.iter().map(|t| &t.png))
        .chain(audio.iter().map(|a| &a.path))
    {
        cmd.args(["-i", path]);
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
            ClipSpec { path: "a.mp4".into(), in_s: 0.5, out_s: 2.0, has_audio: true, effect: "hue=s=0".into() },
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
        assert!(f.contains("[3:v]format=rgba[ti0]"));
        assert!(f.contains("[vx0][ti0]overlay=enable='between(t,0.200,2.200)'[vt0]"));
        assert!(f.contains("[4:a]") && f.contains("volume=0.50,adelay=1000:all=1[au0]"));
        assert!(f.contains("[ac][au0]amix=inputs=2:duration=first:normalize=0[am]"));
        assert!(f.ends_with("[vt0]null[vout];[am]anull[aout]"));

        // no overlays / titles / audio degenerates to plain concat
        let f = build_filter(&clips, &[], &[], &[]);
        assert!(f.ends_with("[vc]null[vout];[ac]anull[aout]"));
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
            ClipSpec { path: a.clone(), in_s: 0.5, out_s: 1.5, has_audio: true, effect: "hue=s=0".into() },
            ClipSpec { path: b.clone(), in_s: 0.0, out_s: 1.0, has_audio: false, ..Default::default() },
        ];
        let overlays = [OverlaySpec { path: b, in_s: 0.0, out_s: 0.5, at: 0.2, effect: "vignette".into() }];
        let audio = [AudioSpec { path: a, in_s: 0.0, out_s: 1.0, at: 0.5, volume: 0.6 }];
        // a beveled title card over the first second
        let png = render_title("MorReel", 120, "white", 0.45, "Cameo", 8).await.unwrap();
        assert!(std::fs::metadata(&png).unwrap().len() > 0);
        // second call must be a cache hit
        assert_eq!(render_title("MorReel", 120, "white", 0.45, "Cameo", 8).await.unwrap(), png);
        let titles = [TitleSpec { png, at: 0.0, dur: 1.0 }];
        let mut last = 0.0;
        export(&clips, &overlays, &titles, &audio, &out, false, |p| last = p).await.unwrap();
        assert_eq!(last, 1.0);

        // fast preview render (playback path) produces a playable file too
        let fast_out = dir.join("preview.mp4");
        export(&clips, &overlays, &titles, &audio, &fast_out, true, |_| {}).await.unwrap();
        assert!(std::fs::metadata(&fast_out).unwrap().len() > 0);

        let dims = capture("ffprobe", &[
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0",
            &out.display().to_string(),
        ]).await.unwrap();
        assert_eq!(dims.trim(), "1080,1920");
    }
}
