//! Drop an emoji on the timeline as a transparent PNG sticker. ffmpeg's
//! drawtext can't rasterize colour-emoji bitmap strikes (see
//! engine::font_is_unusable), so we shell to `pango-view` instead — same
//! shell-over-engine stance as ffmpeg/curl — and cache one PNG per emoji
//! under the config dir like Giphy downloads.

use crate::engine::{config_dir, H, W};
use std::path::PathBuf;

// ponytail: short favorites grid; free-text input covers everything else.
pub const PICKS: &[&str] = &[
    "😀", "😂", "🥰", "😍", "😎", "🤔", "😭", "😡", "🥳", "🤯",
    "👍", "👎", "👏", "🙌", "🙏", "💪", "🔥", "✨", "⭐", "💯",
    "❤️", "💔", "💕", "💀", "👻", "🤖", "🎉", "🚀", "👀", "💬",
];

/// Cache filename stem: hex codepoints joined by '-', so "❤️" and "❤" get
/// distinct files and nothing shell-unsafe reaches the filesystem.
fn cache_stem(emoji: &str) -> String {
    emoji
        .chars()
        .map(|c| format!("{:x}", c as u32))
        .collect::<Vec<_>>()
        .join("-")
}

/// Rasterize `emoji` to a transparent PNG (cached); returns the file to import.
///
/// The glyph is centered on a transparent full-frame (1080×1920) canvas: the
/// overlay pipeline's default "Crop" framing scales-to-cover and center-crops,
/// which would chop a tight square render — and "Fit" letterboxes on opaque
/// black, killing the transparency. A frame-sized canvas makes framing a no-op.
pub async fn render(emoji: &str) -> Result<PathBuf, String> {
    let emoji = emoji.trim();
    if emoji.is_empty() {
        return Err("Pick or type an emoji first.".to_string());
    }
    let dir = config_dir().join("emoji");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // ".916" marks the padded-canvas format so stale tight-cropped caches from
    // the first cut of this feature are ignored, not shipped cut off.
    let dest = dir.join(format!("{}.916.png", cache_stem(emoji)));
    if dest.exists() {
        return Ok(dest);
    }
    // Noto Color Emoji is a 128px bitmap face; 256pt just scales it up so the
    // sticker isn't blurry after the user grows it on a 1080-wide frame. If the
    // family is missing, fontconfig substitutes whatever emoji face exists.
    let raw = dest.with_extension("raw.png");
    #[cfg(not(target_os = "android"))]
    {
        let out = tokio::process::Command::new("pango-view")
            .args([
                "--no-display",
                "-q",
                "--background=transparent",
                "--font=Noto Color Emoji 256",
                "-o",
                &raw.to_string_lossy(),
                "-t",
                emoji,
            ])
            .output()
            .await
            .map_err(|e| format!("can't run pango-view ({e}) — install pango to add emoji"))?;
        if !out.status.success() || !raw.exists() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
    }
    // No pango on Android — the webview's canvas renders color emoji natively.
    #[cfg(target_os = "android")]
    std::fs::write(&raw, crate::droid::emoji_canvas_png(emoji).await?).map_err(|e| e.to_string())?;
    let glyph = image::open(&raw).map_err(|e| e.to_string())?.to_rgba8();
    let _ = std::fs::remove_file(&raw);
    // The canvas render carries generous padding pango never had — crop to the
    // glyph's alpha bounding box so the sticker starts at a sensible size.
    #[cfg(target_os = "android")]
    let glyph = {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (x, y, p) in glyph.enumerate_pixels() {
            if p.0[3] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        if x0 > x1 {
            glyph
        } else {
            image::imageops::crop_imm(&glyph, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
        }
    };
    // A pasted multi-emoji string can outgrow the frame — shrink to fit first.
    let glyph = if glyph.width() > W || glyph.height() > H {
        let s = f64::min(W as f64 / glyph.width() as f64, H as f64 / glyph.height() as f64);
        image::imageops::resize(
            &glyph,
            ((glyph.width() as f64 * s) as u32).max(1),
            ((glyph.height() as f64 * s) as u32).max(1),
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        glyph
    };
    let mut canvas = image::RgbaImage::new(W, H);
    image::imageops::overlay(
        &mut canvas,
        &glyph,
        i64::from((W - glyph.width().min(W)) / 2),
        i64::from((H - glyph.height().min(H)) / 2),
    );
    canvas.save(&dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_are_distinct_and_fs_safe() {
        assert_eq!(cache_stem("🔥"), "1f525");
        assert_eq!(cache_stem("❤️"), "2764-fe0f", "keeps the VS16 selector");
        assert_ne!(cache_stem("❤️"), cache_stem("❤"));
        assert!(cache_stem("../x").chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }

    #[test]
    fn picks_are_unique_and_nonempty() {
        let mut seen = std::collections::HashSet::new();
        for e in PICKS {
            assert!(!e.trim().is_empty());
            assert!(seen.insert(*e), "duplicate pick {e:?}");
        }
    }
}
