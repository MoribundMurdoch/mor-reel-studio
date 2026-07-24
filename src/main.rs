// SPDX-License-Identifier: GPL-3.0-or-later
// MorReel Studio — portrait-only (9:16) video editor.
// V1: main clip track (trim/reorder/split, ripple by construction).
// V2..Vn: free-timed overlay tracks (video/photo), higher number on top.
// T1..Tn: text/shape tracks above picture. A1..An: audio beds under V1.
// FX: adjustment layers between picture and text.

mod bevel;
mod coords;
#[cfg(target_os = "android")]
mod droid;
mod emoji;
mod engine;
mod giphy;
mod hub;
mod keyframe;
mod plugin;

#[cfg(not(target_os = "android"))]
use dioxus::desktop::tao::window::Icon;
#[cfg(not(target_os = "android"))]
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use engine::{AudioSpec, ClipSpec, OverlaySpec, TitleSpec};
use futures_util::StreamExt; // rx.next() in the live-control coroutine
use mor_rust_dioxus_ui_kit::{
    use_shortcut, MenuItem, MenuSeparator, Modal, MorAppFrame, MorCheckbox, MorMenuDropdown,
    MorSelect, MorShortcutRoot, MorStyleProvider, MorTabs, MorTextInput, Slider, UiMode,
};

// ponytail: rfd has no Android backend. Same call shape; file dialogs route
// to the in-app picker overlay in droid.rs. Kept in main.rs because this is
// the only file that uses rfd.
#[cfg(target_os = "android")]
mod rfd {
    use std::path::{Path, PathBuf};

    pub struct FileHandle(PathBuf);
    impl FileHandle {
        pub fn path(&self) -> &Path {
            &self.0
        }
        pub fn file_name(&self) -> String {
            self.0
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    }

    #[derive(Default)]
    pub struct AsyncFileDialog {
        exts: Vec<String>,
        title: String,
        file_name: Option<String>,
    }
    impl AsyncFileDialog {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn add_filter(mut self, _name: impl AsRef<str>, ext: &[impl AsRef<str>]) -> Self {
            self.exts.extend(ext.iter().map(|e| e.as_ref().to_lowercase()));
            self
        }
        pub fn set_title(mut self, t: impl AsRef<str>) -> Self {
            self.title = t.as_ref().to_string();
            self
        }
        pub fn set_file_name(mut self, n: impl AsRef<str>) -> Self {
            self.file_name = Some(n.as_ref().to_string());
            self
        }
        pub async fn pick_file(self) -> Option<FileHandle> {
            crate::droid::pick(self.title, self.exts, false).await.pop().map(FileHandle)
        }
        pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
            let v = crate::droid::pick(self.title, self.exts, true).await;
            (!v.is_empty()).then(|| v.into_iter().map(FileHandle).collect())
        }
        // ponytail: only the Plugin Hub asks for a folder — stays "cancelled"
        // until anyone needs a custom hub dir on a phone.
        pub async fn pick_folder(self) -> Option<FileHandle> {
            None
        }
        // ponytail: no save dialog — exports and projects land in the app's
        // USB-visible external files dir under the suggested name; the status
        // bar shows the full path.
        pub async fn save_file(self) -> Option<FileHandle> {
            let name = self.file_name.unwrap_or_else(|| "untitled".into());
            Some(FileHandle(crate::droid::save_dir().join(name)))
        }
    }
}

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

/// Quit: close the desktop window; exit the process on Android (an activity
/// has no window to close).
fn close_app() {
    #[cfg(not(target_os = "android"))]
    dioxus::desktop::window().close();
    #[cfg(target_os = "android")]
    std::process::exit(0);
}

/// Window / taskbar icon (128px RGBA PNG).
#[cfg(not(target_os = "android"))]
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

// ponytail: Android is one fullscreen webview activity — no WindowBuilder, no
// menus, no custom-head script (the shortcut guard matters less with no
// hardware keyboard). Phase 1 of the port: boot the same App.
#[cfg(target_os = "android")]
fn main() {
    UiMode::Native.apply_env();
    dioxus::launch(App);
}

#[cfg(not(target_os = "android"))]
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

mod model;
use model::*;

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
    /// Popped-out program monitor: the same `Editor` over shared state, rendering
    /// only the monitor stage + its transform/text handles. Editing in it (move,
    /// scale, rotate, stretch, text seat) writes the shared model, so the main
    /// window updates live — one project, two windows.
    Monitor,
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
    adjustments: Vec<AdjustmentItem> = Vec::<AdjustmentItem>::new,
    selected: Option<Sel> = || None,
    playhead: f64 = || 0.0f64,
    show_overlays: bool = || true,
    show_titles: bool = || true,
    v_lanes: u8 = || DEF_V_LANES,
    t_lanes: u8 = || DEF_T_LANES,
    a_lanes: u8 = || DEF_A_LANES,
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
    show_giphy: bool = || false,
    giphy_query: String = String::new,
    giphy_stickers: bool = || false,
    giphy_results: Vec<giphy::Gif> = Vec::<giphy::Gif>::new,
    giphy_busy: bool = || false,
    show_emoji: bool = || false,
    emoji_input: String = String::new,
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
    // Timeline in/out points — a playback range (work area). None = whole reel.
    mark_in: Option<f64> = || None,
    mark_out: Option<f64> = || None,
    settings: ProjectSettings = ProjectSettings::default,
    export_opts: engine::ExportOpts = engine::ExportOpts::default,
    // Share dialog: the exported file's name (FCP's title field), and the
    // label the progress toast shows while a long render runs.
    export_name: String = || "morreel".to_string(),
    export_label: String = String::new,
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
    // Seconds captured so far — grows the live A2 ghost tile during a take.
    vo_len: f64 = || 0.0,
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
        if cfg!(target_os = "android") {
            style { {ANDROID_CSS} }
        }
        MorShortcutRoot { Editor { state, view: EditorView::Full } }
        // ponytail: picker overlay is Android-only (rfd has no backend there).
        {android_picker()}
    }
}

