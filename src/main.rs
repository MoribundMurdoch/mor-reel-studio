// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — portrait-only (9:16) video editor.
// V1: main clip track (trim/reorder/split, ripple by construction).
// V2: full-frame cutaway overlays. A1: audio mixed under. Effects per video item.

mod bevel;
mod engine;

use dioxus::prelude::*;
use engine::{AudioSpec, ClipSpec, OverlaySpec, TitleSpec};
use mor_rust_dioxus_ui_kit::{
    use_shortcut, MenuItem, MenuSeparator, Modal, MorAppFrame, MorMenuDropdown, MorSelect,
    MorShortcutRoot, MorStyleProvider, Slider, UiMode,
};

/// MorReel look: near-black neutral surround (color judgment happens against it),
/// with the UI palette derived from the app's own title colors — tungsten amber
/// for video, teal for audio, gold for titles, record-red for the playhead.
const MORREEL_TOML: &str = r##"
bg            = "#141417"
panel         = "#1b1b20"
header        = "#0f0f12"
text          = "#eae7e0"
text_muted    = "#8f8d97"
border        = "#26262d"
border_light  = "#37363f"
accent        = "#e6a23d"
accent_hover  = "#f2b755"
btn           = "#2a2a31"
btn_hover     = "#34343d"
font_family   = "Cantarell, 'Segoe UI', system-ui, sans-serif"
font_size_base= "13px"
font_size_h1  = "20px"
padding_base  = "8px"
border_radius = "6px"
destructive   = "#e5484d"
success       = "#3dd6d0"
warning       = "#e8c060"
"##;

fn main() {
    let cfg = UiMode::launch_config("MorReel Studio");
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

/// Named looks in the moranima spirit — each is one ffmpeg filter snippet,
/// applied identically to preview frames and export so preview = export.
const EFFECTS: &[(&str, &str)] = &[
    ("None", ""),
    ("B&W", "hue=s=0"),
    ("Sepia", "colorchannelmixer=.393:.769:.189:0:.349:.686:.168:0:.272:.534:.131"),
    ("Warm", "colortemperature=4500"),
    ("Cool", "colortemperature=8500"),
    ("Punch", "eq=contrast=1.18:saturation=1.45"),
    ("Dreamy", "gblur=sigma=2,eq=brightness=0.04:saturation=1.15"),
    ("Vignette", "vignette"),
    ("Slow zoom", "zoompan=z='min(zoom+0.0006,1.25)':d=1:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=1080x1920:fps=30,setsar=1"),
];

fn effect_filter(name: &str) -> &'static str {
    EFFECTS.iter().find(|(n, _)| *n == name).map_or("", |(_, f)| f)
}

const TITLE_COLORS: &[(&str, &str)] = &[
    ("White", "white"),
    ("Black", "black"),
    ("Gold", "#E8C060"),
    ("Red", "#E5484D"),
    ("Cyan", "#3DD6D0"),
];

const TITLE_POS: &[(&str, f64)] = &[("Top", 0.10), ("Middle", 0.45), ("Lower third", 0.72)];

/// Bevel styles from the mor_cameo_emboss plugin: cameo = raised, intaglio = sunken.
const BEVELS: &[&str] = &["Off", "Cameo", "Intaglio"];

fn title_color(name: &str) -> &'static str {
    TITLE_COLORS.iter().find(|(n, _)| *n == name).map_or("white", |(_, c)| c)
}

fn title_y(name: &str) -> f64 {
    TITLE_POS.iter().find(|(n, _)| *n == name).map_or(0.45, |(_, y)| *y)
}

/// Rasterize a title card from its item's params.
async fn render_one(t: &TitleItem) -> Result<String, String> {
    engine::render_title(
        &t.text,
        t.font_size as u32,
        title_color(&t.color),
        title_y(&t.pos),
        &t.bevel,
        t.bevel_size as u32,
    )
    .await
}

#[derive(Clone, PartialEq)]
struct TitleItem {
    text: String,
    at: f64,
    dur: f64,
    font_size: f64,
    color: String,
    pos: String,
    bevel: String,
    bevel_size: f64,
    /// Rendered PNG path; empty while a render is in flight.
    png: String,
}

#[derive(Clone, PartialEq)]
struct Clip {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    has_audio: bool,
    effect: String,
    thumb: String,
    /// 480p scrub proxy path; empty until the background build finishes.
    proxy: String,
}

impl Clip {
    fn spec(&self) -> ClipSpec {
        ClipSpec {
            path: self.path.clone(),
            in_s: self.in_s,
            out_s: self.out_s,
            has_audio: self.has_audio,
            effect: effect_filter(&self.effect).to_string(),
        }
    }

    fn trimmed(&self) -> f64 {
        self.out_s - self.in_s
    }

