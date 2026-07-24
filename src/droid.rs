// SPDX-License-Identifier: GPL-3.0-or-later
// droid.rs — Android glue: media permissions over JNI and the in-app file
// picker that stands in for the desktop rfd dialogs (rfd has no Android
// backend). `mod rfd` in main.rs awaits `pick()`; the `AndroidPicker` overlay
// mounted at the App root answers it through a oneshot channel.

use dioxus::prelude::*;
use jni::objects::{JObject, JString, JValue};
use std::path::{Path, PathBuf};

const ROOT: &str = "/storage/emulated/0";

pub struct PickReq {
    title: String,
    /// lowercase, no dot; empty = every file.
    exts: Vec<String>,
    multiple: bool,
    tx: tokio::sync::oneshot::Sender<Vec<PathBuf>>,
}

pub static PICK: GlobalSignal<Option<PickReq>> = Signal::global(|| None);

/// Ask the user for file(s) via the overlay. Empty result = cancelled.
pub async fn pick(title: String, mut exts: Vec<String>, multiple: bool) -> Vec<PathBuf> {
    request_media_permissions();
    // "All files" arrives as a "*" filter — drop it; empty exts means all.
    exts.retain(|e| e != "*");
    let (tx, rx) = tokio::sync::oneshot::channel();
    *PICK.write() = Some(PickReq { title, exts, multiple, tx });
    rx.await.unwrap_or_default()
}

/// Dirs-first listing of `dir`, dotfiles skipped, files filtered by `exts`.
fn list(dir: &Path, exts: &[String]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                dirs.push(p);
            } else {
                let ext = p
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.to_lowercase())
                    .unwrap_or_default();
                if exts.is_empty() || exts.contains(&ext) {
                    files.push(p);
                }
            }
        }
    }
    dirs.sort();
    files.sort();
    (dirs, files)
}

/// Bottom-sheet file browser answering [`PICK`]. Mounted once at the App root;
/// renders nothing while no request is pending. Styled by the `.mr-picker-*`
/// rules in app.android.css so it shares the app's glass tokens.
#[component]
pub fn AndroidPicker() -> Element {
    let mut dir = use_signal(|| PathBuf::from(ROOT));
    let mut sel = use_signal(Vec::<PathBuf>::new);
    let mut tick = use_signal(|| 0u32);
    // Refresh while open: picks up a permission grant (the system dialog gives
    // us no callback) and newly arrived files.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            if PICK.read().is_some() {
                tick += 1;
            }
        }
    });

    let req = PICK.read();
    let Some(req) = req.as_ref() else {
        return rsx! {};
    };
    let _ = tick();
    let title = req.title.clone();
    let multiple = req.multiple;
    let has_perm = has_media_permission();
    let (dirs, files) = list(&dir(), &req.exts);
    let can_up = dir() != Path::new(ROOT);
    drop(req);

    let mut finish = move |paths: Vec<PathBuf>| {
        if let Some(r) = PICK.write().take() {
            let _ = r.tx.send(paths);
        }
        sel.set(Vec::new());
        dir.set(PathBuf::from(ROOT));
    };

    rsx! {
        div { class: "mr-picker-scrim",
            div { class: "mr-picker-sheet",
                div { class: "mr-picker-title", "{title}" }
                div { class: "mr-picker-path", "{dir().display()}" }
                if !has_perm {
                    div { class: "mr-picker-perm",
                        "MorReel needs media access to list your videos and photos."
                        button {
                            class: "mr-picker-btn",
                            style: "margin-top:8px;display:block;width:100%;",
                            onclick: move |_| {
                                request_media_permissions();
                                tick += 1;
                            },
                            "Grant access"
                        }
                    }
                }
                div { class: "mr-picker-list",
                    if can_up {
                        button {
                            class: "mr-picker-row",
                            onclick: move |_| {
                                let up = dir().parent().map(Path::to_path_buf);
                                if let Some(u) = up { dir.set(u); }
                            },
                            "⬆  .."
                        }
                    }
                    for d in dirs {
                        button {
                            key: "{d.display()}",
                            class: "mr-picker-row",
                            onclick: {
                                let d = d.clone();
                                move |_| dir.set(d.clone())
                            },
                            "📁  {d.file_name().unwrap_or_default().to_string_lossy()}"
                        }
                    }
                    for f in files {
                        button {
                            key: "{f.display()}",
                            class: if sel().contains(&f) { "mr-picker-row sel" } else { "mr-picker-row" },
                            onclick: {
                                let f = f.clone();
                                move |_| {
                                    if multiple {
                                        let mut s = sel();
                                        match s.iter().position(|p| p == &f) {
                                            Some(i) => { s.remove(i); }
                                            None => s.push(f.clone()),
                                        }
                                        sel.set(s);
                                    } else {
                                        finish(vec![f.clone()]);
                                    }
                                }
                            },
                            "{f.file_name().unwrap_or_default().to_string_lossy()}"
                        }
                    }
                }
                div { class: "mr-picker-actions",
                    button { class: "mr-picker-btn", onclick: move |_| finish(Vec::new()), "Cancel" }
                    if multiple {
                        button {
                            class: "mr-picker-btn primary",
                            onclick: move |_| { let s = sel(); finish(s); },
                            "Add ({sel().len()})"
                        }
                    }
                }
            }
        }
    }
}