#[cfg(target_os = "android")]
fn android_picker() -> Element {
    rsx! { droid::AndroidPicker {} }
}
#[cfg(not(target_os = "android"))]
fn android_picker() -> Element {
    rsx! {}
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

/// Program monitor window: the same `Editor` in `Monitor` view over the *shared*
/// state, so it's a live, editable monitor — move/scale/rotate/stretch the
/// selected layer and drag text right on the popped-out phone, and the main
/// window updates with it. Same trick as [`PoppedInspector`]. Runs in its own
/// VirtualDom, so it gets its own style provider; closing it docks the monitor
/// back (use_drop).
#[component]
fn PoppedMonitor(state: EditorState, out: Signal<bool>) -> Element {
    use_drop(move || out.set(false));
    rsx! {
        MorStyleProvider { theme_toml: Some(MORREEL_TOML.to_string()) }
        style { {APP_CSS} }
        MorShortcutRoot { Editor { state, view: EditorView::Monitor } }
    }
}

/// Logical viewport size for placing floated panels (desktop window, CSS px).
#[cfg(not(target_os = "android"))]
fn viewport_logical() -> (f64, f64) {
    let win = dioxus::desktop::window();
    let size = win.inner_size();
    let scale = win.scale_factor().max(0.1);
    (size.width as f64 / scale, size.height as f64 / scale)
}

/// ponytail: no window to measure on Android — a typical portrait-phone CSS
/// viewport is close enough for default float placement.
#[cfg(target_os = "android")]
fn viewport_logical() -> (f64, f64) {
    (412.0, 892.0)
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
    // The popped-out monitor window: show the monitor stage + handles, hide the
    // rail/inspector and timeline. Shares is_main's "no chrome" via the layout.
    let is_monitor = view == EditorView::Monitor;
    let mut clips = state.clips;
    let mut overlays = state.overlays;
    let mut audios = state.audios;
    let mut titles = state.titles;
    let mut adjustments = state.adjustments;
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
    let mut export_label = state.export_label;
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

    // Re-render the monitor as the timeline frame at `t`: V1 → overlays (by
    // track) → FX → titles, matching export so multi-track stacks read the same
    // on the monitor. Doesn't move the playhead or touch the selection — that's
    // `seek_to`'s job — so inspector edits can refresh in place.
    let mut refresh_monitor = move |t: f64| {
        let any_solo = clips.read().iter().any(|c| c.enabled && c.solo)
            || overlays.read().iter().any(|o| o.enabled && o.solo)
            || audios.read().iter().any(|a| a.enabled && a.solo);
        // All title cards active at t, low track under high (export order).
        let title_stack: Vec<(String, f64)> = if show_titles() {
            let ts = titles.read();
            let mut active: Vec<&TitleItem> = ts
                .iter()
                .filter(|ti| ti.enabled && t >= ti.at && t < ti.at + ti.dur && !ti.pngs.is_empty())
                .collect();
            active.sort_by_key(|ti| ti.track);
            active
                .into_iter()
                .filter_map(|ti| {
                    let k = ti.card_at(t).unwrap_or(0).min(ti.pngs.len().saturating_sub(1));
                    ti.pngs
                        .get(k)
                        .map(|p| (p.clone(), title_alpha(t, ti.at, ti.dur)))
                })
                .collect()
        } else {
            Vec::new()
        };
        // All enabled overlays at t, sorted low track → high (V2 under V3…).
        let overlay_layers: Vec<(String, f64, String, String)> = if show_overlays() {
            let ov = overlays.read();
            let mut ovs: Vec<&OverlayItem> = ov
                .iter()
                .filter(|o| o.enabled && t >= o.at && t < o.at + o.trimmed())
                .collect();
            ovs.sort_by_key(|o| o.track);
            ovs.into_iter()
                .map(|o| {
                    let mut look = o.look();
                    if any_solo && !o.solo {
                        look = join_chain(look, "hue=s=0".into());
                    }
                    (o.scrub_path(), o.src_at(t - o.at), o.framing.clone(), look)
                })
                .collect()
        } else {
            Vec::new()
        };
        let loc = locate(&clips.read(), t);
        // Adjustment layers covering the playhead grade the composite below the
        // titles. Chaining every active look mirrors the export, where each
        // adjustment applies in sequence over the running picture.
        let active_adjust: Vec<String> = adjustments
            .read()
            .iter()
            .filter(|a| a.enabled && t >= a.at && t < a.at + a.dur)
            .map(|a| a.look())
            .filter(|l| !l.is_empty())
            .collect();
        let adjust = (!active_adjust.is_empty()).then(|| active_adjust.join(","));
        let mut layers = engine::Over {
            titles: title_stack,
            layers: overlay_layers,
            adjust,
            ..Default::default()
        };
        if let Some((i, local)) = loc {
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
        } else if let Some((path, local, fr, eff)) = layers.layers.first().cloned() {
            // No V1: still show stacked cutaways (edge case / empty main track).
            let mut rest = layers;
            rest.layers.remove(0);
            request_preview(path, local, fr, eff, rest);
        }
    };

    // Seek: playhead moves, selection follows the V1 clip underneath (only in
    // the picture phases), monitor re-renders the composite at the new time.
    let mut seek_to = move |t: f64| {
        playhead.set(t);
        if let Some((i, _)) = locate(&clips.read(), t) {
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
        refresh_monitor(t);
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
    let mut v_lanes = state.v_lanes;
    let mut t_lanes = state.t_lanes;
    let mut a_lanes = state.a_lanes;
    let snapshot = move || Snapshot {
        clips: clips(),
        overlays: overlays(),
        audios: audios(),
        titles: titles(),
        adjustments: adjustments(),
        markers: markers(),
        mixer: mixer(),
        v_lanes: v_lanes(),
        t_lanes: t_lanes(),
        a_lanes: a_lanes(),
    };
    // Unsaved-changes tracking. Baseline = the reel's serialized form as last
    // saved or opened; None means never saved this session. Comparing the *JSON*
    // (not the struct) is deliberate: thumb/wave/proxy are `#[serde(skip)]`, so a
    // background proxy landing never counts as an edit — only what hits disk does.
    let mut saved_json = state.saved_json;
    let is_dirty = move || timeline_dirty(&snapshot(), saved_json().as_deref());
    let mut restore = move |s: Snapshot| {
        let mut s = s;
        normalize_lanes(&mut s);
        clips.set(s.clips);
        overlays.set(s.overlays);
        audios.set(s.audios);
        titles.set(s.titles);
        adjustments.set(s.adjustments);
        markers.set(s.markers);
        mixer.set(s.mixer);
        v_lanes.set(s.v_lanes);
        t_lanes.set(s.t_lanes);
        a_lanes.set(s.a_lanes);
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
    let mut show_giphy = state.show_giphy;
    let mut giphy_query = state.giphy_query;
    let mut giphy_stickers = state.giphy_stickers;
    let mut giphy_results = state.giphy_results;
    let mut giphy_busy = state.giphy_busy;
    let mut show_emoji = state.show_emoji;
    let mut emoji_input = state.emoji_input;
    // Search GIPHY (or clear on empty). One coroutine-free spawn, guarded by busy.
    let run_giphy_search = move |_: ()| {
        if giphy_busy() {
            return;
        }
        spawn(async move {
            let q = giphy_query().trim().to_string();
            if q.is_empty() {
                return;
            }
            let key = match giphy::api_key() {
                Ok(k) => k,
                Err(e) => {
                    status.set(e);
                    return;
                }
            };
            giphy_busy.set(true);
            status.set(format!("Searching GIPHY for “{q}”…"));
            match giphy::search(&q, giphy_stickers(), &key).await {
                Ok(hits) => {
                    status.set(format!("{} result(s) — click one to drop it on V2.", hits.len()));
                    giphy_results.set(hits);
                }
                Err(e) => status.set(format!("GIPHY: {e}")),
            }
            giphy_busy.set(false);
        });
    };
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
            Sel::Adjust(k) => adjustments.read().get(k).map_or(0, |a| a.group),
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
            Sel::Adjust(k) => adjustments.read().get(k).map(|a| a.at),
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
                Sel::Adjust(k) => adjustments.write()[k].at = at,
                Sel::Main(_) => {}
            }
            return;
        }
        let min_at = overlays.read().iter().filter(|o| o.group == gid).map(|o| o.at)
            .chain(audios.read().iter().filter(|a| a.group == gid).map(|a| a.at))
            .chain(titles.read().iter().filter(|t| t.group == gid).map(|t| t.at))
            .chain(adjustments.read().iter().filter(|a| a.group == gid).map(|a| a.at))
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
        for a in adjustments.write().iter_mut().filter(|a| a.group == gid) {
            a.at += dt;
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

    // I/O: trim the V1 clip under the playhead to the playhead — I keeps what
    // follows, O keeps what came before. The playhead then rides the new edge
    // so the monitor shows exactly the frame the trim landed on.
    let mut set_edge_here = move |out: bool| {
        let Some((i, src)) = locate(&clips.read(), playhead()) else {
            status.set("Add a clip first — I/O trim the clip under the playhead.".into());
            return;
        };
        push_undo("");
        let old = spans();
        let (name, refresh) = {
            let mut cl = clips.write();
            let c = &mut cl[i];
            // A reversed clip's timeline head is its source tail, so the
            // source edge that moves swaps.
            if out == c.reverse {
                c.in_s = src.min(c.out_s - 0.1).max(0.0);
            } else {
                c.out_s = src.max(c.in_s + 0.1).min(c.duration);
            }
            let refresh = (out == c.reverse)
                .then(|| (c.scrub_path(), c.path.clone(), c.in_s, c.framing.clone()));
            (c.name.clone(), refresh)
        };
        ride(old, &|k| Some(start_of(k)));
        let (s, e) = spans()[i];
        if out {
            status.set(format!("Out point set — {name} now ends at {}.", fmt_t(e)));
            seek_to((e - 0.05).max(s));
        } else {
            status.set(format!("In point set — {name} now starts at {}.", fmt_t(s)));
            seek_to(s);
        }
        // The old head frame is gone — retake the thumbnail at the new in point.
        if let Some((scrub, path, in_s, fr)) = refresh {
            spawn(async move {
                if let Ok(thumb) =
                    engine::frame_data_uri(&scrub, in_s, 108, 192, &fr, "", engine::Over::default())
                        .await
                {
                    if let Some(c) = clips.write().get_mut(i) {
                        if c.path == path && (c.in_s - in_s).abs() < 1e-6 {
                            c.thumb = thumb;
                        }
                    }
                }
            });
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
        // Park the playhead on the edge you just trimmed so the monitor shows it,
        // not a stale frame — the whole point of a ripple trim is seeing the cut.
        // OUT → last kept frame (edge minus a frame); IN → the clip's new first.
        let frame = 1.0 / engine::FPS as f64;
        seek_to(if edge_out {
            (start_of(i + 1) - frame).max(start_of(i))
        } else {
            start_of(i)
        });
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

    // A cutaway on overlay track `track` (2=V2…) starting at `at`.
    let mut add_overlay_path = move |path: String, at: f64, track: u8| {
        let track = track.max(2).min(1 + v_lanes());
        // Ensure the destination track is visible.
        if track > 1 + v_lanes() {
            v_lanes.set((track - 1).min(MAX_V_LANES));
        }
        spawn(async move {
            let name = file_name_of(&path);
            match engine::probe(&path).await {
                Ok((duration, _)) => {
                    push_undo("");
                    if track > 1 + v_lanes() {
                        v_lanes.set((track - 1).clamp(1, MAX_V_LANES));
                    }
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
                        track,
                        proxy: String::new(),
                        group: 0,
                        enabled: true,
                        solo: false,
                    });
                    queue_proxy(path);
                    // Show the new layer composited over the picture right away,
                    // then select it (after, so seek_to's clip-follow can't win).
                    refresh_monitor(at.max(0.0));
                    selected.set(Some(Sel::Over(overlays.read().len() - 1)));
                    status.set(format!(
                        "Overlay at {} — V{track} over V1 while it runs.",
                        fmt_t(at)
                    ));
                }
                Err(e) => status.set(format!("Could not add overlay: {e}")),
            }
        });
    };

    // Render an emoji to a cached transparent PNG, then drop it on the lowest
    // V-lane that's free at the playhead — growing a new lane when every
    // existing one is occupied, so the sticker never lands on top of (or under)
    // another overlay's window.
    let mut add_emoji = move |e: String| {
        let at = playhead();
        let track = {
            let overs = overlays.read();
            let busy = |t: u8| {
                overs.iter().any(|o| o.track == t && o.at <= at && at < o.at + o.trimmed())
            };
            match (2..=1 + v_lanes()).find(|t| !busy(*t)) {
                Some(t) => t,
                None => {
                    let grown = (v_lanes() + 1).min(MAX_V_LANES);
                    v_lanes.set(grown);
                    1 + grown
                }
            }
        };
        spawn(async move {
            match emoji::render(&e).await {
                Ok(path) => add_overlay_path(path.display().to_string(), at, track),
                Err(err) => status.set(format!("Emoji: {err}")),
            }
        });
    };

    let add_overlay = move |track: u8| {
        let track = track.max(2);
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Video & photos", &media_ext())
                .add_filter("All files", &["*"])
                .set_title(format!("Add overlay (V{track})"))
                .pick_file()
                .await
            else {
                return;
            };
            add_overlay_path(f.path().display().to_string(), playhead(), track);
        });
    };

    // Sound under the main track from `at` onto bus `lane` (1=A1, 2=A2, …).
    // A video dropped here contributes its soundtrack.
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
                    let bus = lane.max(1).min(MAX_A_LANES);
                    if bus > a_lanes() {
                        a_lanes.set(bus);
                    }
                    mixer.write().ensure_lanes(bus);
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
                    status.set(format!(
                        "Audio on A{bus} at {} — mixed under the main track.",
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
        let bus = lane.max(1).min(MAX_A_LANES);
        spawn(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .add_filter("Audio", engine::AUDIO_EXT)
                .add_filter("Video (use its soundtrack)", engine::VIDEO_EXT)
                .add_filter("All files", &["*"])
                .set_title(format!("Add audio (A{bus})"))
                .pick_file()
                .await
            else {
                return;
            };
            add_audio_path(f.path().display().to_string(), playhead(), bus);
        });
    };

    let mut add_v_track = move |_: ()| {
        if v_lanes() >= MAX_V_LANES {
            status.set(format!("Already at max video tracks (V{}).", 1 + MAX_V_LANES));
            return;
        }
        push_undo("");
        v_lanes.set(v_lanes() + 1);
        status.set(format!("Added video track V{}.", 1 + v_lanes()));
    };
    let mut add_t_track = move |_: ()| {
        if t_lanes() >= MAX_T_LANES {
            status.set(format!("Already at max text tracks (T{}).", MAX_T_LANES));
            return;
        }
        push_undo("");
        t_lanes.set(t_lanes() + 1);
        status.set(format!("Added text track T{}.", t_lanes()));
    };
    let mut add_a_track = move |_: ()| {
        if a_lanes() >= MAX_A_LANES {
            status.set(format!("Already at max audio tracks (A{}).", MAX_A_LANES));
            return;
        }
        push_undo("");
        a_lanes.set(a_lanes() + 1);
        mixer.write().ensure_lanes(a_lanes());
        status.set(format!("Added audio track A{}.", a_lanes()));
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

    let gather_specs = move || -> (Vec<ClipSpec>, Vec<OverlaySpec>, Vec<engine::AdjustSpec>, Vec<TitleSpec>, Vec<AudioSpec>) {
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
        // Sort by track so higher lanes composite last (on top) — matches preview.
        let mut o_ordered: Vec<_> = ov.iter().enumerate().filter(|(_, o)| o.enabled).collect();
        o_ordered.sort_by_key(|(i, o)| (o.track, *i));
        let ospecs = o_ordered
            .into_iter()
            .map(|(_, o)| {
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
        let titles_r = titles.read();
        let mut t_ordered: Vec<_> = titles_r
            .iter()
            .enumerate()
            .filter(|(_, t)| t.enabled && !t.pngs.is_empty())
            .collect();
        t_ordered.sort_by_key(|(i, t)| (t.track, *i));
        let tspecs = t_ordered
            .into_iter()
            .flat_map(|(_, t)| {
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
        let adjspecs = adjustments.read().iter().map(|a| a.spec()).collect();
        (specs, ospecs, adjspecs, tspecs, aspecs)
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
    let mut mark_in = state.mark_in;
    let mut mark_out = state.mark_out;
    // Playback range from the timeline in/out points; a degenerate or unset
    // range falls back to the whole reel.
    let play_range = move || {
        let end = total_of();
        let s = mark_in().unwrap_or(0.0).clamp(0.0, end);
        let e = mark_out().unwrap_or(end).clamp(0.0, end);
        if e > s + 0.05 { (s, e) } else { (0.0, end) }
    };
    let mut start_play = move || {
        if clips.read().is_empty() {
            return;
        }
        let g = play_gen() + 1;
        play_gen.set(g);
        playing.set(true);
        spawn(async move {
            let wav = std::env::temp_dir().join("morreel-playmix.wav");
            let (specs, _, _, _, aspecs) = gather_specs();
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
                let (rs, re) = play_range();
                if t >= re {
                    if loop_playback() {
                        seek_to(rs);
                        // Restart the mix from the range start so sound loops with picture.
                        if let Some(child) = audio.as_mut() {
                            let _ = child.start_kill();
                        }
                        audio = match engine::launch_audio(&wav, rs) {
                            Ok(child) => Some(child),
                            Err(_) => None,
                        };
                        last = std::time::Instant::now();
                        continue;
                    }
                    seek_to(re);
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
        let (rs, re) = play_range();
        if playhead() >= re - 0.05 {
            seek_to(rs); // replay from the top of the range
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

    // Timeline in/out points (mark in/out): pin the playback range to a slice
    // of the reel. Unlike I/O (which trim the clip), these cut nothing.
    let mut set_mark = move |out: bool| {
        let t = playhead();
        if out {
            mark_out.set(Some(t));
        } else {
            mark_in.set(Some(t));
        }
        let (rs, re) = play_range();
        status.set(format!(
            "{} point marked — playback runs {}..{} (Shift+X clears).",
            if out { "Out" } else { "In" },
            fmt_t(rs),
            fmt_t(re)
        ));
    };
    let mut clear_marks = move |_: ()| {
        if mark_in().is_none() && mark_out().is_none() {
            return;
        }
        mark_in.set(None);
        mark_out.set(None);
        status.set("In/out points cleared — playback covers the whole reel.".to_string());
    };

    // Record voiceover onto A2 at the playhead (iMovie "Record Voiceover").
    // First press starts the mic; second press (or V) stops and lands the take.
    // Capture runs in a background task so the UI stays responsive.
    let mut vo_session = state.vo_session;
    let mut vo_stop = state.vo_stop;
    let mut vo_len = state.vo_len;
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
        vo_len.set(0.0);
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
            let started = std::time::Instant::now();
            loop {
                if vo_stop() {
                    break;
                }
                // Grow the A2 ghost tile in step with the take (~12 fps).
                vo_len.set(started.elapsed().as_secs_f64());
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
            export_label.set("Transcribing audio for captions".to_string());
            let res = {
                let (specs, _, _, _, _) = gather_specs();
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
                Err(e) if e == "cancelled" => status.set("Transcription cancelled.".to_string()),
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
    let mut export_name = state.export_name;

    let mut run_export = move |_: ()| {
        if clips.read().is_empty() || export_progress().is_some() {
            return;
        }
        playing.set(false);
        show_export.set(false);
        spawn(async move {
            let opts = export_opts();
            // The share dialog's name field seeds the save dialog; slashes
            // would silently become directories.
            let name = export_name().replace('/', "-").trim().to_string();
            let name = if name.is_empty() { "morreel".to_string() } else { name };
            let Some(file) = rfd::AsyncFileDialog::new()
                .add_filter(opts.format.label(), &[opts.format.ext()])
                .set_file_name(format!("{name}.{}", opts.format.ext()))
                .set_title("Export portrait video")
                .save_file()
                .await
            else {
                return;
            };
            ensure_titles().await;
            let (specs, ospecs, adjspecs, tspecs, aspecs) = gather_specs();
            export_progress.set(Some(0.0));
            export_label.set(format!("Exporting {name} — {} at {}", opts.format.label(), engine::size_label(opts.width)));
            status.set(format!("Exporting {} at {}…", opts.format.label(), engine::size_label(opts.width)));
            let res = engine::export(&specs, &ospecs, &adjspecs, &tspecs, &aspecs, file.path(), opts, |p| {
                export_progress.set(Some(p))
            })
            .await;
            export_progress.set(None);
            match res {
                Ok(()) => status.set(format!("Exported {}", file.path().display())),
                Err(e) if e == "cancelled" => {
                    // A killed ffmpeg leaves a truncated file behind — sweep it.
                    let _ = std::fs::remove_file(file.path());
                    status.set("Export cancelled.".to_string());
                }
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
            let (specs, ospecs, adjspecs, tspecs, aspecs) = gather_specs();
            export_progress.set(Some(0.0));
            export_label.set("Rendering full preview".to_string());
            status.set("Rendering preview…".to_string());
            let res = engine::export(&specs, &ospecs, &adjspecs, &tspecs, &aspecs, &out, engine::ExportOpts::preview(), |p| {
                export_progress.set(Some(p))
            })
            .await;
            export_progress.set(None);
            match res {
                Ok(()) => match engine::launch_player(&out) {
                    Ok(player) => status.set(format!("Playing preview in {player}.")),
                    Err(e) => status.set(format!("Preview rendered but {e}")),
                },
                Err(e) if e == "cancelled" => status.set("Preview render cancelled.".to_string()),
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
            Some(Sel::Adjust(k)) => {
                adjustments.write().remove(k);
                selected.set(None);
                seek_to(playhead()); // its look leaves the composite
            }
            None => {}
        }
    };

    // Drop a full-frame adjustment layer at the playhead. Default span reaches
    // the end of the reel (min 1 s), which is the usual "grade the rest of this"
    // intent; trim it on the FX lane or move it like any other lane item.
    let mut add_adjustment = move |_: ()| {
        push_undo("");
        let at = playhead();
        let dur = (total_of() - at).max(1.0);
        adjustments.write().push(AdjustmentItem::new(at, dur));
        selected.set(Some(Sel::Adjust(adjustments.read().len() - 1)));
        seek_to(at);
        status.set("Adjustment layer added — grade or add an effect in the Style inspector.".to_string());
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

    // One lane arm of Disable/Solo: flip `$flag` on the item, report through the
    // caller's message closure, and (usually) reseek so the preview reflects it.
    macro_rules! flip_flag {
        ($xs:ident, $i:expr, $flag:ident, $seek:expr, |$it:ident, $on:ident| $msg:expr) => {{
            push_undo("");
            let mut xs = $xs.write();
            let Some(item) = xs.get_mut($i) else { return };
            item.$flag = !item.$flag;
            let msg = {
                let $on = item.$flag;
                let $it = &*item;
                $msg
            };
            drop(xs);
            status.set(msg);
            if $seek {
                seek_to(playhead());
            }
        }};
    }

    // FCP-style Disable: item stays on the timeline but is invisible + silent
    // in preview and export. (V is already voiceover — Shift+D here.)
    let mut toggle_disable_sel = move |_: ()| match selected() {
        Some(Sel::Main(i)) => flip_flag!(clips, i, enabled, true, |c, on| if on {
            format!("{} enabled — back in preview and export.", c.name)
        } else {
            format!("{} disabled — dimmed on the timeline, invisible and silent.", c.name)
        }),
        Some(Sel::Over(j)) => flip_flag!(overlays, j, enabled, true, |o, on| if on {
            format!("{} enabled.", o.name)
        } else {
            format!("{} disabled — cutaway hidden.", o.name)
        }),
        Some(Sel::Aud(k)) => flip_flag!(audios, k, enabled, false, |a, on| if on {
            format!("{} enabled.", a.name)
        } else {
            format!("{} disabled — silent.", a.name)
        }),
        Some(Sel::Title(k)) => flip_flag!(titles, k, enabled, true, |_t, on| if on {
            "Title enabled.".to_string()
        } else {
            "Title disabled — not composited.".to_string()
        }),
        Some(Sel::Adjust(k)) => flip_flag!(adjustments, k, enabled, true, |_a, on| if on {
            "Adjustment layer enabled.".to_string()
        } else {
            "Adjustment layer disabled — its look drops out.".to_string()
        }),
        None => status.set("Select a clip, cutaway, bed, title or FX layer to disable.".into()),
    };

    // FCP-style Solo: isolate selected item's audio; non-soloed picture goes
    // B&W. Toggle off when already soloed alone, or clear all solos with a
    // second press on the same item.
    let mut toggle_solo_sel = move |_: ()| match selected() {
        Some(Sel::Main(i)) => flip_flag!(clips, i, solo, true, |c, on| if on {
            format!("{} soloed — other audio silent; non-soloed clips in B&W.", c.name)
        } else {
            format!("{} unsoloed.", c.name)
        }),
        Some(Sel::Over(j)) => flip_flag!(overlays, j, solo, true, |o, on| if on {
            format!("{} soloed.", o.name)
        } else {
            format!("{} unsoloed.", o.name)
        }),
        Some(Sel::Aud(k)) => flip_flag!(audios, k, solo, false, |a, on| if on {
            format!("{} soloed — only soloed beds play.", a.name)
        } else {
            format!("{} unsoloed.", a.name)
        }),
        _ => status.set("Select a clip, cutaway or bed to solo.".into()),
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
            // ponytail: adjustment copy/paste not wired yet — cheap to add a
            // ClipboardItem::Adjust variant when someone wants to duplicate one.
            Some(Sel::Adjust(_)) => None,
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

    // Paste just the *look* — transform, grade and effect — from the last copied
    // clip/cutaway onto the selected one, keeping its media and timing. FCP's
    // "Paste Attributes": the fast way to make several clips match one.
    let mut paste_attrs = move |_: ()| {
        let Some(item) = clipboard() else {
            status.set("Copy a clip or cutaway first (Ctrl+C), then paste its look.".to_string());
            return;
        };
        let attrs = match &item {
            ClipboardItem::Main(c) => Some((c.transform.clone(), c.grade, c.effect.clone(), c.effect_amount)),
            ClipboardItem::Over(o) => Some((o.transform.clone(), o.grade, o.effect.clone(), o.effect_amount)),
            _ => None,
        };
        let Some((xf, gr, eff, amt)) = attrs else {
            status.set("The clipboard holds audio or a title — copy a clip or cutaway to paste a look.".to_string());
            return;
        };
        push_undo("");
        let applied = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                cl[i].transform = xf;
                cl[i].grade = gr;
                cl[i].effect = eff;
                cl[i].effect_amount = amt;
                true
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                ov[j].transform = xf;
                ov[j].grade = gr;
                ov[j].effect = eff;
                ov[j].effect_amount = amt;
                true
            }
            _ => false,
        };
        if applied {
            seek_to(playhead());
            status.set("Pasted look — transform, grade and effect.".to_string());
        } else {
            status.set("Select a V1 clip or V2 cutaway to paste the look onto.".to_string());
        }
    };

    // Strip the added look back to defaults — identity transform, neutral grade,
    // no effect. FCP's "Remove Attributes". Framing, speed and volume stay: they
    // are how the source fills the frame and plays, not styling. Works on an FX
    // layer too (it carries grade + effect, no transform).
    let mut reset_attrs = move |_: ()| {
        push_undo("");
        let applied = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                let mut cl = clips.write();
                cl[i].transform = engine::AnimatedTransform::default();
                cl[i].grade = engine::Grade::default();
                cl[i].effect = "None".into();
                cl[i].effect_amount = 1.0;
                true
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                let mut ov = overlays.write();
                ov[j].transform = engine::AnimatedTransform::default();
                ov[j].grade = engine::Grade::default();
                ov[j].effect = "None".into();
                ov[j].effect_amount = 1.0;
                true
            }
            Some(Sel::Adjust(k)) if k < adjustments.read().len() => {
                let mut aj = adjustments.write();
                aj[k].grade = engine::Grade::default();
                aj[k].effect = "None".into();
                aj[k].effect_amount = 1.0;
                true
            }
            _ => false,
        };
        if applied {
            seek_to(playhead());
            status.set("Reset look to default.".to_string());
        } else {
            status.set("Select a clip, cutaway or FX layer to reset its look.".to_string());
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

    // Frame-nudge the selected lane item (cutaway / title / bed / FX) along the
    // timeline — the keyboard equivalent of the audio ±1f buttons, extended to
    // every free-position lane. V1 is magnetic (gapless concat), so it has no
    // free position to nudge. Grouped members ride along (shift_lane).
    let mut nudge_item = move |frames: f64| {
        let Some(sel) = selected() else {
            status.set("Select a cutaway, title, bed or FX layer to nudge.".to_string());
            return;
        };
        if matches!(sel, Sel::Main(_)) {
            status.set("V1 clips are magnetic — nudge a cutaway, title, bed or FX layer instead.".to_string());
            return;
        }
        shift_lane(sel, frames / engine::FPS as f64);
        seek_to(playhead());
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
    // Dragging a text card up/down between T-lanes restacks it. (title index,
    // track at grab, grab client-y). Higher track composites on top.
    let mut title_lane_drag = use_signal(|| Option::<(usize, u8, f64)>::None);
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

    // Refresh the monitor after editing the selected item's look/transform: the
    // full timeline composite (layers under and over included, so a transparent
    // sticker shows over the picture it rides on), at the playhead when it sits
    // inside the item, else clamped into the item's span so the change is still
    // visible. Unlike `seek_to` this never steals the selection.
    let mut refresh_sel_monitor = move || {
        let t = playhead();
        let span = match selected() {
            Some(Sel::Main(i)) => {
                let cl = clips.read();
                cl.get(i).map(|c| {
                    let start: f64 = extents(&cl).iter().take(i).sum();
                    (start, start + c.trimmed())
                })
            }
            Some(Sel::Over(j)) => overlays.read().get(j).map(|o| (o.at, o.at + o.trimmed())),
            _ => None,
        };
        let t = match span {
            Some((a, b)) => t.clamp(a, (b - 0.001).max(a)),
            None => t,
        };
        refresh_monitor(t);
    };

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
        let wrote = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                clips.write()[i].transform.set_pose(t);
                true
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                overlays.write()[j].transform.set_pose(t);
                true
            }
            _ => false,
        };
        if wrote {
            refresh_sel_monitor();
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
    // time, then refresh the monitor at the playhead (clamped into the item's
    // span) so a keyed value shows where it was set. Never steals the selection,
    // so an overlay's opacity can be keyed while it stays selected.
    let mut edit_sel_at = move |edit: &dyn Fn(&mut engine::AnimatedTransform, f64)| {
        let Some((start, speed, _in_s, dur)) = sel_anchor() else { return };
        let t = ((playhead() - start) * speed).clamp(0.0, dur);
        let wrote = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                edit(&mut clips.write()[i].transform, t);
                true
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                edit(&mut overlays.write()[j].transform, t);
                true
            }
            _ => false,
        };
        if wrote {
            refresh_sel_monitor();
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

    // The ease of the key sitting at the playhead for this row, if any — drives
    // the velocity chip. Same clip-local clock as edit_sel_at.
    let sel_key_interp = move |label: &str| -> Option<keyframe::Interp> {
        let (start, speed, _in_s, dur) = sel_anchor()?;
        let t = ((playhead() - start) * speed).clamp(0.0, dur);
        match selected() {
            Some(Sel::Main(i)) => clips.read().get(i).and_then(|c| xf_key_interp(&c.transform, label, t)),
            Some(Sel::Over(j)) => overlays.read().get(j).and_then(|o| xf_key_interp(&o.transform, label, t)),
            _ => None,
        }
    };

    // The selected element's grade, whichever lane it is on. Mirrors
    // selected_xf so the one grade panel drives both V1 and V2.
    let selected_grade = move || match selected() {
        Some(Sel::Main(i)) => clips.read().get(i).map(|c| c.grade),
        Some(Sel::Over(j)) => overlays.read().get(j).map(|o| o.grade),
        Some(Sel::Adjust(k)) => adjustments.read().get(k).map(|a| a.grade),
        _ => None,
    };
    let mut set_selected_grade = move |g: engine::Grade| {
        let wrote = match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                clips.write()[i].grade = g;
                true
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                overlays.write()[j].grade = g;
                true
            }
            Some(Sel::Adjust(k)) if k < adjustments.read().len() => {
                adjustments.write()[k].grade = g;
                true
            }
            _ => false,
        };
        if wrote {
            refresh_sel_monitor();
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

    // Sub-tabs inside the Style phase — the dense one — split into Look (grade +
    // framing) and Transform (position/scale/rotate + Ken Burns) so neither view
    // is a long scroll.
    let mut style_tab = state.style_tab;

    // Grabbing a handle measures the monitor first, so the very first pointer
    // move already has real geometry to work against.
    let mut begin_xf = move |grab: XfGrab, from: (f64, f64)| {
        let Some(start) = selected_xf() else { return };
        let Some(el) = phone_el() else { return };
        // The drag edits the transform, so show its numbers while they move.
        style_tab.set("Transform".to_string());
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
                        // Velocity: when a key sits at the playhead, cycle its
                        // ease (Ease → Lin → Hold). Each maps to a different
                        // segment curve in the engine, so the tween's speed
                        // changes in preview and export alike.
                        if let Some(cur) = sel_key_interp(label) {
                            button {
                                r#type: "button",
                                class: "mr-kf-ease",
                                title: "Keyframe velocity — click to cycle ease in/out, linear, hold",
                                onclick: move |_| {
                                    push_undo("keyframe-ease");
                                    edit_sel_at(&|at, t| xf_cycle_interp(at, label, t));
                                    status.set("Keyframe velocity changed.".to_string());
                                },
                                "{interp_glyph(cur)}"
                            }
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
    use_shortcut(Some(" ".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_play(()))));
    use_shortcut(Some("BACKSPACE".to_string()), is_main.then(|| EventHandler::new(move |()| delete_sel(()))));
    use_shortcut(Some("ARROWLEFT".to_string()), is_main.then(|| EventHandler::new(move |()| nudge(-0.1))));
    use_shortcut(Some("ARROWRIGHT".to_string()), is_main.then(|| EventHandler::new(move |()| nudge(0.1))));
    use_shortcut(Some("SHIFT+ARROWLEFT".to_string()), is_main.then(|| EventHandler::new(move |()| nudge(-1.0))));
    use_shortcut(Some("SHIFT+ARROWRIGHT".to_string()), is_main.then(|| EventHandler::new(move |()| nudge(1.0))));
    use_shortcut(Some("HOME".to_string()), is_main.then(|| EventHandler::new(move |()| seek_to(0.0))));
    use_shortcut(Some("END".to_string()), is_main.then(|| EventHandler::new(move |()| seek_to(total_of()))));
    // Frame-step the playhead (1/30 s — engine::FPS). Finer than the arrows'
    // 0.1s; `,`/`.` is the editor-standard single-frame scrub.
    let frame = 1.0 / engine::FPS as f64;
    use_shortcut(Some(",".to_string()), is_main.then(|| EventHandler::new(move |()| seek_to((playhead() - frame).max(0.0)))));
    use_shortcut(Some(".".to_string()), is_main.then(|| EventHandler::new(move |()| seek_to((playhead() + frame).min(total_of())))));
    // Ripple-trim the selected clip a frame at a time — keyboard precision edit.
    // Alt+,/. nudge the clip's OUT (end); add Shift for the IN (start). Modifier
    // order must match the registry's CTRL+SHIFT+ALT build order.
    use_shortcut(Some("ALT+,".to_string()), is_main.then(|| EventHandler::new(move |()| ripple_trim(true, -1.0))));
    use_shortcut(Some("ALT+.".to_string()), is_main.then(|| EventHandler::new(move |()| ripple_trim(true, 1.0))));
    use_shortcut(Some("SHIFT+ALT+,".to_string()), is_main.then(|| EventHandler::new(move |()| ripple_trim(false, -1.0))));
    use_shortcut(Some("SHIFT+ALT+.".to_string()), is_main.then(|| EventHandler::new(move |()| ripple_trim(false, 1.0))));
    // < / > (Shift+,/.) move the selected lane item one frame; add Ctrl for ten —
    // frame-accurate sync for cutaways, captions, music and FX layers.
    use_shortcut(Some("SHIFT+,".to_string()), is_main.then(|| EventHandler::new(move |()| nudge_item(-1.0))));
    use_shortcut(Some("SHIFT+.".to_string()), is_main.then(|| EventHandler::new(move |()| nudge_item(1.0))));
    use_shortcut(Some("CTRL+SHIFT+,".to_string()), is_main.then(|| EventHandler::new(move |()| nudge_item(-10.0))));
    use_shortcut(Some("CTRL+SHIFT+.".to_string()), is_main.then(|| EventHandler::new(move |()| nudge_item(10.0))));
    // Prev/next edit point (V1 cut).
    use_shortcut(Some("ARROWUP".to_string()), is_main.then(|| EventHandler::new(move |()| jump_edit(-1))));
    use_shortcut(Some("ARROWDOWN".to_string()), is_main.then(|| EventHandler::new(move |()| jump_edit(1))));
    use_shortcut(Some("[".to_string()), is_main.then(|| EventHandler::new(move |()| step_sel(-1))));
    use_shortcut(Some("]".to_string()), is_main.then(|| EventHandler::new(move |()| step_sel(1))));
    use_shortcut(Some("ESCAPE".to_string()), is_main.then(|| EventHandler::new(move |()| {
        if vo_session().is_some() {
            stop_voiceover(());
        } else {
            ctx_menu.set(None);
        }
    })));
    use_shortcut(Some("V".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_voiceover(()))));
    use_shortcut(Some("G".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_safe(()))));
    use_shortcut(Some("T".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_handles(()))));
    use_shortcut(Some("M".to_string()), is_main.then(|| EventHandler::new(move |()| drop_marker(()))));
    use_shortcut(Some("SHIFT+M".to_string()), is_main.then(|| EventHandler::new(move |()| clear_markers(()))));
    use_shortcut(Some("B".to_string()), is_main.then(|| EventHandler::new(move |()| analyze_beats(()))));
    // Disable/Solo: FCP uses V / Option-S; V is voiceover here, so Shift+D / Alt+S.
    use_shortcut(Some("SHIFT+D".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_disable_sel(()))));
    use_shortcut(Some("ALT+S".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_solo_sel(()))));
    // The menu item binds "~"; this covers layouts where ~ is Shift+` and the
    // combo therefore arrives as SHIFT+~.
    use_shortcut(Some("SHIFT+~".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_magnet(()))));

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
    // Ctrl+, — the platform-conventional "preferences" shortcut.
    use_shortcut(Some("Ctrl+,".to_string()), is_main.then(|| EventHandler::new(move |()| show_settings.set(true))));
    let mut set_mode = move |m: UiMode| {
        preferred_mode.set(m);
        let _ = m.save_preference();
        status.set(format!("Window mode → {m} (applies on next launch)"));
    };
    let radio = move |m: UiMode| if preferred_mode() == m { "●" } else { "○" };

    // Pop-out program monitor: the monitor MOVES to its own OS window — the
    // embedded phone hides while it's out, and closing the window docks it back.
    let mut monitor_out = state.monitor_out;
    // Android's stand-in for the pop-out monitor: the phone preview expands to
    // fill the screen (CapCut's expand), toggled from the transport.
    let mut mon_full = use_signal(|| false);
    let mut open_monitor = move || {
        if monitor_out() {
            return;
        }
        #[cfg(not(target_os = "android"))]
        {
            use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
            let dom = VirtualDom::new_with_props(
                PoppedMonitor,
                PoppedMonitorProps { state, out: monitor_out },
            );
            let cfg = Config::new()
                .with_menu(None::<dioxus::desktop::muda::Menu>)
                .with_window(
                    WindowBuilder::new()
                        .with_title("MorReel Monitor")
                        .with_inner_size(LogicalSize::new(414.0, 764.0))
                        .with_window_icon(window_icon()),
                );
            let _ = dioxus::desktop::window().new_window(dom, cfg);
            monitor_out.set(true);
            status.set("Monitor popped out — close its window to dock it back.".to_string());
        }
        #[cfg(target_os = "android")]
        status.set("Pop-out windows aren't available on Android.".to_string());
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
        #[cfg(not(target_os = "android"))]
        {
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
                        .with_inner_size(LogicalSize::new(430.0, 900.0))
                        .with_window_icon(window_icon()),
                );
            let _ = dioxus::desktop::window().new_window(dom, cfg);
            inspector_out.set(true);
            status.set("Inspector popped out — close its window to dock it back.".to_string());
        }
        #[cfg(target_os = "android")]
        status.set("Pop-out windows aren't available on Android.".to_string());
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
    use_shortcut(Some("CTRL+=".to_string()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.25))));
    use_shortcut(Some("CTRL+-".to_string()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.0 / 1.25))));
    use_shortcut(Some("CTRL++".to_string()), is_main.then(|| EventHandler::new(move |()| zoom_by(1.25))));
    // Clip appearance modes — FCP Control-Option-1…6.
    use_shortcut(Some("CTRL+ALT+1".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Wave))));
    use_shortcut(Some("CTRL+ALT+2".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::WaveFilm))));
    use_shortcut(Some("CTRL+ALT+3".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Equal))));
    use_shortcut(Some("CTRL+ALT+4".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::FilmWave))));
    use_shortcut(Some("CTRL+ALT+5".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Film))));
    use_shortcut(Some("CTRL+ALT+6".to_string()), is_main.then(|| EventHandler::new(move |()| clip_appear.set(ClipAppear::Labels))));
    // Clip height — FCP Control-Option-Up/Down for waveform size.
    use_shortcut(Some("CTRL+ALT+ARROWUP".to_string()), is_main.then(|| EventHandler::new(move |()| {
        clip_height.set((clip_height() + 0.15).min(2.0));
    })));
    use_shortcut(Some("CTRL+ALT+ARROWDOWN".to_string()), is_main.then(|| EventHandler::new(move |()| {
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
                        Lane::V(n) => add_overlay_path(path, at, n),
                        Lane::A(n) => add_audio_path(path, at, n),
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
    let mut float_drag = use_signal(|| Option::<(f64, f64, f64, f64)>::None);
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
    // Titlebar drag only — resizing is the webview's own CSS `resize: both`.
    let mut begin_float = move |mx: f64, my: f64| {
        let (x, y, _, _) = pin_float_geom();
        float_drag.set(Some((mx, my, x, y)));
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
        #[cfg(not(target_os = "android"))]
        dioxus::desktop::window().set_fullscreen(is_fullscreen());
        status.set(if is_fullscreen() {
            "Fullscreen — F11 or the View menu to exit.".to_string()
        } else {
            "Windowed.".to_string()
        });
    };
    use_shortcut(Some("F11".to_string()), is_main.then(|| EventHandler::new(move |()| toggle_fullscreen(()))));

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
                clips.write()[i].effect = name.clone();
                refresh_sel_monitor();
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                overlays.write()[j].effect = name.clone();
                refresh_sel_monitor();
            }
            Some(Sel::Adjust(k)) if k < adjustments.read().len() => {
                adjustments.write()[k].effect = name.clone();
                refresh_sel_monitor();
            }
            _ => status.set("Select a V1 clip, V2 overlay or FX layer to apply an effect.".to_string()),
        }
    };

    // Live strength change for the selected video item's effect.
    let mut set_effect_amount = move |v: f64| {
        push_undo("fx-amount");
        match selected() {
            Some(Sel::Main(i)) if i < clips.read().len() => {
                clips.write()[i].effect_amount = v;
                refresh_sel_monitor();
            }
            Some(Sel::Over(j)) if j < overlays.read().len() => {
                overlays.write()[j].effect_amount = v;
                refresh_sel_monitor();
            }
            Some(Sel::Adjust(k)) if k < adjustments.read().len() => {
                adjustments.write()[k].effect_amount = v;
                refresh_sel_monitor();
            }
            _ => {}
        }
    };

    // One effects-palette tile grid — every family renders the same
    // tile/active/placeholder shape and differs only in what a click applies.
    // Items are (label, click payload, is-active, placeholder style).
    let tile_grid = move |items: Vec<(String, String, bool, Option<String>)>,
                          onpick: EventHandler<String>| {
        rsx! {
            div { class: "mr-fx-grid",
                for (label, payload, active, ph) in items {
                    button {
                        key: "{label}",
                        class: if active { "mr-fx-tile active" } else { "mr-fx-tile" },
                        onclick: move |_| onpick.call(payload.clone()),
                        div { class: "mr-fx-ph", style: ph.unwrap_or_default() }
                        span { "{label}" }
                    }
                }
            }
        }
    };

    // Context-menu rows every lane repeats, like `group_rows` above.
    let copy_paste_rows = move || {
        rsx! {
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
        }
    };
    let disable_solo_rows = move |enabled: bool, soloed: bool| {
        rsx! {
            CtxItem {
                label: if enabled { "Disable".to_string() } else { "Enable".to_string() },
                shortcut: Some("Shift+D".to_string()),
                on_action: move |_| toggle_disable_sel(()),
            }
            CtxItem {
                label: if soloed { "Unsolo".to_string() } else { "Solo".to_string() },
                shortcut: Some("Alt+S".to_string()),
                on_action: move |_| toggle_solo_sel(()),
            }
        }
    };
    // "Move track up/down" pair — bumps within [lo, hi]; `set` writes the
    // clamped value back to the item's track/lane field.
    let move_track_rows = move |cur: u8, lo: u8, hi: u8, set: EventHandler<u8>| {
        rsx! {
            CtxItem {
                label: "Move track up".to_string(),
                disabled: cur >= hi,
                on_action: move |_| set.call((cur + 1).min(hi)),
            }
            CtxItem {
                label: "Move track down".to_string(),
                disabled: cur <= lo,
                on_action: move |_| set.call(cur.saturating_sub(1).max(lo)),
            }
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

    // The workspace phase buttons, one definition rendered in two homes: the
    // two flanks hugging the phone in the main window, and a horizontal bar
    // under the popped-out monitor's transport — so switching phases doesn't
    // require a trip back to the main window.
    // (phase, icon, label, tooltip, needs a selected clip, shows a done-tick)
    let wf_defs: [(Phase, &str, &str, &str, bool, bool); 8] = [
        (Phase::Add, "＋", "Add", "Add clips, b-roll or music", false,
            !clips.read().is_empty()),
        (Phase::Cut, "✂", "Cut", "Trim, split and arrange the current clip", true, false),
        (Phase::Style, "✦", "Style", "Effects, transform and Ken Burns for the current clip", true,
            clips.read().iter().any(|c| c.effect != "None" || c.transform.scale.is_animated())),
        (Phase::Effects, "◧", "FX", "Chroma key and image/particle effects for the current clip or overlay", true,
            clips.read().iter().any(|c| is_keyer(&c.effect))
                || overlays.read().iter().any(|o| is_keyer(&o.effect) || !o.blend.is_empty())),
        (Phase::Background, "▧", "Bg", "Frame background behind banded or shrunk clips", false,
            clips.read().iter().any(|c| c.transform.bg != engine::Bg::Black)),
        (Phase::Text, "T", "Text", "Text and captions", false, !titles.read().is_empty()),
        (Phase::Audio, "♪", "Audio", "Music and voiceover under the picture", false,
            !audios.read().is_empty()),
        (Phase::Export, "⇪", "Export", "Export your reel", false, false),
    ];
    let wf_button = move |(phase, icon, label, tip, needs_sel, tick): (Phase, &'static str, &'static str, &'static str, bool, bool)| rsx! {
        button {
            key: "{label}",
            class: if active_phase() == phase { "mr-wf active" } else { "mr-wf" },
            class: if phase == Phase::Export { "mr-wf-export" },
            title: "{tip}",
            onclick: move |_| {
                if needs_sel && selected().is_none() {
                    if let Some((i, _)) = locate(&clips.read(), playhead()) { selected.set(Some(Sel::Main(i))); }
                }
                active_phase.set(phase);
            },
            span { class: "mr-wf-icon", "{icon}" }
            span { class: "mr-wf-label", "{label}" }
            if tick { span { class: "mr-wf-tick", "✓" } }
        }
    };
    let wf_phases_a = move || rsx! { for d in wf_defs[..4].iter().copied() { {wf_button(d)} } };
    let wf_phases_b = move || rsx! { for d in wf_defs[4..].iter().copied() { {wf_button(d)} } };

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
                            if !is_dirty() {
                                close_app();
                                return;
                            }
                            // ponytail: rfd message dialogs don't exist on Android —
                            // refuse quit and say so instead of a dead Yes/No shim.
                            #[cfg(target_os = "android")]
                            {
                                status.set("Save the reel first (File → Save), then Quit.".into());
                            }
                            #[cfg(not(target_os = "android"))]
                            spawn(async move {
                                let r = rfd::AsyncMessageDialog::new()
                                    .set_title("Unsaved changes")
                                    .set_description("This reel has unsaved changes. Quit without saving?")
                                    .set_buttons(rfd::MessageButtons::YesNo)
                                    .show()
                                    .await;
                                if r == rfd::MessageDialogResult::Yes {
                                    close_app();
                                }
                            });
                        },
                    }
                }
                // "Edit" — history, clipboard, delete, grouping: verbs that act
                // on whatever is selected. Right after File, as everywhere.
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
                    MenuItem {
                        label: "Paste look (transform + grade + effect)".to_string(),
                        shortcut: Some("Ctrl+Shift+V".to_string()),
                        disabled: clipboard().is_none()
                            || !matches!(selected(), Some(Sel::Main(_)) | Some(Sel::Over(_))),
                        on_action: move |_| paste_attrs(()),
                    }
                    MenuItem {
                        label: "Reset look".to_string(),
                        shortcut: Some("Ctrl+Shift+X".to_string()),
                        disabled: !matches!(
                            selected(),
                            Some(Sel::Main(_)) | Some(Sel::Over(_)) | Some(Sel::Adjust(_))
                        ),
                        on_action: move |_| reset_attrs(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Ripple delete".to_string(),
                        shortcut: Some("Delete".to_string()),
                        disabled: selected().is_none(),
                        on_action: move |_| delete_sel(()),
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
                // "Clip" — per-clip surgery, split out of Edit (FCP/Premiere's
                // Clip/Modify menu): trim, transitions, audio state, position.
                MorMenuDropdown { label: "Clip".to_string(),
                    MenuItem {
                        label: "Set in point at playhead".to_string(),
                        shortcut: Some("I".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_edge_here(false),
                    }
                    MenuItem {
                        label: "Set out point at playhead".to_string(),
                        shortcut: Some("O".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_edge_here(true),
                    }
                    MenuItem {
                        label: "Split at playhead".to_string(),
                        shortcut: Some(key_scheme().split().to_string()),
                        disabled: no_clips,
                        on_action: move |_| split_at_playhead(()),
                    }
                    MenuItem {
                        label: "Join clips".to_string(),
                        shortcut: Some("Ctrl+J".to_string()),
                        disabled: !matches!(selected(), Some(Sel::Main(_))) || exporting,
                        on_action: move |_| join_clips(()),
                    }
                    MenuSeparator {}
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
                                Some(Sel::Adjust(k)) => adjustments.read().get(k).map(|a| a.enabled),
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
                        on_action: move |_| add_overlay(2),
                    }
                    MenuItem {
                        label: "Add from Giphy…".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| show_giphy.set(true),
                    }
                    MenuItem {
                        label: "Add emoji…".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| show_emoji.set(true),
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
                    MenuItem {
                        label: "Add adjustment layer (FX)".to_string(),
                        disabled: no_clips || exporting,
                        on_action: move |_| add_adjustment(()),
                    }
                    MenuSeparator {}
                    MenuItem {
                        label: "Add video track (V)".to_string(),
                        disabled: v_lanes() >= MAX_V_LANES,
                        on_action: move |_| add_v_track(()),
                    }
                    MenuItem {
                        label: "Add text track (T)".to_string(),
                        disabled: t_lanes() >= MAX_T_LANES,
                        on_action: move |_| add_t_track(()),
                    }
                    MenuItem {
                        label: "Add audio track (A)".to_string(),
                        disabled: a_lanes() >= MAX_A_LANES,
                        on_action: move |_| add_a_track(()),
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
                        label: match mark_in() {
                            Some(t) => format!("Mark in at playhead (now {})", fmt_t(t)),
                            None => "Mark in at playhead".to_string(),
                        },
                        shortcut: Some("Shift+I".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_mark(false),
                    }
                    MenuItem {
                        label: match mark_out() {
                            Some(t) => format!("Mark out at playhead (now {})", fmt_t(t)),
                            None => "Mark out at playhead".to_string(),
                        },
                        shortcut: Some("Shift+O".to_string()),
                        disabled: no_clips,
                        on_action: move |_| set_mark(true),
                    }
                    MenuItem {
                        label: "Clear in/out points".to_string(),
                        shortcut: Some("Shift+X".to_string()),
                        disabled: mark_in().is_none() && mark_out().is_none(),
                        on_action: move |_| clear_marks(()),
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
                    // No second OS window on Android — the fullscreen monitor
                    // and docked inspector are the phone equivalents.
                    if cfg!(not(target_os = "android")) {
                    MenuItem {
                        label: "Pop out monitor".to_string(),
                        disabled: monitor_out(),
                        on_action: move |_| open_monitor(),
                    }
                    }
                    if cfg!(not(target_os = "android")) {
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
                        shortcut: Some("Ctrl+/".to_string()),
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
                } else if inspector_out() {
                    span { class: "mor-statusbar-chip", "inspector in its own window" }
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

            div { class: if is_monitor { "mr-root mr-root-mon" } else { "mr-root" },
                // Releasing the mouse ends an interaction, so the next drag of
                // the same slider or item starts a fresh undo step instead of
                // collapsing into the previous one's snapshot.
                onmouseup: move |_| {
                    undo_tag.set(String::new());
                    float_drag.set(None);
                },
                onmousemove: move |evt| {
                    let Some((mx, my, ox, oy)) = float_drag() else { return };
                    let p = evt.client_coordinates();
                    let (vw, vh) = viewport_logical();
                    float_xy.set(Some((
                        (ox + p.x - mx).clamp(0.0, (vw - 48.0).max(0.0)),
                        (oy + p.y - my).clamp(0.0, (vh - 48.0).max(0.0)),
                    )));
                },
                div {
                    class: "mr-work",
                    // Reflect pop-out state so CSS can reclaim the vacated space:
                    // monitor out → the preview column shrinks to the transport;
                    // inspector out → the rail is gone and the stage centers.
                    class: if is_main && monitor_out() { "mr-mon-out" },
                    class: if is_main && inspector_out() { "mr-insp-out" },
                    // The whole preview column — stage, format bar AND transport —
                    // travels with the monitor: while it's popped into its own
                    // window (which renders all of this itself), the main window
                    // shows none of it, not a leftover transport console.
                    if is_monitor || (is_main && !monitor_out()) {
                    div { class: "mr-preview-col",
                        div { class: "mr-stage",
                            // Phase spine, split into two flanks that hug the phone —
                            // reclaims the dead space beside the 9:16 picture and
                            // frees the rail height the old 4×2 grid ate. Main view
                            // only; the popped-out monitor is stage-only.
                            if is_main {
                            div { class: "mr-wf-flank",
                                {wf_phases_a()}
                            }
                            }
                            div {
                                class: {
                                    let base = if matches!(drop_hover(), Some(Lane::V(_))) { "mr-phone mr-drop" } else { "mr-phone" };
                                    if mon_full() { format!("{base} mr-mon-full") } else { base.to_string() }
                                },
                                onmounted: move |evt| phone_el.set(Some(evt.data())),
                                oncontextmenu: move |evt| open_ctx(evt, Ctx::Monitor),
                                // Dropping on the picture means "show me this" —
                                // append to the end of the main track.
                                ondragover: move |evt| {
                                    evt.prevent_default();
                                    if !matches!(drop_hover(), Some(Lane::V(_))) {
                                        drop_hover.set(Some(Lane::V(2)));
                                    }
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
                                if mon_full() {
                                    button {
                                        class: "mr-mon-exit",
                                        onclick: move |evt| {
                                            evt.stop_propagation();
                                            mon_full.set(false);
                                        },
                                        "✕"
                                    }
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
                                        let (bcx, bcy) = (50.0 + xf.x * 100.0, 50.0 + xf.y * 100.0);
                                        // Rotation pads sit just beyond each corner, on the
                                        // line from the box centre out — grab past the scale
                                        // handle and turn, Canva-style.
                                        let rot_pads: Vec<(usize, f64, f64)> = corners
                                            .iter()
                                            .enumerate()
                                            .map(|(n, &(fx, fy))| {
                                                let (dx, dy) = (fx * 100.0 - bcx, fy * 100.0 - bcy);
                                                let len = (dx * dx + dy * dy).sqrt().max(0.001);
                                                let px = (fx * 100.0 + dx / len * 4.5).clamp(0.5, 99.5);
                                                let py = (fy * 100.0 + dy / len * 4.5).clamp(0.5, 99.5);
                                                (n, px, py)
                                            })
                                            .collect();
                                        rsx! {
                                            div {
                                                class: "mr-xf",
                                                // The box and handles take the pointer
                                                // themselves; the frame never does, so a
                                                // right-click still reaches the monitor.
                                                style: "pointer-events:none",
                                                // Mid-drag, a viewport-sized capture layer
                                                // tracks the pointer, so a fast pull past the
                                                // monitor's edge doesn't drop the drag.
                                                if xf_drag().is_some() {
                                                    div {
                                                        class: "mr-xf-capture",
                                                        onmousemove: move |evt| {
                                                            let Some((grab, from, start, rect)) = xf_drag() else { return };
                                                            // The mouseup happened off-window: end
                                                            // the drag on the first buttonless move.
                                                            if evt.held_buttons().is_empty() {
                                                                xf_drag.set(None);
                                                                return;
                                                            }
                                                            let p = evt.client_coordinates();
                                                            let snap = evt.modifiers().shift();
                                                            set_selected_xf(xf_apply(grab, start, from, (p.x, p.y), rect, snap));
                                                        },
                                                        onmouseup: move |_| xf_drag.set(None),
                                                    }
                                                }
                                                div {
                                                    class: "mr-xf-box",
                                                    style: "left:{bl}%;top:{bt}%;width:{bw}%;height:{bh}%;transform:rotate({xf.rotation}deg)",
                                                    onmousedown: move |evt| {
                                                        evt.stop_propagation();
                                                        let p = evt.client_coordinates();
                                                        begin_xf(XfGrab::Move, (p.x, p.y));
                                                    },
                                                }
                                                for (n, px, py) in rot_pads {
                                                    div {
                                                        key: "r{n}",
                                                        class: "mr-xf-rc",
                                                        title: "Drag to rotate \u{2014} hold Shift to snap to 15\u{b0}",
                                                        style: "left:{px}%;top:{py}%",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            let p = evt.client_coordinates();
                                                            begin_xf(XfGrab::Rotate, (p.x, p.y));
                                                        },
                                                    }
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
                            if is_main {
                            div { class: "mr-wf-flank",
                                {wf_phases_b()}
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
                                    if cfg!(target_os = "android") {
                                        button {
                                            class: "mor-btn",
                                            title: "Expand the monitor to fill the screen",
                                            onclick: move |_| { mon_full.set(true); },
                                            "⛶ Fullscreen"
                                        }
                                    } else {
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
                            // The popped-out monitor keeps the workspace phase
                            // buttons as a bar under the transport — same buttons
                            // as the main window's flanks, laid out in a row.
                            if is_monitor {
                                div { class: "mr-wf-flank mr-wf-bar",
                                    {wf_phases_a()}
                                    {wf_phases_b()}
                                }
                            }
                        }
                    }
                    } // if is_main (monitor column)

                    // Right rail: the phase spine sits atop the inspector, so the
                    // full-width bottom bar is gone and the monitor/timeline get
                    // that height back. The inspector still floats/closes below it.
                    // Hidden in the popped-out monitor window (stage only), and in
                    // the main window while the inspector lives in its own window —
                    // an empty rail would just hold a dead flex:1 column.
                    // (The popped window is a separate Editor with its own
                    // inspector_out = false, so its solo panel still renders.)
                    if !is_monitor && !(is_main && inspector_out()) {
                    div { class: "mr-rail",
                    if is_main && !insp_open() {
                        button {
                            class: "mr-insp-reopen",
                            title: "Show the inspector panel",
                            onclick: move |_| insp_open.set(true),
                            "Inspector ›"
                        }
                    }

                    // The popped-out case never gets here — the whole rail is
                    // hidden above while the inspector has its own window.
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
                        // CSS `resize: both` does the resizing; mirror the result
                        // back so the pinned geometry (and layout save) track it.
                        onresize: move |evt| {
                            if !insp_float() { return; }
                            if let Ok(sz) = evt.get_border_box_size() {
                                float_size.set(Some((sz.width, sz.height)));
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
                                begin_float(p.x, p.y);
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
                                if is_main && cfg!(not(target_os = "android")) {
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
                                }
                                if is_main {
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

                        if active_phase() == Phase::Add {
                            p { class: "mor-statusbar-muted mr-export-blurb",
                                "Main clips go on V1, cutaways on V2, and music or voiceover underneath."
                            }
                            div { class: "mr-phase-actions",
                                button { class: "mor-btn primary", disabled: exporting, onclick: move |_| import_clips(()), "＋ Add clips (V1)" }
                                button { class: "mor-btn", disabled: exporting, onclick: move |_| add_overlay(2), "⧉ Add b-roll (V2)" }
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
                                    for i in 0..mx.tracks.len() {
                                        div { key: "{i}", class: "mr-mixer-row",
                                            span { class: "mr-mixer-tag", "{mix_label(i)}" }
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
                                    "V1 is every clip's own sound; A-lanes are the beds. Solo one to audition it. Applies to preview and export alike."
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
                                        onchange: move |name: String| {
                                            push_undo("framing");
                                            clips.write()[i].framing = name;
                                            refresh_sel_monitor();
                                        },
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb", "{framing_hint(&c.framing)}" }
                                    div { class: "mr-field-row",
                                        div { class: "mr-field-grow",
                                            MorSelect {
                                                label: "Effect".to_string(),
                                                value: c.effect.clone(),
                                                options: effect_names.clone(),
                                                onchange: move |name: String| {
                                                    push_undo("fx-pick");
                                                    clips.write()[i].effect = name;
                                                    refresh_sel_monitor();
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
                                        h4 { class: "mr-fx-cat", "Audio" }
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
                                    h4 { class: "mr-fx-cat", "Arrange" }
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
                                    MorSelect {
                                        label: "Track".to_string(),
                                        value: format!("V{}", o.track.max(2)),
                                        options: overlay_tracks(v_lanes()).into_iter().map(|n| format!("V{}", n)).collect(),
                                        onchange: move |v: String| {
                                            push_undo("");
                                            let n = v.trim_start_matches('V').parse::<u8>().unwrap_or(2);
                                            if let Some(item) = overlays.write().get_mut(j) {
                                                item.track = n.max(2).min(1 + v_lanes());
                                            }
                                            seek_to(playhead());
                                        },
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
                                        label: "Framing (9:16)".to_string(),
                                        value: o.framing.clone(),
                                        options: FRAMINGS.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                                        onchange: move |name: String| {
                                            push_undo("framing");
                                            overlays.write()[j].framing = name;
                                            refresh_sel_monitor();
                                        },
                                    }
                                    p { class: "mor-statusbar-muted mr-export-blurb", "{framing_hint(&o.framing)}" }
                                    div { class: "mr-field-row",
                                        div { class: "mr-field-grow",
                                            MorSelect {
                                                label: "Effect".to_string(),
                                                value: o.effect.clone(),
                                                options: effect_names.clone(),
                                                onchange: move |name: String| {
                                                    push_undo("fx-pick");
                                                    overlays.write()[j].effect = name;
                                                    refresh_sel_monitor();
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
                                    MorSelect {
                                        label: "Track".to_string(),
                                        value: format!("T{}", t.track.max(1)),
                                        options: title_tracks(t_lanes()).into_iter().map(|n| format!("T{}", n)).collect(),
                                        onchange: move |v: String| {
                                            push_undo("");
                                            let n = v.trim_start_matches('T').parse::<u8>().unwrap_or(1);
                                            if let Some(item) = titles.write().get_mut(k) {
                                                item.track = n.max(1).min(t_lanes());
                                            }
                                            seek_to(playhead());
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
                                        // At least to the reel out point from this card's start
                                        // (and a 20s floor for short projects / freehand holds).
                                        max: (total - t.at).max(20.0).max(t.dur),
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
                                    // TikTok "text-to-speech": the card reads itself onto A2.
                                    button {
                                        class: "mor-btn",
                                        title: "Speak this card's text onto A2 at the card's start (espeak-ng)",
                                        onclick: move |_| {
                                            let (text, at) = match titles.read().get(k) {
                                                Some(t) => (t.text.clone(), t.at),
                                                None => return,
                                            };
                                            status.set("Reading card aloud…".to_string());
                                            spawn(async move {
                                                let path = match engine::tts(&text).await {
                                                    Ok(p) => p,
                                                    Err(e) => {
                                                        status.set(e);
                                                        return;
                                                    }
                                                };
                                                let path_s = path.display().to_string();
                                                match engine::probe(&path_s).await {
                                                    Ok((duration, true)) if duration > 0.05 => {
                                                        push_undo("");
                                                        let n = audios
                                                            .read()
                                                            .iter()
                                                            .filter(|a| a.name.starts_with("Speech"))
                                                            .count()
                                                            + 1;
                                                        audios.write().push(AudioItem {
                                                            path: path_s.clone(),
                                                            name: format!("Speech {n}"),
                                                            duration,
                                                            out_s: duration,
                                                            at,
                                                            fade_in: 0.02,
                                                            fade_out: 0.05,
                                                            lane: 2, // A2 — same bed as voiceover takes
                                                            ..Default::default()
                                                        });
                                                        // TikTok keeps the card up while it's read.
                                                        if let Some(item) = titles.write().get_mut(k) {
                                                            if item.dur < duration {
                                                                item.dur = duration;
                                                            }
                                                        }
                                                        status.set(format!(
                                                            "Speech on A2 at {} ({}).",
                                                            fmt_t(at),
                                                            fmt_clip_dur(duration)
                                                        ));
                                                        fill_audio_waves(path_s);
                                                    }
                                                    _ => status.set(
                                                        "Text-to-speech produced no audio.".to_string(),
                                                    ),
                                                }
                                            });
                                        },
                                        "Read aloud"
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
                                        value: a.lane_tag(),
                                        options: audio_tracks(a_lanes()).into_iter().map(|n| format!("A{n}")).collect(),
                                        onchange: move |v: String| {
                                            push_undo("");
                                            let n = v.trim_start_matches('A').parse::<u8>().unwrap_or(1);
                                            audios.write()[k].lane = n.max(1).min(a_lanes());
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
                            Some(Sel::Adjust(k)) if k < adjustments.read().len() => {
                                let a = adjustments.read()[k].clone();
                                rsx! {
                                    p { class: "mor-statusbar-muted mr-export-blurb",
                                        "Adjustment layer — its grade and effect apply to every clip and cutaway beneath it, only while it covers the timeline. Trim or drag it on the FX lane."
                                    }
                                    div { class: "mr-field-row",
                                        span { class: "mor-statusbar-muted", "{fmt_t(a.at)} → {fmt_t(a.at + a.dur)}  ({fmt_clip_dur(a.dur)})" }
                                    }
                                    div { class: "mr-toolbar",
                                        button {
                                            class: "mor-btn",
                                            title: "Start this adjustment at the playhead",
                                            onclick: move |_| {
                                                push_undo("");
                                                adjustments.write()[k].at = playhead().max(0.0);
                                                seek_to(playhead());
                                            },
                                            "⊙ Start at playhead"
                                        }
                                        button {
                                            class: "mor-btn",
                                            title: "Stretch this adjustment across the whole reel",
                                            onclick: move |_| {
                                                push_undo("");
                                                let end = total_of();
                                                {
                                                    let mut aj = adjustments.write();
                                                    aj[k].at = 0.0;
                                                    aj[k].dur = end.max(0.1);
                                                }
                                                seek_to(playhead());
                                            },
                                            "⇔ Whole reel"
                                        }
                                    }
                                    h4 { class: "mr-fx-cat", "Effect" }
                                    div { class: "mr-field-row",
                                        div { class: "mr-field-grow",
                                            MorSelect {
                                                label: "Effect".to_string(),
                                                value: a.effect.clone(),
                                                options: effect_names.clone(),
                                                onchange: move |name: String| apply_effect(name),
                                            }
                                        }
                                    }
                                    if a.effect != "None" {
                                        Slider {
                                            label: Some("Strength"),
                                            min: 0.0,
                                            max: 1.0,
                                            step: 0.01,
                                            precision: 2,
                                            value: a.effect_amount,
                                            oninput: Some(EventHandler::new(move |v: f64| set_effect_amount(v))),
                                        }
                                    }
                                    {grade_panel()}
                                    div { class: "mr-toolbar",
                                        button { class: "mor-btn mr-danger", onclick: move |_| delete_sel(()), "✕ Remove adjustment" }
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
                    } // mr-rail
                    } // if !is_monitor
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
                                // Free-span like a title — no source to trim against.
                                Sel::Adjust(k) => {
                                    if let Some(a) = adjustments.write().get_mut(k) {
                                        let (at, dur) = title_edge_resize(at0, a0, left, dt);
                                        a.at = at;
                                        a.dur = dur;
                                    }
                                }
                                Sel::Over(j) => {
                                    if let Some(o) = overlays.write().get_mut(j) {
                                        if engine::is_still(&o.path) {
                                            let (at, inn, out) =
                                                still_edge_resize(at0, a0, b0, o.speed, left, dt);
                                            o.at = at;
                                            o.in_s = inn;
                                            o.out_s = out;
                                        } else {
                                            let (at, inn, out) = media_edge_resize(
                                                at0, a0, b0, src_dur0, speed0, left, dt, true,
                                            );
                                            o.at = at;
                                            o.in_s = inn;
                                            o.out_s = out;
                                        }
                                    }
                                }
                                Sel::Main(i) => {
                                    let old = spans();
                                    if let Some(c) = clips.write().get_mut(i) {
                                        // A still loops, so its Out isn't bound by a source
                                        // length — let the right edge grow past the nominal
                                        // 60s. (Left still trims the head, the ripple norm.)
                                        let src = if engine::is_still(&c.path) {
                                            f64::MAX
                                        } else {
                                            src_dur0
                                        };
                                        let (_, inn, out) = media_edge_resize(
                                            0.0, a0, b0, src, speed0, left, dt, false,
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
                        // Text card dragged up/down between T-lanes restacks it —
                        // higher track composites on top. Lane pitch is 36px
                        // (.mr-lane: 30px height + 6px margin, app.css), fixed
                        // regardless of the clip-height slider. Runs alongside the
                        // horizontal shift below, so a diagonal drag does both.
                        if let Some((k, start_tr, start_y)) = title_lane_drag() {
                            let rows = ((start_y - p.y) / 36.0).round() as i32; // up = higher track
                            let nt = (start_tr as i32 + rows).clamp(1, t_lanes() as i32) as u8;
                            if titles.read().get(k).is_some_and(|t| t.track.max(1) != nt) {
                                push_undo("drag-lane"); // same tag as shift_lane: one undo per drag
                                if let Some(t) = titles.write().get_mut(k) {
                                    t.track = nt;
                                }
                                drag_moved.set(true); // a lane hop is a drag, swallow the click
                            }
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
                        title_lane_drag.set(None);
                        fade_drag.set(None);
                        vol_drag.set(None);
                        len_drag.set(None);
                        pan.set(None);
                        scrubbing.set(false);
                    },
                    onmouseleave: move |_| {
                        drag.set(None);
                        title_lane_drag.set(None);
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
                        // Tapping the empty timeline opens the picker — on touch
                        // there is no drag-and-drop, so the drop zone must also
                        // be the button (and the copy can't promise drops).
                        span {
                            class: "mor-statusbar-muted mr-timeline-hint",
                            style: "cursor: pointer;",
                            onclick: move |_| import_clips(()),
                            if cfg!(target_os = "android") {
                                "Tap here to add clips — your story builds left to right"
                            } else {
                                "Drop media here, or Add clips (Ctrl+O) — your story builds left to right"
                            }
                        }
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
                                    // In/out playback range — shaded band with bracket edges.
                                    if mark_in().is_some() || mark_out().is_some() {
                                        {
                                            let s = mark_in().unwrap_or(0.0).clamp(0.0, total);
                                            let e = mark_out().unwrap_or(total).clamp(0.0, total);
                                            let (s, e) = (s.min(e), s.max(e));
                                            rsx! {
                                                div {
                                                    class: "mr-range",
                                                    style: "left: {s * scale}px; width: {(e - s) * scale}px",
                                                    title: "Play range {fmt_t(s)}–{fmt_t(e)} — Shift+X clears",
                                                }
                                            }
                                        }
                                    }
                                    // Text tracks top-to-bottom: highest track first (T2 over T1).
                                    for tr in title_tracks(t_lanes()).into_iter().rev() {
                                        {
                                        let tag = format!("T{}", tr);
                                        let is_top = tr == t_lanes();
                                        rsx! {
                                        div {
                                            key: "t-lane-{tr}",
                                            class: if show_titles() { "mr-lane mr-lane-t" } else { "mr-lane mr-lane-t mr-lane-off" },
                                            span {
                                                class: if show_titles() { "mr-lane-tag title mr-lane-toggle" } else { "mr-lane-tag title mr-lane-toggle off" },
                                                title: if is_top {
                                                    if show_titles() { "Titles shown — click to hide in the monitor" } else { "Titles hidden in the monitor — click to show" }
                                                } else {
                                                    "Text track — higher number stacks on top"
                                                },
                                                onclick: move |evt| {
                                                    evt.stop_propagation();
                                                    if is_top {
                                                        let on = !show_titles();
                                                        show_titles.set(on);
                                                        seek_to(playhead());
                                                    }
                                                },
                                                if show_titles() { "{tag}" } else if is_top { "{tag}⃠" } else { "{tag}" }
                                            }
                                            for (k, t) in titles().into_iter().enumerate().filter(|(_, t)| t.track.max(1) == tr) {
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
                                                            let p = evt.client_coordinates();
                                                            drag.set(Some((Sel::Title(k), p.x, 0.0, 0.0)));
                                                            let tr = titles.read().get(k).map_or(1, |t| t.track.max(1));
                                                            title_lane_drag.set(Some((k, tr, p.y)));
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
                                        }
                                        }
                                    }
                                    // FX lane: adjustment layers grading the picture below over
                                    // their span. Only shown once one exists (Insert ▸ Add
                                    // adjustment layer) so it doesn't clutter a plain reel.
                                    if !adjustments.read().is_empty() {
                                        div { class: "mr-lane mr-lane-fx",
                                            span { class: "mr-lane-tag fx", title: "Adjustment layers — grade/effect over everything below", "FX" }
                                            for (k, a) in adjustments().into_iter().enumerate() {
                                                div {
                                                    key: "adj-{k}",
                                                    class: item_class(
                                                        "mr-lane-item fx",
                                                        selected() == Some(Sel::Adjust(k)),
                                                        marked().contains(&Sel::Adjust(k)),
                                                        !a.enabled,
                                                        false,
                                                    ),
                                                    style: "left: {a.at * scale}px; width: {a.dur * scale}px",
                                                    onmousedown: move |evt| {
                                                        if evt.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary) && !evt.modifiers().ctrl() {
                                                            selected.set(Some(Sel::Adjust(k)));
                                                            drag.set(Some((Sel::Adjust(k), evt.client_coordinates().x, 0.0, 0.0)));
                                                        }
                                                    },
                                                    onclick: move |evt| {
                                                        if drag_moved() {
                                                            drag_moved.set(false);
                                                            return;
                                                        }
                                                        if evt.modifiers().ctrl() {
                                                            toggle_mark(Sel::Adjust(k));
                                                            return;
                                                        }
                                                        let at = adjustments.read()[k].at;
                                                        seek_to(at);
                                                        selected.set(Some(Sel::Adjust(k)));
                                                    },
                                                    div {
                                                        class: "mr-len-grip in",
                                                        title: "Drag to change when this adjustment starts",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("ajlen{k}"));
                                                            let (at, d) = adjustments.read().get(k).map_or((0.0, 3.0), |a| (a.at, a.dur));
                                                            len_drag.set(Some((Sel::Adjust(k), true, evt.client_coordinates().x, at, d, 0.0, 1.0, 0.0)));
                                                        },
                                                    }
                                                    div {
                                                        class: "mr-len-grip out",
                                                        title: "Drag to change how long this adjustment lasts",
                                                        onmousedown: move |evt| {
                                                            evt.stop_propagation();
                                                            push_undo(&format!("ajlen{k}"));
                                                            let (at, d) = adjustments.read().get(k).map_or((0.0, 3.0), |a| (a.at, a.dur));
                                                            len_drag.set(Some((Sel::Adjust(k), false, evt.client_coordinates().x, at, d, 0.0, 1.0, 0.0)));
                                                        },
                                                    }
                                                    span { class: "mr-clip-dur", "{fmt_clip_dur(a.dur)}" }
                                                    if a.group != 0 {
                                                        span { class: "mr-group-dot", style: "background: hsl({(a.group * 67) % 360}, 70%, 60%)" }
                                                    }
                                                    "◈ {a.label()}"
                                                }
                                            }
                                        }
                                    }
                                    // Overlay tracks Vn..V2 (highest on top). Drop targets per track.
                                    for tr in overlay_tracks(v_lanes()).into_iter().rev() {
                                        {
                                        let lane = Lane::V(tr);
                                        let tag = format!("V{}", tr);
                                        let is_bottom = tr == 2;
                                        rsx! {
                                        div {
                                            key: "v-lane-{tr}",
                                            class: if drop_hover() == Some(lane) {
                                                "mr-lane mr-lane-v mr-drop"
                                            } else if show_overlays() {
                                                "mr-lane mr-lane-v"
                                            } else {
                                                "mr-lane mr-lane-v mr-lane-off"
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
                                            span {
                                                class: if show_overlays() { "mr-lane-tag mr-lane-toggle" } else { "mr-lane-tag mr-lane-toggle off" },
                                                title: if is_bottom {
                                                    if show_overlays() {
                                                        "Cutaways shown — click to hide in the monitor"
                                                    } else {
                                                        "Cutaways hidden in the monitor — click to show"
                                                    }
                                                } else {
                                                    "Overlay track — higher number stacks on top of lower"
                                                },
                                                onclick: move |evt| {
                                                    evt.stop_propagation();
                                                    if is_bottom {
                                                        let on = !show_overlays();
                                                        show_overlays.set(on);
                                                        seek_to(playhead());
                                                    }
                                                },
                                                if show_overlays() { "{tag}" } else if is_bottom { "{tag}⃠" } else { "{tag}" }
                                            }
                                            for (j, o) in overlays().into_iter().enumerate().filter(|(_, o)| o.track.max(2) == tr) {
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
                                    // A1..An under V1 — each bus is its own lane (music / VO / SFX…).
                                    for bus in audio_tracks(a_lanes()) {
                                        {
                                        let lane = Lane::A(bus);
                                        let tag = format!("A{}", bus);
                                        rsx! {
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
                                            // Live voiceover ghost: a red tile on A2 that grows
                                            // as you record, so the take shows up in place instead
                                            // of popping in when you stop. The real AudioItem lands
                                            // on stop; this is display-only (no pointer events).
                                            if bus == 2 {
                                                if let Some((_, vo_at)) = vo_session() {
                                                    div {
                                                        class: "mr-lane-item audio",
                                                        style: "left: {vo_at * scale}px; width: {(vo_len() * scale).max(2.0)}px; background: rgba(233,30,60,0.35); border-color: rgba(233,30,60,0.9); pointer-events: none;",
                                                        span { class: "mr-clip-dur", "● {fmt_clip_dur(vo_len())}" }
                                                    }
                                                }
                                            }
                                            for (k, a) in audios().into_iter().enumerate().filter(|(_, a)| a.lane.max(1) == bus) {
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
                // Workflow spine moved into the right rail, above the inspector
                // (see mr-rail) — the full-width bottom bar is gone.
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
                                on_action: move |_| add_overlay(2),
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
                                    on_action: move |_| set_edge_here(false),
                                }
                                CtxItem {
                                    label: "Set out point at playhead".to_string(),
                                    shortcut: Some("O".to_string()),
                                    on_action: move |_| set_edge_here(true),
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
                                {disable_solo_rows(
                                    clips.read().get(i).is_some_and(|c| c.enabled),
                                    clips.read().get(i).is_some_and(|c| c.solo),
                                )}
                                CtxItem {
                                    label: "Detach audio to A1".to_string(),
                                    shortcut: Some("Ctrl+U".to_string()),
                                    disabled: !clips.read().get(i).is_some_and(|c| c.has_audio),
                                    on_action: move |_| detach_audio(()),
                                }
                                MenuSeparator {}
                                {copy_paste_rows()}
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
                            let tr = overlays.read().get(j).map(|o| o.track.max(2)).unwrap_or(2);
                            let tag = format!("V{tr}");
                            let max_tr = 1 + v_lanes();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag", "{tag}" }
                                    span { class: "mr-ctx-name", "{name}" }
                                }
                                CtxItem {
                                    label: "Split at playhead".to_string(),
                                    shortcut: Some("S".to_string()),
                                    on_action: move |_| split_at_playhead(()),
                                }
                                {move_track_rows(tr, 2, max_tr, EventHandler::new(move |t| {
                                    push_undo("");
                                    if let Some(o) = overlays.write().get_mut(j) {
                                        o.track = t;
                                    }
                                    seek_to(playhead());
                                }))}
                                {copy_paste_rows()}
                                CtxItem {
                                    label: "Effects palette…".to_string(),
                                    on_action: move |_| {
                                        insp_open.set(true);
                                        show_effects.set(true);
                                    },
                                }
                                {disable_solo_rows(
                                    overlays.read().get(j).is_some_and(|o| o.enabled),
                                    overlays.read().get(j).is_some_and(|o| o.solo),
                                )}
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
                            let bus = audios.read().get(k).map(|a| a.lane.max(1)).unwrap_or(1);
                            let tag = format!("A{bus}");
                            let max_a = a_lanes();
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
                                {copy_paste_rows()}
                                CtxItem {
                                    label: if audios.read().get(k).is_some_and(|a| a.volume <= 0.001) {
                                        "Unmute".to_string()
                                    } else {
                                        "Mute".to_string()
                                    },
                                    shortcut: Some("Ctrl+Shift+M".to_string()),
                                    on_action: move |_| mute_sel(()),
                                }
                                {disable_solo_rows(
                                    audios.read().get(k).is_some_and(|a| a.enabled),
                                    audios.read().get(k).is_some_and(|a| a.solo),
                                )}
                                {move_track_rows(bus, 1, max_a, EventHandler::new(move |l| {
                                    push_undo("");
                                    if let Some(a) = audios.write().get_mut(k) {
                                        a.lane = l;
                                    }
                                }))}
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
                            let tr = titles.read().get(k).map(|t| t.track.max(1)).unwrap_or(1);
                            let tag = format!("T{tr}");
                            let max_t = t_lanes();
                            rsx! {
                                div { class: "mr-ctx-head",
                                    span { class: "mr-ctx-tag title", "{tag}" }
                                    span { class: "mr-ctx-name", "{text}" }
                                }
                                CtxItem {
                                    label: "Add another text".to_string(),
                                    shortcut: Some("Ctrl+T".to_string()),
                                    disabled: no_clips || exporting,
                                    on_action: move |_| add_title(()),
                                }
                                {move_track_rows(tr, 1, max_t, EventHandler::new(move |t| {
                                    push_undo("");
                                    if let Some(item) = titles.write().get_mut(k) {
                                        item.track = t;
                                    }
                                    seek_to(playhead());
                                }))}
                                CtxItem {
                                    label: "Extend to end of reel".to_string(),
                                    disabled: no_clips,
                                    on_action: move |_| {
                                        let Some(t) = titles.read().get(k).cloned() else { return };
                                        let end = total_of();
                                        let dur = title_dur_to_end(t.at, end);
                                        if (dur - t.dur).abs() < 1e-6 {
                                            status.set("Text already runs to the end of the reel.".to_string());
                                            return;
                                        }
                                        push_undo("");
                                        if let Some(item) = titles.write().get_mut(k) {
                                            item.dur = dur;
                                        }
                                        status.set(format!(
                                            "Text extended to end of reel ({})",
                                            fmt_clip_dur(dur)
                                        ));
                                    },
                                }
                                MenuSeparator {}
                                {copy_paste_rows()}
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
                            add_overlay(2);
                        },
                        span { class: "mr-add-tag", "V2" }
                        strong { "Overlay" }
                        span { class: "mor-statusbar-muted", "B-roll / PiP over V1 (add V3+ for stacks)" }
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
                                        {tile_grid(
                                            engine::TRANSITIONS.iter()
                                                .map(|(l, _)| (l.to_string(), l.to_string(), current == *l, None))
                                                .collect(),
                                            EventHandler::new(move |label: String| {
                                                push_undo("");
                                                let old = spans();
                                                clips.write()[i].transition = label.clone();
                                                if label != "None" && clips.read()[i].trans_dur < 0.1 {
                                                    clips.write()[i].trans_dur = 0.5;
                                                }
                                                ride(old, &|k| Some(start_of(k)));
                                                selected.set(Some(Sel::Main(i)));
                                                seek_to(playhead().min(total_of()));
                                            }),
                                        )}
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
                                        {tile_grid(
                                            engine::TITLE_ANIMS.iter()
                                                .map(|n| (n.to_string(), n.to_string(), current == *n, None))
                                                .collect(),
                                            EventHandler::new(move |name: String| {
                                                push_undo("");
                                                titles.write()[k].anim = name;
                                            }),
                                        )}
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
                                        {tile_grid(
                                            engine::AUDIO_TREATS.iter()
                                                .map(|n| (n.to_string(), n.to_string(), current == *n, None))
                                                .collect(),
                                            EventHandler::new(move |name: String| {
                                                push_undo("");
                                                audios.write()[k].treat = name;
                                            }),
                                        )}
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
                                    {tile_grid(
                                        engine::Bg::ALL.iter()
                                            .map(|b| (b.label().to_string(), b.label().to_string(),
                                                current == Some(*b), Some(format!("background: {}", b.color()))))
                                            .collect(),
                                        EventHandler::new(move |label: String| {
                                            let Some(b) = engine::Bg::ALL.iter().copied().find(|b| b.label() == label) else { return };
                                            push_undo("bg");
                                            for c in clips.write().iter_mut() { c.transform.bg = b; }
                                            seek_to(playhead());
                                        }),
                                    )}
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
                                    // "None" clears any key; then every keyer.
                                    {tile_grid(
                                        std::iter::once(("None".to_string(), current == "None"))
                                            .chain(all_effects().into_iter().filter(|(c, _, _)| c == "Key").map(|(_, n, _)| { let cur = current == n; (n, cur) }))
                                            .map(|(n, cur)| (n.clone(), n, cur, None))
                                            .collect(),
                                        EventHandler::new(move |name: String| apply_effect(name)),
                                    )}
                                    if let Some(j) = over_idx {
                                        h4 { class: "mr-fx-cat", "Blend (V2 over V1)" }
                                        {tile_grid(
                                            BLEND_MODES.iter()
                                                .map(|(l, m)| (l.to_string(), m.to_string(), blend == *m, None))
                                                .collect(),
                                            EventHandler::new(move |mode: String| {
                                                push_undo("blend");
                                                if j < overlays.read().len() { overlays.write()[j].blend = mode; }
                                                seek_to(playhead());
                                            }),
                                        )}
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
                    // ponytail: folder pick is a no-op on Android — GitHub fetch is enough.
                    if !cfg!(target_os = "android") {
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
            open: show_giphy,
            title: "Add from Giphy".to_string(),
            div { class: "mr-hub",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Search GIPHY, then click a result to drop it on V2 at the playhead. "
                    "Stickers come in with transparency; GIFs fill the frame. Needs a free "
                    "GIPHY_API_KEY (developers.giphy.com)."
                }
                div { class: "mr-toolbar",
                    input {
                        class: "mor-input",
                        r#type: "text",
                        placeholder: "Search GIFs…",
                        value: "{giphy_query}",
                        oninput: move |e| giphy_query.set(e.value()),
                        onkeydown: move |e| if e.key() == Key::Enter { run_giphy_search(()); },
                    }
                    button {
                        class: if giphy_stickers() { "mor-btn primary" } else { "mor-btn" },
                        onclick: move |_| giphy_stickers.set(!giphy_stickers()),
                        "Stickers"
                    }
                    button {
                        class: "mor-btn primary",
                        disabled: giphy_busy(),
                        onclick: move |_| run_giphy_search(()),
                        if giphy_busy() { "Searching…" } else { "Search" }
                    }
                }
                div { class: "mr-giphy-grid",
                    for g in giphy_results() {
                        img {
                            key: "{g.id}",
                            class: "mr-giphy-thumb",
                            src: "{g.preview}",
                            title: "{g.title}",
                            onclick: move |_| {
                                let g = g.clone();
                                let at = playhead();
                                spawn(async move {
                                    status.set(format!("Fetching “{}”…", g.title));
                                    match giphy::download(&g).await {
                                        Ok(path) => add_overlay_path(path.display().to_string(), at, 2),
                                        Err(e) => status.set(format!("GIPHY download: {e}")),
                                    }
                                });
                            },
                        }
                    }
                }
                div { class: "mr-toolbar",
                    button { class: "mor-btn", onclick: move |_| show_giphy.set(false), "Done" }
                }
            }
        }
        Modal {
            open: show_emoji,
            title: "Add emoji".to_string(),
            div { class: "mr-hub",
                p { class: "mor-statusbar-muted mr-export-blurb",
                    "Click a favorite or paste any emoji — dropped on V2 at the playhead as a transparent sticker."
                }
                div { class: "mr-emoji-grid",
                    for e in emoji::PICKS {
                        button {
                            key: "{e}",
                            class: "mr-emoji-cell",
                            onclick: move |_| add_emoji(e.to_string()),
                            "{e}"
                        }
                    }
                }
                div { class: "mr-toolbar",
                    input {
                        class: "mor-input",
                        r#type: "text",
                        placeholder: "…or type/paste any emoji",
                        value: "{emoji_input}",
                        oninput: move |e| emoji_input.set(e.value()),
                        onkeydown: move |e| if e.key() == Key::Enter { add_emoji(emoji_input()); },
                    }
                    button { class: "mor-btn primary", onclick: move |_| add_emoji(emoji_input()), "Add" }
                    button { class: "mor-btn", onclick: move |_| show_emoji.set(false), "Done" }
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
                    ("Shift+I / Shift+O / Shift+X", "Mark in / out playback range / clear it"),
                    ("V", "Record / stop voiceover onto A2"),
                    ("Ctrl+P", "Full preview with audio in mpv/ffplay"),
                    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                    ("Ctrl+C / Ctrl+V", "Copy / paste timeline item at the playhead"),
                    ("Ctrl+Shift+V", "Paste look (transform + grade + effect) onto the selection"),
                    ("Ctrl+Shift+X", "Reset look to default"),
                    ("I / O", "Set in / out point at playhead"),
                    ("S", "Split at playhead"),
                    ("F", "Add freeze frame at the playhead"),
                    ("Ctrl+R", "Instant replay (last 1.5s at half speed)"),
                    ("Ctrl+D", "Add cross dissolve into the clip under the playhead"),
                    ("Ctrl+J", "Join adjacent same-source clips"),
                    ("Ctrl+Shift+M", "Mute / unmute selected clip or bed"),
                    ("Delete / Backspace", "Ripple delete selection"),
                    ("← / →", "Nudge playhead 0.1s (Shift = 1s)"),
                    (", / .", "Step playhead one frame (1/30 s)"),
                    ("Shift+, / Shift+.", "Nudge selected cutaway/title/bed/FX one frame (Ctrl = 10)"),
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
        // FCP-style Share window: preview thumb + attributes on top, a file-
        // information strip underneath, Cancel/Next at the bottom.
        Modal {
            open: show_export,
            title: "Share".to_string(),
            div { class: "mr-share-dialog",
                div { class: "mr-share-top",
                    if !preview().is_empty() {
                        img { class: "mr-share-thumb", src: "{preview}" }
                    }
                    div { class: "mr-share-fields",
                        MorTextInput {
                            label: "Name".to_string(),
                            value: export_name(),
                            onchange: move |v: String| export_name.set(v),
                        }
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
                    }
                }
                p { class: "mr-share-info",
                    "{export_opts().width} × {export_opts().height} · 30 fps · {fmt_t(total)}"
                    match export_opts().format {
                        engine::Format::Mp4 => " · AAC 192 kbps",
                        engine::Format::WebM => " · Opus 128 kbps",
                        engine::Format::Gif => " · silent — GIF carries no audio",
                    }
                    " · .{export_opts().format.ext()} · ~{fmt_bytes(export_opts().est_bytes(total))} est."
                }
                if let Some(warn) = over_limits(total, &settings().platform) {
                    p { class: "mr-share-warn", "⚠ {warn}" }
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
                        "Next: choose file…"
                    }
                }
            }
        }
        // Progress toast — floats over the corner while a long render runs,
        // with a live bar and a cancel that actually kills the job.
        if let Some(p) = export_progress() {
            div { class: "mr-export-toast",
                div { class: "mr-export-toast-row",
                    span { class: "mr-export-toast-label", "{export_label}" }
                    span { class: "mor-statusbar-muted", "{(p * 100.0) as u32}%" }
                }
                div { class: "mr-progress", div { style: "width: {p * 100.0:.1}%" } }
                button {
                    class: "mor-btn mr-export-toast-cancel",
                    onclick: move |_| engine::cancel_render(),
                    "Cancel"
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

const APP_CSS: &str = include_str!("app.css");
/// CapCut-style phone chrome, loaded after [`APP_CSS`] on Android only.
const ANDROID_CSS: &str = include_str!("app.android.css");

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
    fn title_dur_to_end_reaches_reel_out_point() {
        // Starts mid-reel → duration fills to the out point.
        assert_eq!(title_dur_to_end(2.0, 10.0), 8.0);
        // Whole reel from zero.
        assert_eq!(title_dur_to_end(0.0, 12.5), 12.5);
        // Past the end still gets the minimum hold (same floor as edge resize).
        assert_eq!(title_dur_to_end(15.0, 10.0), 0.3);
    }

    #[test]
    fn multi_track_ranges_and_drop_routing() {
        assert_eq!(overlay_tracks(2), vec![2, 3]);
        assert_eq!(title_tracks(3), vec![1, 2, 3]);
        assert_eq!(audio_tracks(4), vec![1, 2, 3, 4]);
        // Drop routing accepts any V/A lane; photos never go to audio.
        assert_eq!(route_drop(Kind::Video, Lane::V(3)), Ok((Lane::V(3), None)));
        assert_eq!(route_drop(Kind::Still, Lane::V(4)), Ok((Lane::V(4), None)));
        assert_eq!(route_drop(Kind::Audio, Lane::A(3)), Ok((Lane::A(3), None)));
        assert_eq!(
            route_drop(Kind::Audio, Lane::V(3)),
            Ok((Lane::A(1), Some("audio goes to A1")))
        );
        assert!(route_drop(Kind::Still, Lane::A(3)).is_err());
    }

    #[test]
    fn normalize_lanes_grows_counts_from_items() {
        let mut s = Snapshot {
            overlays: vec![OverlayItem {
                path: "x.mp4".into(),
                name: "x".into(),
                duration: 1.0,
                in_s: 0.0,
                out_s: 1.0,
                at: 0.0,
                effect: "None".into(),
                effect_amount: 1.0,
                framing: "Crop".into(),
                transform: Default::default(),
                grade: Default::default(),
                speed: 1.0,
                reverse: false,
                blend: String::new(),
                track: 5, // V5 → need 4 overlay lanes (V2..V5)
                proxy: String::new(),
                group: 0,
                enabled: true,
                solo: false,
            }],
            titles: vec![TitleItem {
                track: 3,
                ..base_title()
            }],
            audios: vec![AudioItem {
                lane: 4,
                ..Default::default()
            }],
            v_lanes: 1,
            t_lanes: 1,
            a_lanes: 1,
            ..Default::default()
        };
        normalize_lanes(&mut s);
        assert_eq!(s.v_lanes, 4); // tracks 2..=5
        assert_eq!(s.t_lanes, 3);
        assert_eq!(s.a_lanes, 4);
        assert!(s.mixer.tracks.len() >= 5); // V1 + A1..A4
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
    fn still_edge_resize_stretches_either_side() {
        // A still (in=0, held 5s) on V2. Right edge grows the hold, uncapped —
        // no 60s source ceiling, unlike media_edge_resize.
        let (at, inn, out) = still_edge_resize(5.0, 0.0, 5.0, 1.0, false, 100.0);
        assert_eq!((at, inn, out), (5.0, 0.0, 105.0));
        // Left edge extends leftward: `at` moves earlier, right edge stays at 10,
        // in stays 0 (the old media path walled here because in couldn't go < 0).
        let (at, inn, out) = still_edge_resize(5.0, 0.0, 5.0, 1.0, true, -3.0);
        assert_eq!((at, inn, out), (2.0, 0.0, 8.0));
        // Speed scales the span: out is in source seconds (2× → 8s span = 16s out).
        let (_, inn, out) = still_edge_resize(0.0, 0.0, 10.0, 2.0, false, 3.0);
        assert_eq!((inn, out), (0.0, 16.0));
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
        let out = std::process::Command::new(engine::ffmpeg_bin())
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
    fn keyframe_velocity_cycles_and_reports() {
        use keyframe::Interp::*;
        let mut at = engine::AnimatedTransform::default();
        // Arm a scale curve with two keys, the second (at t=1) defaulting to Smooth.
        xf_toggle_key(&mut at, "Scale", 0.0); // first diamond → Curve
        xf_write(&mut at, "Scale", 1.5, 1.0); // animated now → keys t=1 at Smooth
        assert_eq!(xf_key_interp(&at, "Scale", 1.0), Some(Smooth));
        xf_cycle_interp(&mut at, "Scale", 1.0);
        assert_eq!(xf_key_interp(&at, "Scale", 1.0), Some(Linear));
        xf_cycle_interp(&mut at, "Scale", 1.0);
        assert_eq!(xf_key_interp(&at, "Scale", 1.0), Some(Hold));
        xf_cycle_interp(&mut at, "Scale", 1.0);
        assert_eq!(xf_key_interp(&at, "Scale", 1.0), Some(Smooth)); // wraps
        // Cycling changes only the ease, not the value.
        assert!((at.scale.sample(1.0) - 1.5).abs() < 1e-9);
        // No key at this time → nothing to report or cycle.
        assert_eq!(xf_key_interp(&at, "Scale", 0.5), None);
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
        assert_eq!(route_drop(Kind::Video, Lane::V(2)), Ok((Lane::V(2), None)));
        assert_eq!(route_drop(Kind::Audio, Lane::A(1)), Ok((Lane::A(1), None)));
        assert_eq!(route_drop(Kind::Audio, Lane::A(2)), Ok((Lane::A(2), None)));

        // Sound aimed at a video lane still goes to A1, and says so.
        assert_eq!(route_drop(Kind::Audio, Lane::V1), Ok((Lane::A(1), Some("audio goes to A1"))));
        assert_eq!(route_drop(Kind::Audio, Lane::V(2)), Ok((Lane::A(1), Some("audio goes to A1"))));

        // A video on an audio lane contributes its soundtrack rather than being refused.
        assert_eq!(
            route_drop(Kind::Video, Lane::A(1)),
            Ok((Lane::A(1), Some("using its soundtrack")))
        );
        assert_eq!(
            route_drop(Kind::Video, Lane::A(2)),
            Ok((Lane::A(2), Some("using its soundtrack")))
        );
        // A photo genuinely has nothing to give an audio track.
        assert!(route_drop(Kind::Still, Lane::A(1)).is_err());
        assert!(route_drop(Kind::Still, Lane::A(2)).is_err());
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
        let empty = Snapshot::default();
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
            titles: vec![title],
            ..Default::default()
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
        let snap = Snapshot { clips: vec![c], markers: vec![2.5], ..Default::default() };

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
    fn handles_measure_from_the_box_centre_not_the_frame() {
        // A sticker parked in the top-left quarter: its centre on the 270x480
        // monitor is (67.5, 120), nowhere near the frame centre.
        let start = engine::Transform { x: -0.25, y: -0.25, ..Default::default() };
        // Doubling the distance from the BOX centre doubles the size.
        let t = xf_apply(XfGrab::Scale, start, (117.5, 120.0), (167.5, 120.0), RECT, false);
        assert!((t.scale - 2.0).abs() < 1e-9, "scale = {}", t.scale);
        // From straight above the box to its right is a quarter turn.
        let t = xf_apply(XfGrab::Rotate, start, (67.5, 20.0), (167.5, 120.0), RECT, false);
        assert!((t.rotation - 90.0).abs() < 1e-6, "rotation = {}", t.rotation);
    }

    #[test]
    fn a_tilted_side_handle_stretches_along_its_own_axis() {
        // Rotated a quarter turn, the box's width axis points down the screen.
        let start = engine::Transform { rotation: 90.0, ..Default::default() };
        let t = xf_apply(XfGrab::StretchX, start, (135.0, 340.0), (135.0, 440.0), RECT, false);
        assert!((t.scale_x - 2.0).abs() < 1e-9, "scale_x = {}", t.scale_x);
        // A screen-horizontal drag is perpendicular to that axis — no stretch.
        let t = xf_apply(XfGrab::StretchX, start, (135.0, 340.0), (235.0, 340.0), RECT, false);
        assert!((t.scale_x - 1.0).abs() < 1e-9, "scale_x = {}", t.scale_x);
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