    /// What preview/scrub extraction should read: the proxy once built.
    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

#[derive(Clone, PartialEq)]
struct OverlayItem {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    at: f64,
    effect: String,
    proxy: String,
}

impl OverlayItem {
    fn scrub_path(&self) -> String {
        if self.proxy.is_empty() { self.path.clone() } else { self.proxy.clone() }
    }
}

#[derive(Clone, PartialEq)]
struct AudioItem {
    path: String,
    name: String,
    duration: f64,
    in_s: f64,
    out_s: f64,
    at: f64,
    volume: f64,
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

/// Map a global timeline position to (clip index, source-file time) on V1.
fn locate(clips: &[Clip], t: f64) -> Option<(usize, f64)> {
    let mut acc = 0.0;
    for (i, c) in clips.iter().enumerate() {
        let d = c.trimmed();
        if t < acc + d || i + 1 == clips.len() {
            return Some((i, c.in_s + (t - acc).clamp(0.0, d)));
        }
        acc += d;
    }
    None
}

fn fmt_t(s: f64) -> String {
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
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

#[component]
fn App() -> Element {
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        MorShortcutRoot { Editor {} }
    }
}

/// Program monitor window: just the phone, fed by the editor's preview signal.
/// Runs in its own VirtualDom, so it gets its own style provider.
#[component]
fn Monitor(preview: Signal<String>) -> Element {
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        div { class: "mr-monitor",
            // The webview's default menu (Reload/Inspect) is noise in a monitor.
            oncontextmenu: move |evt| evt.prevent_default(),
            div { class: "mr-phone",
                if preview().is_empty() {
                    span { "Portrait preview" }
                } else {
                    img { src: "{preview}" }
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

    let total_of = move || clips.read().iter().map(Clip::trimmed).sum::<f64>();

    // Right-click menu: (viewport x, y, what was clicked). One menu, many targets.
    let mut ctx_menu = use_signal(|| Option::<(f64, f64, Ctx)>::None);
    let mut open_ctx = move |evt: Event<MouseData>, target: Ctx| {
        evt.prevent_default(); // replaces the webview's Reload/Inspect menu
        evt.stop_propagation();
        let p = evt.client_coordinates();
        ctx_menu.set(Some((p.x, p.y, target)));
    };

    // Preview extraction: latest-wins queue so slider drags don't stack ffmpeg runs.
    let mut pending = use_signal(|| Option::<(String, f64, String, Option<String>)>::None);
    let mut preview_busy = use_signal(|| false);
    let mut request_preview = move |path: String, t: f64, effect: String, title: Option<String>| {
        pending.set(Some((path, t, effect, title)));
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
                let Some((p, t, e, ti)) = next else { break };
                if let Ok(uri) = engine::frame_data_uri(&p, t, 540, 960, &e, ti.as_deref()).await {
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
            .find(|ti| t >= ti.at && t < ti.at + ti.dur && !ti.png.is_empty())
            .map(|ti| ti.png.clone());
        let over = overlays
            .read()
            .iter()
            .find(|o| t >= o.at && t < o.at + (o.out_s - o.in_s))
            .map(|o| (o.scrub_path(), o.in_s + (t - o.at), effect_filter(&o.effect).to_string()));
        let loc = locate(&clips.read(), t);
        if let Some((i, _)) = loc {
            if selected() != Some(Sel::Main(i)) {
                selected.set(Some(Sel::Main(i)));
            }
        }
        if let Some((path, local, eff)) = over {
            request_preview(path, local, eff, title_png);
        } else if let Some((i, local)) = loc {
            let (path, eff) = {
                let cl = clips.read();
                (cl[i].scrub_path(), effect_filter(&cl[i].effect).to_string())
            };
            request_preview(path, local, eff, title_png);
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
                        item.png = png;
                    }
                    seek_to(playhead());
                }
                Err(e) => status.set(format!("Title render failed: {e}")),
            }
        });
    };

    let start_of = move |i: usize| -> f64 {
        clips.read().iter().take(i).map(Clip::trimmed).sum()
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
        clips.read().iter().map(|c| { let s = acc; acc += c.trimmed(); (s, acc) }).collect()
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

    // Split at playhead: a selected V2/A1 item splits if the playhead is inside
    // its span; otherwise the V1 clip under the playhead splits. Selection lands
    // on the right half — where the playhead is — matching seek behavior.
    let mut split_at_playhead = move |_: ()| {
        const MIN: f64 = 0.1;
        let t = playhead();
        let mut too_close = move || status.set("Playhead is too close to an edge to split.".to_string());

        if let Some(Sel::Over(j)) = selected() {
            let Some(o) = overlays.read().get(j).cloned() else { return };
            match cut_local(o.in_s, o.out_s, o.in_s + (t - o.at), MIN) {
                Some(local) => {
                    {
                        let mut ov = overlays.write();
                        let mut right = ov[j].clone();
                        ov[j].out_s = local;
                        right.in_s = local;
                        right.at = t;
                        ov.insert(j + 1, right);
                    }
                    selected.set(Some(Sel::Over(j + 1)));
                    status.set(format!("Split overlay {} at {}.", o.name, fmt_t(t)));
                }
                None => too_close(),
            }
            return;
        }
        if let Some(Sel::Aud(k)) = selected() {
            let Some(a) = audios.read().get(k).cloned() else { return };
            match cut_local(a.in_s, a.out_s, a.in_s + (t - a.at), MIN) {
                Some(local) => {
                    {
                        let mut au = audios.write();
                        let mut right = au[k].clone();
                        au[k].out_s = local;
                        right.in_s = local;
                        right.at = t;
                        au.insert(k + 1, right);
                    }
                    selected.set(Some(Sel::Aud(k + 1)));
                    status.set(format!("Split audio {} at {}.", a.name, fmt_t(t)));
                }
                None => too_close(),
            }
            return;
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
        let (scrub, path) = {
            let mut cl = clips.write();
            let mut right = cl[i].clone();
            cl[i].out_s = local;
            right.in_s = local;
            cl.insert(i + 1, right);
            (cl[i + 1].scrub_path(), cl[i + 1].path.clone())
        };
        selected.set(Some(Sel::Main(i + 1)));
        status.set(format!("Split {} at {}.", name, fmt_t(t)));
        // The right half's thumbnail still shows the left's frame — retake it
        // at the new in point so the two halves are tellable apart.
        spawn(async move {
            if let Ok(thumb) = engine::frame_data_uri(&scrub, local, 108, 192, "", None).await {
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

    let import_clips = move |_: ()| {
        if importing() {
            return;
        }
        spawn(async move {
            let Some(files) = rfd::AsyncFileDialog::new()
                .add_filter("Video", &["mp4", "mov", "mkv", "webm", "m4v", "avi"])
                .set_title("Add clips")
                .pick_files()
                .await
            else {
                return;
            };
            importing.set(true);
            for f in files {
                let path = f.path().display().to_string();
                status.set(format!("Importing {}…", f.file_name()));
                match engine::probe(&path).await {
                    Ok((duration, has_audio)) => {
                        let thumb =
                            engine::frame_data_uri(&path, (duration * 0.1).min(1.0), 108, 192, "", None)
                                .await
                                .unwrap_or_default();
                        clips.write().push(Clip {
                            path: path.clone(),
                            name: f.file_name(),
                            duration,
                            in_s: 0.0,
                            out_s: duration,
                            has_audio,
                            effect: "None".to_string(),
                            thumb,
                            proxy: String::new(),
                        });
                        queue_proxy(path);
                        if selected().is_none() {
                            select_clip(0);
                        }
                    }
                    Err(e) => status.set(format!("Could not import {}: {e}", f.file_name())),
                }
            }
            importing.set(false);
            status.set(format!("{} clip(s) on the timeline.", clips.read().len()));
        });
    };

    let add_overlay = move |_: ()| {
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Video", &["mp4", "mov", "mkv", "webm", "m4v", "avi"])
                .set_title("Add overlay (V2)")
                .pick_file()
                .await
            else {
                return;
            };
            let path = f.path().display().to_string();
            match engine::probe(&path).await {
                Ok((duration, _)) => {
                    overlays.write().push(OverlayItem {
                        path: path.clone(),
                        name: f.file_name(),
                        duration,
                        in_s: 0.0,
                        out_s: duration,
                        at: playhead(),
                        effect: "None".to_string(),
                        proxy: String::new(),
                    });
                    queue_proxy(path);
                    selected.set(Some(Sel::Over(overlays.read().len() - 1)));
                    status.set("Overlay added at the playhead (V2 covers V1 while it runs).".to_string());
                }
                Err(e) => status.set(format!("Could not add overlay: {e}")),
            }
        });
    };

    let add_audio = move |_: ()| {
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Audio", &["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "mp4"])
                .set_title("Add audio (A1)")
                .pick_file()
                .await
            else {
                return;
            };
            let path = f.path().display().to_string();
            match engine::probe(&path).await {
                Ok((duration, has_audio)) => {
                    if !has_audio {
                        status.set(format!("{} has no audio stream.", f.file_name()));
                        return;
                    }
                    audios.write().push(AudioItem {
                        path,
                        name: f.file_name(),
                        duration,
                        in_s: 0.0,
                        out_s: duration,
                        at: playhead(),
                        volume: 1.0,
                    });
                    selected.set(Some(Sel::Aud(audios.read().len() - 1)));
                    status.set("Audio added at the playhead — mixed under the main track.".to_string());
                }
                Err(e) => status.set(format!("Could not add audio: {e}")),
            }
        });
    };

    let mut add_title = move |_: ()| {
        if clips.read().is_empty() {
            return;
        }
        titles.write().push(TitleItem {
            text: "Title".to_string(),
            at: playhead(),
            dur: 3.0,
            font_size: 110.0,
            color: "White".to_string(),
            pos: "Middle".to_string(),
            bevel: "Cameo".to_string(),
            bevel_size: 10.0,
            png: String::new(),
        });
        let k = titles.read().len() - 1;
        selected.set(Some(Sel::Title(k)));
        rerender_title(k);
        status.set("Title added at the playhead — edit it in the inspector.".to_string());
    };

    // In-app playback: a timer walks the playhead in real time and reuses the
    // scrub pipeline (proxies + latest-wins queue), so frames that can't keep
    // up are dropped instead of queued. No audio — use Full preview for that.
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
        });
    };

    // Render any title still missing its PNG (e.g. render was interrupted).
    let ensure_titles = move || async move {
        let missing: Vec<(usize, TitleItem)> = titles
            .read()
            .iter()
            .enumerate()
            .filter(|(_, t)| t.png.is_empty())
            .map(|(k, t)| (k, t.clone()))
            .collect();
        for (k, t) in missing {
            if let Ok(png) = render_one(&t).await {
                if let Some(item) = titles.write().get_mut(k) {
                    item.png = png;
                }
            }
        }
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
                effect: effect_filter(&o.effect).to_string(),
            })
            .collect();
        let tspecs = titles
            .read()
            .iter()
            .filter(|t| !t.png.is_empty())
            .map(|t| TitleSpec { png: t.png.clone(), at: t.at, dur: t.dur })
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
            })
            .collect();
        (specs, ospecs, tspecs, aspecs)
    };

    let mut do_export = move |_: ()| {
        if clips.read().is_empty() || export_progress().is_some() {
            return;
        }
        playing.set(false);
        spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter("MP4", &["mp4"])
                .set_file_name("morreel.mp4")
                .set_title("Export portrait video")
                .save_file()
                .await
            else {
                return;
            };
            ensure_titles().await;
            let (specs, ospecs, tspecs, aspecs) = gather_specs();
            export_progress.set(Some(0.0));
            status.set("Exporting…".to_string());
            let res = engine::export(&specs, &ospecs, &tspecs, &aspecs, file.path(), false, |p| {
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
            let res = engine::export(&specs, &ospecs, &tspecs, &aspecs, &out, true, |p| {
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
                let old = spans();
                clips.write().swap(i, j);
                selected.set(Some(Sel::Main(j)));
                ride(old, &|k| Some(start_of(if k == i { j } else if k == j { i } else { k })));
            }
        }
    };

    let mut delete_sel = move |_: ()| {
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
    use_shortcut(Some("~".into()), Some(EventHandler::new(move |()| toggle_magnet(()))));

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

    // Pop-out program monitor: a second OS window sharing the same preview
    // signal — closing it is how you "dock" it back.
    let open_monitor = move || {
        use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
        let dom = VirtualDom::new_with_props(Monitor, MonitorProps { preview });
        let cfg = Config::new()
            .with_menu(None::<dioxus::desktop::muda::Menu>)
            .with_window(
                WindowBuilder::new()
                    .with_title("MorReel Monitor")
                    .with_inner_size(LogicalSize::new(414.0, 764.0)),
            );
        let _ = dioxus::desktop::window().new_window(dom, cfg);
    };

    let total = total_of();
    let exporting = export_progress().is_some();
    let no_clips = clips.read().is_empty();
    let effect_names: Vec<String> = EFFECTS.iter().map(|(n, _)| n.to_string()).collect();

    rsx! {
        MorAppFrame {
            title: "MorReel Studio".to_string(),
            subtitle: Some("portrait 9:16".to_string()),
            app_name: "MorReel Studio".to_string(),
            menu: Some(rsx! {
                MorMenuDropdown { label: "File".to_string(),
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
                        on_action: move |_| open_monitor(),
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
                if !magnet() {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "magnet off" }
                }
                if preferred_mode() != active_mode {
                    span { class: "mor-statusbar-chip mor-statusbar-warn", "window mode: restart to apply" }
                }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "{fmt_t(total)} total" }
                span { class: "mor-statusbar-chip mor-statusbar-muted", "1080×1920 • 30 fps" }
            },

            div { class: "mr-root",
                div { class: "mr-work",
                    div { class: "mr-preview-col",
                        div { class: "mr-phone",
                            oncontextmenu: move |evt| open_ctx(evt, Ctx::Monitor),
                            if preview().is_empty() {
                                span { "Portrait preview" }
                            } else {
                                img { src: "{preview}" }
                            }
                        }
                        if !clips.read().is_empty() {
                            div { class: "mr-scrub",
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
                                        title: "Open the monitor in its own window — edit on one screen, watch on another",
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
                                class: "mor-btn",
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

                        {match selected() {
                            Some(Sel::Main(i)) if i < clips.read().len() => {
                                let c = clips.read()[i].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 { "V1 · {c.name}" }
                                        p { class: "mor-statusbar-muted",
                                            "{fmt_t(c.duration)} source • keeping {fmt_t(c.trimmed())}"
                                            if !c.has_audio { " • no audio" }
                                            if c.proxy.is_empty() { " • building proxy…" } else { " • proxy" }
                                        }
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
                                            let eff = effect_filter(&c.effect).to_string();
                                            move |v: f64| {
                                                let old = spans();
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].in_s = v.min(cl[i].out_s - 0.1).max(0.0);
                                                    cl[i].in_s
                                                };
                                                ride(old, &|k| Some(start_of(k)));
                                                playhead.set(start_of(i));
                                                request_preview(path.clone(), t, eff.clone(), None);
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
                                            let eff = effect_filter(&c.effect).to_string();
                                            move |v: f64| {
                                                let old = spans();
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].out_s = v.max(cl[i].in_s + 0.1).min(cl[i].duration);
                                                    cl[i].out_s
                                                };
                                                ride(old, &|k| Some(start_of(k)));
                                                playhead.set(start_of(i + 1));
                                                request_preview(path.clone(), t, eff.clone(), None);
                                            }
                                        })),
                                    }
                                    MorSelect {
                                        label: "Effect".to_string(),
                                        value: c.effect.clone(),
                                        options: effect_names.clone(),
                                        onchange: {
                                            let path = c.scrub_path();
                                            move |name: String| {
                                                let t = {
                                                    let mut cl = clips.write();
                                                    cl[i].effect = name.clone();
                                                    cl[i].in_s
                                                };
                                                request_preview(path.clone(), t, effect_filter(&name).to_string(), None);
                                            }
                                        },
                                    }
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
                                        h3 { "V2 · {o.name}" }
                                        p { class: "mor-statusbar-muted",
                                            "Cutaway covers V1 from {fmt_t(o.at)} for {fmt_t(o.out_s - o.in_s)} — main audio keeps playing."
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
                                            let eff = effect_filter(&o.effect).to_string();
                                            move |v: f64| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].in_s = v.min(ov[j].out_s - 0.1).max(0.0);
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, eff.clone(), None);
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
                                            let eff = effect_filter(&o.effect).to_string();
                                            move |v: f64| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].out_s = v.max(ov[j].in_s + 0.1).min(ov[j].duration);
                                                    ov[j].out_s
                                                };
                                                request_preview(path.clone(), t, eff.clone(), None);
                                            }
                                        })),
                                    }
                                    MorSelect {
                                        label: "Effect".to_string(),
                                        value: o.effect.clone(),
                                        options: effect_names.clone(),
                                        onchange: {
                                            let path = o.scrub_path();
                                            move |name: String| {
                                                let t = {
                                                    let mut ov = overlays.write();
                                                    ov[j].effect = name.clone();
                                                    ov[j].in_s
                                                };
                                                request_preview(path.clone(), t, effect_filter(&name).to_string(), None);
                                            }
                                        },
                                    }
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove overlay" }
                                    }
                                }
                            }
                            Some(Sel::Title(k)) if k < titles.read().len() => {
                                let t = titles.read()[k].clone();
                                rsx! {
                                    div { class: "mr-clip-info",
                                        h3 { "T · Title" }
                                        p { class: "mor-statusbar-muted",
                                            "Shown from {fmt_t(t.at)} for {fmt_t(t.dur)}"
                                            if t.png.is_empty() { " • rendering…" }
                                        }
                                    }
                                    mor_rust_dioxus_ui_kit::MorTextInput {
                                        label: "Text".to_string(),
                                        value: t.text.clone(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.text = v;
                                                item.png.clear();
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
                                                item.png.clear();
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
                                                item.png.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Position".to_string(),
                                        value: t.pos.clone(),
                                        options: TITLE_POS.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.pos = v;
                                                item.png.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    MorSelect {
                                        label: "Bevel (cameo emboss)".to_string(),
                                        value: t.bevel.clone(),
                                        options: BEVELS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: move |v: String| {
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.bevel = v;
                                                item.png.clear();
                                            }
                                            rerender_title(k);
                                        },
                                    }
                                    if t.bevel != "Off" {
                                        Slider {
                                            label: Some("Bevel size"),
                                            min: 2.0,
                                            max: 30.0,
                                            step: 1.0,
                                            precision: 0,
                                            value: t.bevel_size,
                                            oninput: Some(EventHandler::new(move |v: f64| {
                                                if let Some(item) = titles.write().get_mut(k) {
                                                    item.bevel_size = v;
                                                    item.png.clear();
                                                }
                                                rerender_title(k);
                                            })),
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
                                        h3 { "A1 · {a.name}" }
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
                                            audios.write()[k].volume = v;
                                        })),
                                    }
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove audio" }
                                    }
                                }
                            }
                            _ => rsx! {
                                p { class: "mor-statusbar-muted",
                                    "Add portrait or landscape clips — everything is center-cropped to 9:16. Select an item on the timeline to edit it."
                                }
                            },
                        }}

                        p { class: "mor-statusbar-muted mr-keys",
                            "I/O trim · S split · Del ripple delete · ←/→ scrub (Shift = 1s) · [ ] select clip · ~ magnet · Ctrl+O add · Ctrl+E export"
                        }
                    }
                }

                div { class: "mr-timeline",
                    oncontextmenu: move |evt| open_ctx(evt, Ctx::Timeline),
                    if clips.read().is_empty() {
                        span { class: "mor-statusbar-muted mr-timeline-hint", "Timeline — clips play left to right" }
                    } else {
                        {
                            // ponytail: scale keyed to shortest clip (min 48px wide) — no
                            // per-clip min-width, so ruler/playhead geometry stays exact.
                            let min_dur = clips.read().iter().map(Clip::trimmed).fold(f64::MAX, f64::min);
                            let scale = (48.0 / min_dur).clamp(14.0, 240.0);
                            let track_end = total
                                .max(overlays.read().iter().map(|o| o.at + o.out_s - o.in_s).fold(0.0, f64::max))
                                .max(titles.read().iter().map(|t| t.at + t.dur).fold(0.0, f64::max))
                                .max(audios.read().iter().map(|a| a.at + a.out_s - a.in_s).fold(0.0, f64::max));
                            let tick_s = if track_end <= 30.0 { 5.0 } else if track_end <= 120.0 { 10.0 } else { 30.0 };
                            let ph = playhead().min(total);
                            rsx! {
                                div { class: "mr-track", style: "width: {track_end * scale}px",
                                    div {
                                        class: "mr-ruler",
                                        onclick: move |evt: Event<MouseData>| {
                                            seek_to((evt.element_coordinates().x / scale).clamp(0.0, total_of()));
                                        },
                                        for k in 0..=((track_end / tick_s) as usize) {
                                            span {
                                                class: "mr-tick",
                                                style: "left: {k as f64 * tick_s * scale}px",
                                                "{fmt_t(k as f64 * tick_s)}"
                                            }
                                        }
                                    }
                                    div { class: "mr-lane",
                                        span { class: "mr-lane-tag title", "T" }
                                        for (k, t) in titles().into_iter().enumerate() {
                                            div {
                                                key: "title-{k}",
                                                class: if selected() == Some(Sel::Title(k)) { "mr-lane-item title selected" } else { "mr-lane-item title" },
                                                style: "left: {t.at * scale}px; width: {t.dur * scale}px",
                                                onclick: move |_| {
                                                    let at = titles.read()[k].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Title(k)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Title(k)));
                                                    open_ctx(evt, Ctx::Title(k));
                                                },
                                                "𝐓 {t.text}"
                                            }
                                        }
                                    }
                                    div { class: "mr-lane",
                                        span { class: "mr-lane-tag", "V2" }
                                        for (j, o) in overlays().into_iter().enumerate() {
                                            div {
                                                key: "{j}-{o.path}",
                                                class: if selected() == Some(Sel::Over(j)) { "mr-lane-item selected" } else { "mr-lane-item" },
                                                style: "left: {o.at * scale}px; width: {(o.out_s - o.in_s) * scale}px",
                                                onclick: move |_| {
                                                    let at = overlays.read()[j].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Over(j)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Over(j)));
                                                    open_ctx(evt, Ctx::Over(j));
                                                },
                                                "{o.name}"
                                            }
                                        }
                                    }
                                    div { class: "mr-clips",
                                        span { class: "mr-lane-tag", "V1" }
                                        for (i, c) in clips().into_iter().enumerate() {
                                            div {
                                                key: "{i}-{c.path}",
                                                class: if selected() == Some(Sel::Main(i)) { "mr-clip selected" } else { "mr-clip" },
                                                style: "width: {c.trimmed() * scale}px",
                                                onclick: move |_| select_clip(i),
                                                // Right-click selects without moving the playhead.
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Main(i)));
                                                    open_ctx(evt, Ctx::Clip(i));
                                                },
                                                if c.thumb.is_empty() {
                                                    div { class: "mr-thumb-missing" }
                                                } else {
                                                    img { src: "{c.thumb}" }
                                                }
                                                span { class: "mr-clip-name",
                                                    if c.effect != "None" { "✨ " }
                                                    "{c.name}"
                                                }
                                                span { class: "mr-clip-dur", "{fmt_t(c.trimmed())}" }
                                            }
                                        }
                                    }
                                    div { class: "mr-lane mr-lane-a1",
                                        span { class: "mr-lane-tag", "A1" }
                                        for (k, a) in audios().into_iter().enumerate() {
                                            div {
                                                key: "{k}-{a.path}",
                                                class: if selected() == Some(Sel::Aud(k)) { "mr-lane-item audio selected" } else { "mr-lane-item audio" },
                                                style: "left: {a.at * scale}px; width: {(a.out_s - a.in_s) * scale}px",
                                                onclick: move |_| {
                                                    let at = audios.read()[k].at;
                                                    seek_to(at);
                                                    selected.set(Some(Sel::Aud(k)));
                                                },
                                                oncontextmenu: move |evt| {
                                                    selected.set(Some(Sel::Aud(k)));
                                                    open_ctx(evt, Ctx::Aud(k));
                                                },
                                                "♪ {a.name}"
                                            }
                                        }
                                    }
                                    div { class: "mr-playhead", style: "left: {ph * scale}px" }
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
                    ("Space", "Play / pause (silent proxy playback)"),
                    ("Ctrl+P", "Full preview with audio in mpv/ffplay"),
                    ("I / O", "Set in / out point at playhead"),
                    ("S", "Split at playhead"),
                    ("Delete / Backspace", "Ripple delete selection"),
                    ("← / →", "Nudge playhead 0.1s (Shift = 1s)"),
                    ("[ / ]", "Select previous / next clip"),
                    ("~", "Toggle magnetic timeline (V2/A1/T ride V1 edits)"),
                    ("Home / End", "Jump to start / end"),
                    ("Ctrl+O", "Add clips"),
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
.mr-root { display: flex; flex-direction: column; gap: 12px; height: 100%; min-height: 0; padding: 12px; box-sizing: border-box; }
.mr-work { display: flex; gap: 16px; flex: 1; min-height: 0; }
.mr-preview-col { display: flex; flex-direction: column; gap: 10px; align-items: center; min-height: 0; padding-top: 4px; }

