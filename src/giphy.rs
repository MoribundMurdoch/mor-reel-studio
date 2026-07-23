//! Browse GIPHY inside the editor: search for GIFs/stickers, then drop one onto
//! the timeline. No HTTP client — we shell to `curl` (same shell-over-engine
//! stance as the git and ffmpeg calls), so search is one process and download is
//! another. Thumbnails aren't fetched here at all: the desktop webview loads each
//! result's preview URL itself (see the `<img src>` in the browser modal).
//!
//! Needs a free GIPHY API key in `GIPHY_API_KEY` (https://developers.giphy.com).

use crate::engine::config_dir;
use serde_json::Value;
use std::path::PathBuf;

/// One search hit. `preview` is a small animated URL the webview shows directly;
/// `download` is the full media we save when the user picks it.
#[derive(Clone, PartialEq, Debug)]
pub struct Gif {
    pub id: String,
    pub preview: String,
    pub download: String,
    pub ext: String,
    pub title: String,
}

/// The key, or a message explaining how to set one. Read per-call so a user can
/// export it and hit Search without relaunching.
pub fn api_key() -> Result<String, String> {
    std::env::var("GIPHY_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| "Set GIPHY_API_KEY (free key at developers.giphy.com) and search again.".to_string())
}

/// Top ~24 GIFs (or transparent stickers) for `query`.
pub async fn search(query: &str, stickers: bool, key: &str) -> Result<Vec<Gif>, String> {
    let kind = if stickers { "stickers" } else { "gifs" };
    let url = format!(
        "https://api.giphy.com/v1/{kind}/search?api_key={k}&q={q}&limit=24&rating=pg-13&bundle=messaging_non_clips",
        k = enc(key),
        q = enc(query),
    );
    let body = curl(&["-sSL", "--max-time", "30", &url]).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("GIPHY sent something unreadable ({e})"))?;
    // GIPHY reports auth/rate errors in meta, HTTP 200 with an empty data array.
    if let Some(msg) = v.get("meta").and_then(|m| m.get("msg")).and_then(Value::as_str) {
        if v.get("meta").and_then(|m| m.get("status")).and_then(Value::as_i64) != Some(200) {
            return Err(format!("GIPHY: {msg}"));
        }
    }
    let data = v.get("data").and_then(Value::as_array).ok_or("GIPHY response had no results")?;
    Ok(data.iter().filter_map(|it| from_item(it, stickers)).collect())
}

/// Save a chosen result under the config dir; returns the local path to import.
pub async fn download(gif: &Gif) -> Result<PathBuf, String> {
    let dir = config_dir().join("giphy");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(format!("{}.{}", safe(&gif.id), gif.ext));
    curl(&["-sSL", "--max-time", "60", "-o", &dest.to_string_lossy(), &gif.download]).await?;
    Ok(dest)
}

// --- helpers ------------------------------------------------------------------

fn from_item(it: &Value, stickers: bool) -> Option<Gif> {
    let images = it.get("images")?;
    let orig = images.get("original")?;
    // GIFs: mp4 (small, plays clean). Stickers: the transparent .gif — mp4 has no
    // alpha and transparency is the point of a sticker overlay.
    let download = if stickers { orig.get("url") } else { orig.get("mp4").or_else(|| orig.get("url")) }?
        .as_str()?
        .to_string();
    let preview = images
        .get("fixed_width")
        .and_then(|f| f.get("webp").or_else(|| f.get("url")))
        .and_then(Value::as_str)
        .unwrap_or(&download)
        .to_string();
    let id = it.get("id").and_then(Value::as_str).unwrap_or("giphy").to_string();
    let title = it
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();
    Some(Gif { ext: ext_of(&download), id, preview, download, title })
}

fn ext_of(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    for e in ["mp4", "gif", "webp"] {
        if path.ends_with(&format!(".{e}")) {
            return e.to_string();
        }
    }
    "gif".to_string()
}

/// Percent-encode a query/key value (keeps unreserved chars, escapes the rest).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

async fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = tokio::process::Command::new("curl")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("can't run curl ({e}) — install curl to browse GIPHY"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_gif_and_sticker_and_encodes() {
        let item = json!({
            "id": "aB3",
            "title": "Party Time",
            "images": {
                "fixed_width": { "webp": "https://m/p.webp", "url": "https://m/p.gif" },
                "original": { "mp4": "https://m/o.mp4", "url": "https://m/o.gif" }
            }
        });
        let g = from_item(&item, false).unwrap();
        assert_eq!(g.download, "https://m/o.mp4", "gif picks mp4");
        assert_eq!(g.ext, "mp4");
        assert_eq!(g.preview, "https://m/p.webp");
        assert_eq!(g.title, "Party Time");
        let s = from_item(&item, true).unwrap();
        assert_eq!(s.download, "https://m/o.gif", "sticker picks transparent gif");
        assert_eq!(s.ext, "gif");
        assert_eq!(ext_of("https://m/x.mp4?cid=1"), "mp4", "ext ignores query");
        assert_eq!(enc("cats & dogs"), "cats%20%26%20dogs");
        assert_eq!(safe("aB3/../x"), "aB3____x");
    }
}