// --- Webview bridges -------------------------------------------------------
// Android has no curl or pango, but the webview ships a full HTTP stack and
// native color-emoji rendering — so those two desktop shell-outs route
// through document::eval instead.

fn b64_payload(reply: String) -> Result<Vec<u8>, String> {
    use base64::Engine;
    match reply.split_once(':') {
        Some(("B64", data)) => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| e.to_string()),
        Some(("ERR", err)) => Err(err.to_string()),
        _ => Err("malformed webview reply".to_string()),
    }
}

/// GET `url` through the webview's fetch().
pub async fn web_fetch(url: &str) -> Result<Vec<u8>, String> {
    let url_js = serde_json::to_string(url).map_err(|e| e.to_string())?;
    let js = format!(
        r#"try {{
            const r = await fetch({url_js});
            if (!r.ok) {{ dioxus.send("ERR:HTTP " + r.status); }} else {{
                const buf = new Uint8Array(await r.arrayBuffer());
                let s = ""; const CH = 32768;
                for (let i = 0; i < buf.length; i += CH)
                    s += String.fromCharCode.apply(null, buf.subarray(i, i + CH));
                dioxus.send("B64:" + btoa(s));
            }}
        }} catch (e) {{ dioxus.send("ERR:" + e); }}"#
    );
    let mut eval = dioxus::document::eval(&js);
    b64_payload(eval.recv::<String>().await.map_err(|e| e.to_string())?)
}

/// Rasterize `text` (an emoji or a short run of them) to a transparent PNG
/// via a webview canvas.
pub async fn emoji_canvas_png(text: &str) -> Result<Vec<u8>, String> {
    let text_js = serde_json::to_string(text).map_err(|e| e.to_string())?;
    let js = format!(
        r#"try {{
            const t = {text_js};
            const f = '256px "Noto Color Emoji", sans-serif';
            const probe = document.createElement('canvas').getContext('2d');
            probe.font = f;
            const w = Math.max(1, Math.ceil(probe.measureText(t).width));
            const c = document.createElement('canvas');
            c.width = w; c.height = 340;
            const x = c.getContext('2d');
            x.font = f; x.textBaseline = 'middle';
            x.fillText(t, 0, 170);
            dioxus.send("B64:" + c.toDataURL('image/png').split(',')[1]);
        }} catch (e) {{ dioxus.send("ERR:" + e); }}"#
    );
    let mut eval = dioxus::document::eval(&js);
    b64_payload(eval.recv::<String>().await.map_err(|e| e.to_string())?)
}

// --- JNI ---

/// Run `f` with an attached JNIEnv and the activity object; swallow (and
/// clear) any pending Java exception so one bad call can't poison the next.
fn with_env<R>(
    f: impl FnOnce(&mut jni::JNIEnv, &JObject) -> jni::errors::Result<R>,
) -> Option<R> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let act = unsafe { JObject::from_raw(ctx.context() as jni::sys::jobject) };
    let r = f(&mut env, &act);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    r.ok()
}

/// Android 13+ media permissions plus the pre-13 storage one; the OS ignores
/// whichever set doesn't apply, so "any granted" is the right check on both.
const PERMS: [&str; 4] = [
    "android.permission.READ_MEDIA_VIDEO",
    "android.permission.READ_MEDIA_IMAGES",
    "android.permission.READ_MEDIA_AUDIO",
    "android.permission.READ_EXTERNAL_STORAGE",
];

pub fn has_media_permission() -> bool {
    with_env(|env, act| {
        for p in PERMS {
            let s = env.new_string(p)?;
            let granted = env
                .call_method(act, "checkSelfPermission", "(Ljava/lang/String;)I", &[JValue::Object(&s)])?
                .i()?
                == 0;
            if granted {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .unwrap_or(false)
}

pub fn request_media_permissions() {
    if has_media_permission() {
        return;
    }
    with_env(|env, act| {
        let arr = env.new_object_array(PERMS.len() as i32, "java/lang/String", JObject::null())?;
        for (i, p) in PERMS.iter().enumerate() {
            let s = env.new_string(p)?;
            env.set_object_array_element(&arr, i as i32, s)?;
        }
        env.call_method(
            act,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&arr), JValue::Int(7)],
        )?;
        Ok(())
    });
}

/// Writable, USB-visible directory for exports and saved projects
/// (…/Android/data/<pkg>/files) — needs no permission at all.
pub fn save_dir() -> PathBuf {
    with_env(|env, act| {
        let mut f = env
            .call_method(act, "getExternalFilesDir", "(Ljava/lang/String;)Ljava/io/File;", &[JValue::Object(&JObject::null())])?
            .l()?;
        if f.is_null() {
            f = env.call_method(act, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
        }
        let p = env.call_method(&f, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?;
        let p = JString::from(p);
        let s = env.get_string(&p)?;
        Ok(PathBuf::from(s.to_str().unwrap_or(".")))
    })
    .unwrap_or_else(|| PathBuf::from("."))
}