/* Signature: the preview is a phone — bezel and faint tungsten glow. */
/* Width-driven: aspect-ratio height doesn't feed the flex column's intrinsic width
   in WebKit, so a flex-sized phone overflows the column. 400px ≈ vertical chrome. */
.mr-phone { position: relative; flex: none; width: calc((100vh - 400px) * 9 / 16); min-width: 140px; max-height: 100%; aspect-ratio: 9 / 16; background: #000; border: 5px solid #060608; box-shadow: 0 0 0 1px var(--mor-border-light), 0 14px 40px rgba(0, 0, 0, 0.55), 0 0 70px color-mix(in srgb, var(--mor-accent) 7%, transparent); border-radius: 24px; overflow: hidden; display: flex; align-items: center; justify-content: center; color: var(--mor-text-muted); font-size: 13px; }
.mr-phone img { width: 100%; height: 100%; object-fit: cover; display: block; }

/* Pop-out monitor window: the phone alone, sized to the window. */
.mr-monitor { height: 100vh; display: flex; align-items: center; justify-content: center; padding: 14px; box-sizing: border-box; background: var(--mor-bg); }
.mr-monitor .mr-phone { width: auto; height: 100%; max-width: 100%; min-width: 0; }

.mr-scrub { width: 100%; }
.mr-play-row { display: flex; gap: 8px; justify-content: center; margin-top: 8px; }
.mr-inspector { flex: 1; min-width: 280px; display: flex; flex-direction: column; gap: 12px; background: var(--mor-panel); border: 1px solid var(--mor-border); border-radius: var(--mor-radius); padding: 14px; overflow-y: auto; }
.mr-toolbar { display: flex; gap: 8px; flex-wrap: wrap; }
.mr-clip-info h3 { margin: 0 0 4px 0; font-size: 14px; overflow-wrap: anywhere; }
.mr-clip-info p { margin: 0; font-size: 12px; }
.mr-danger { color: var(--mor-destructive); }
.mr-keys { margin-top: auto; font-size: 11px; }
.mr-progress { height: 6px; background: var(--mor-border); border-radius: 3px; overflow: hidden; }
.mr-progress > div { height: 100%; background: var(--mor-accent); transition: width 0.3s; }

/* Timeline sits on the darkest surface — the bench under the work. */
.mr-timeline { display: flex; overflow-x: auto; padding: 12px 10px 8px; background: var(--mor-header); border: 1px solid var(--mor-border); border-radius: var(--mor-radius); min-height: 216px; align-items: flex-start; flex: none; }
.mr-timeline-hint { align-self: center; margin: auto; }
.mr-track { position: relative; flex: none; min-width: 100%; }

/* Timecodes are instrument readouts: monospace, tabular. */
.mr-tick, .mr-clip-dur, .mr-key { font-family: ui-monospace, 'Cascadia Mono', 'DejaVu Sans Mono', monospace; font-variant-numeric: tabular-nums; }

.mr-ruler { position: relative; height: 18px; margin-bottom: 6px; border-bottom: 1px solid var(--mor-border-light); cursor: pointer; }
.mr-tick { position: absolute; top: 0; height: 100%; border-left: 1px solid var(--mor-border-light); padding-left: 3px; font-size: 9px; color: var(--mor-text-muted); pointer-events: none; }

.mr-lane { position: relative; height: 30px; margin-bottom: 6px; background: rgba(127, 127, 127, 0.06); border-radius: 4px; }
.mr-lane-tag { position: absolute; top: 4px; left: 4px; z-index: 2; font-size: 9px; font-weight: 700; padding: 1px 5px; border-radius: 3px; background: var(--mor-accent); color: #141417; pointer-events: none; }
.mr-lane-tag.title { background: var(--mor-warning); }
.mr-lane-a1 .mr-lane-tag { background: var(--mor-success); }

.mr-lane-item { position: absolute; top: 2px; bottom: 2px; box-sizing: border-box; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 10px; line-height: 22px; padding: 0 6px 0 30px; border-radius: 4px; border: 2px solid color-mix(in srgb, var(--mor-accent) 40%, transparent); background: color-mix(in srgb, var(--mor-accent) 24%, transparent); cursor: pointer; }
.mr-lane-item.audio { border-color: color-mix(in srgb, var(--mor-success) 40%, transparent); background: color-mix(in srgb, var(--mor-success) 22%, transparent); }
.mr-lane-item.title { border-color: color-mix(in srgb, var(--mor-warning) 45%, transparent); background: color-mix(in srgb, var(--mor-warning) 26%, transparent); }
.mr-lane-item.selected { border-color: var(--mor-accent); box-shadow: 0 0 8px color-mix(in srgb, var(--mor-accent) 35%, transparent); }
.mr-lane-item.audio.selected { border-color: var(--mor-success); box-shadow: 0 0 8px color-mix(in srgb, var(--mor-success) 35%, transparent); }
.mr-lane-item.title.selected { border-color: var(--mor-warning); box-shadow: 0 0 8px color-mix(in srgb, var(--mor-warning) 35%, transparent); }

.mr-clips { position: relative; display: flex; margin-bottom: 6px; }
.mr-clip { flex: none; box-sizing: border-box; overflow: hidden; cursor: pointer; border: 2px solid transparent; border-radius: 6px; padding: 3px; background: var(--mor-panel); display: flex; flex-direction: column; gap: 2px; transition: border-color 0.12s; }
.mr-clip:hover { border-color: var(--mor-border-light); }
.mr-clip.selected { border-color: var(--mor-accent); box-shadow: 0 0 10px color-mix(in srgb, var(--mor-accent) 30%, transparent); }
.mr-clip img, .mr-thumb-missing { width: 100%; height: 72px; object-fit: cover; border-radius: 4px; display: block; background: #000; }
.mr-clip-name { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 10px; }
.mr-clip-dur { font-size: 10px; color: var(--mor-text-muted); }

/* Record-red playhead with a head cap — reads at a glance against amber/teal/gold. */
.mr-playhead { position: absolute; top: 0; bottom: 0; width: 2px; background: var(--mor-destructive); box-shadow: 0 0 6px color-mix(in srgb, var(--mor-destructive) 60%, transparent); pointer-events: none; }
.mr-playhead::before { content: ""; position: absolute; top: 0; left: -4px; border: 5px solid transparent; border-top: 6px solid var(--mor-destructive); }

/* Right-click menu: the kit's dropdown chrome, summoned at the pointer.
   Header chip names the lane the actions apply to, same colors as the timeline. */
.mr-ctx-backdrop { position: fixed; inset: 0; z-index: 400; }
.mr-ctx { display: block; position: fixed; margin: 0; width: 228px; z-index: 401; }
.mr-ctx-head { display: flex; align-items: center; gap: 6px; padding: 4px 10px 7px; border-bottom: 1px solid var(--mor-border-light); margin-bottom: 4px; }
.mr-ctx-tag { flex: none; font-size: 9px; font-weight: 700; padding: 1px 5px; border-radius: 3px; background: var(--mor-accent); color: #141417; }
.mr-ctx-tag.audio { background: var(--mor-success); }
.mr-ctx-tag.title { background: var(--mor-warning); }
.mr-ctx-name { font-size: 11px; color: var(--mor-text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
/* Destructive rows read record-red at rest and hover red, not accent. */
.mr-ctx .mor-menu-action.mr-danger { color: var(--mor-destructive); }
.mr-ctx .mor-menu-action.mr-danger:hover:not(:disabled) { background-color: var(--mor-destructive); color: #fff; }

.mr-shortcut-table { border-collapse: collapse; width: 100%; font-size: 13px; }
.mr-shortcut-table td { padding: 4px 10px 4px 0; }
.mr-key { color: var(--mor-accent-hover); white-space: nowrap; }
@media (max-width: 700px) {
    .mr-work { flex-direction: column; }
    .mr-phone { flex: none; width: auto; height: 45vh; }
    .mr-inspector { min-width: 0; }
}
@media (prefers-reduced-motion: reduce) {
    .mr-clip, .mr-progress > div { transition: none; }
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
            thumb: String::new(),
            proxy: String::new(),
        }
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
        for (name, filter) in EFFECTS.iter().skip(1) {
            assert!(!filter.is_empty(), "effect {name} has no filter");
        }
    }
}
