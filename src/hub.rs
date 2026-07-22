//! Plugin Hub — read a **local clone** of the `mor-reel-studio-plugin-hub` repo,
//! parse its manifests, and track which the user has installed. Like a package
//! manager pointed at a checkout: no network code here (the user `git pull`s the
//! hub), and nothing that runs code is executed from a manifest — installing a
//! code-running plugin only *records* it and produces an `.mcp.json`-shaped file
//! the user opts into. See the hub repo's README for the manifest format.
//!
//! Three kinds (mirrors the hub schema):
//! - `agent` — an external Claude-Code agent (e.g. ButterCut). Install = record it
//!   and show its setup instructions; MorReel links nothing in.
//! - `mcp` — an external process speaking the tool protocol (e.g. Coordinate
//!   Grounding). Install = add its `run` entry to MorReel's aggregated
//!   `mcp-servers.json` for the user to point Claude Code at.
//! - `bundle` — declarative effect looks, no code. Install = its effects join the
//!   editor's effect list (see [`active_bundle_effects`]).

use crate::engine::config_dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Agent,
    Mcp,
    Bundle,
}

#[derive(Clone, PartialEq, Debug, Deserialize)]
pub struct RunSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Debug, Deserialize)]
pub struct BundleEffect {
    pub category: String,
    pub name: String,
    pub filter: String,
}

#[derive(Clone, PartialEq, Debug, Deserialize)]
pub struct BundleSpec {
    pub effects: Vec<BundleEffect>,
}

/// One plugin manifest, as read from `plugins/<id>.json`. Kind-specific fields are
/// optional here and enforced by the hub's schema, not re-validated in the app —
/// a malformed manifest simply won't do anything useful for its kind.
#[derive(Clone, PartialEq, Debug, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub kind: Kind,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub license: String,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub run: Option<RunSpec>,
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub bundle: Option<BundleSpec>,
}

// --- where the hub checkout lives ---------------------------------------------

fn hub_path_file() -> PathBuf {
    config_dir().join("hub-path")
}

/// The configured local hub checkout, if the user has pointed at one.
pub fn hub_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MORREEL_HUB") {
        return Some(PathBuf::from(p));
    }
    std::fs::read_to_string(hub_path_file()).ok().map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty())
}

/// Point MorReel at a local clone of the hub repo (persisted across launches).
pub fn set_hub_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    std::fs::write(hub_path_file(), dir.to_string_lossy().as_bytes()).map_err(|e| e.to_string())
}

/// The canonical hub repo — cloned on demand so a user doesn't have to `git clone`
/// by hand before browsing plugins.
const HUB_REPO: &str = "https://github.com/MoribundMurdoch/mor-reel-studio-plugin-hub";

/// MorReel's own managed checkout of the hub, under the config dir. Kept separate
/// from a user's hand-picked `set_hub_dir` folder so "fetch" only ever touches the
/// clone it owns.
fn managed_hub_dir() -> PathBuf {
    config_dir().join("plugin-hub")
}

/// Clone the canonical hub (or `git pull` it if already fetched) and point MorReel
/// at it. Shells out to `git` — same shell-over-engine stance as the ffmpeg engine,
/// so no HTTP client or new dependency, and `pull` is the natural "check for new
/// plugins". Only ever fetches manifests; installing a plugin stays a separate,
/// consent-gated step. Returns the checkout dir for the caller to reload from.
pub async fn fetch_hub() -> Result<PathBuf, String> {
    let dir = managed_hub_dir();
    if dir.join(".git").is_dir() {
        run_git(&["-C", &dir.to_string_lossy(), "pull", "--ff-only"]).await?;
    } else {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        run_git(&["clone", "--depth", "1", HUB_REPO, &dir.to_string_lossy()]).await?;
    }
    set_hub_dir(&dir)?;
    Ok(dir)
}

async fn run_git(args: &[&str]) -> Result<(), String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("can't run git ({e}) — install git, or clone the hub manually and choose its folder"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Parse every `plugins/*.json` in the hub checkout. Bad files are skipped (with
/// their name), never fatal — one broken manifest can't hide the rest.
pub fn load_manifests(hub_dir: &Path) -> Vec<Manifest> {
    let dir = hub_dir.join("plugins");
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Manifest>(&t).ok()) {
            Some(m) => out.push(m),
            None => eprintln!("plugin-hub: skipping unreadable manifest {}", path.display()),
        }
    }
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    out
}

// --- install state (MorReel's own record, user-owned) --------------------------

/// id → enabled. Presence = installed; the bool = enabled.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct InstallState(pub BTreeMap<String, bool>);

fn state_file() -> PathBuf {
    config_dir().join("hub-installed.json")
}

impl InstallState {
    pub fn load() -> Self {
        std::fs::read_to_string(state_file())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(state_file(), json).map_err(|e| e.to_string())
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.0.get(id).copied().unwrap_or(false)
    }

    /// Install (enabled) or remove a plugin, persisting immediately.
    pub fn set_installed(&mut self, id: &str, installed: bool) -> Result<(), String> {
        if installed {
            self.0.insert(id.to_string(), true);
        } else {
            self.0.remove(id);
        }
        self.save()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        if self.0.contains_key(id) {
            self.0.insert(id.to_string(), enabled);
            self.save()?;
        }
        Ok(())
    }
}

// --- per-kind derived artifacts ------------------------------------------------

/// The `{ "mcpServers": { … } }` document aggregating every installed **and
/// enabled** `mcp` plugin — the shape a Claude Code `.mcp.json` uses. The user
/// points Claude Code at the written file (or copies an entry into their project
/// `.mcp.json`); this is the consent boundary — MorReel never launches these.
fn mcp_servers_doc(manifests: &[Manifest], state: &InstallState) -> serde_json::Value {
    let mut servers = serde_json::Map::new();
    for m in manifests {
        if m.kind != Kind::Mcp || !state.is_enabled(&m.id) {
            continue;
        }
        if let Some(run) = &m.run {
            let mut entry = serde_json::json!({ "command": run.command, "args": run.args });
            if !run.env.is_empty() {
                entry["env"] = serde_json::to_value(&run.env).unwrap();
            }
            entry["description"] = serde_json::Value::String(m.description.clone());
            servers.insert(m.id.clone(), entry);
        }
    }
    serde_json::json!({ "mcpServers": servers })
}

/// Write the aggregated mcp-servers doc to the config dir and return its path.
pub fn sync_mcp_servers(manifests: &[Manifest], state: &InstallState) -> Result<PathBuf, String> {
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let path = config_dir().join("mcp-servers.json");
    let doc = mcp_servers_doc(manifests, state);
    std::fs::write(&path, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Effects contributed by every installed **and enabled** `bundle` plugin, as the
/// editor's `(category, name, filter)` triples. Later duplicates of a built-in or
/// each other are kept in order; the effect lookup takes the first match.
pub fn active_bundle_effects(manifests: &[Manifest], state: &InstallState) -> Vec<(String, String, String)> {
    manifests
        .iter()
        .filter(|m| m.kind == Kind::Bundle && state.is_enabled(&m.id))
        .filter_map(|m| m.bundle.as_ref())
        .flat_map(|b| b.effects.iter().map(|e| (e.category.clone(), e.name.clone(), e.filter.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifests() -> Vec<Manifest> {
        let mcp = r#"{ "id":"coords","kind":"mcp","displayName":"Coordinate Grounding","author":"m","description":"d","license":"GPL-3.0-or-later","repository":"https://x/y.git","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run":{"command":"cargo","args":["run","--bin","mcp"],"env":{"MORREEL_LIVE":"1"}} }"#;
        let agent = r#"{ "id":"buttercut","kind":"agent","displayName":"ButterCut","author":"bf","description":"d","license":"PolyForm-Noncommercial-1.0.0","repository":"https://x/b.git","commit":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","install":"clone and /setup" }"#;
        let bundle = r#"{ "id":"film-looks","kind":"bundle","displayName":"Film Looks","author":"m","description":"d","license":"GPL-3.0-or-later","bundle":{"effects":[{"category":"Look","name":"Noir","filter":"hue=s=0"}]} }"#;
        [mcp, agent, bundle].iter().map(|s| serde_json::from_str(s).unwrap()).collect()
    }

    #[test]
    fn each_kind_parses_with_its_fields() {
        let ms = manifests();
        assert_eq!(ms[0].kind, Kind::Mcp);
        assert_eq!(ms[0].run.as_ref().unwrap().command, "cargo");
        assert_eq!(ms[1].kind, Kind::Agent);
        assert!(ms[1].install.is_some());
        assert_eq!(ms[2].bundle.as_ref().unwrap().effects[0].name, "Noir");
    }

    #[test]
    fn mcp_doc_only_includes_enabled_mcp_plugins() {
        let ms = manifests();
        let mut st = InstallState::default();
        // Nothing installed → empty.
        assert_eq!(mcp_servers_doc(&ms, &st)["mcpServers"].as_object().unwrap().len(), 0);
        // Install the agent — still no mcp servers (wrong kind).
        st.0.insert("buttercut".into(), true);
        assert_eq!(mcp_servers_doc(&ms, &st)["mcpServers"].as_object().unwrap().len(), 0);
        // Enable coords → it appears with its run command.
        st.0.insert("coords".into(), true);
        let doc = mcp_servers_doc(&ms, &st);
        assert_eq!(doc["mcpServers"]["coords"]["command"], "cargo");
        assert_eq!(doc["mcpServers"]["coords"]["env"]["MORREEL_LIVE"], "1");
        // Disable it → gone.
        st.0.insert("coords".into(), false);
        assert_eq!(mcp_servers_doc(&ms, &st)["mcpServers"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn load_manifests_reads_a_directory_and_skips_non_json() {
        let dir = std::env::temp_dir().join(format!("morreel-hub-test-{}", std::process::id()));
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        for (name, kind) in [("coords", "mcp"), ("buttercut", "agent"), ("film-looks", "bundle")] {
            std::fs::write(plugins.join(format!("{name}.json")), sample_json(name, kind)).unwrap();
        }
        std::fs::write(plugins.join("README.txt"), "not a manifest").unwrap();

        let loaded = load_manifests(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(loaded.len(), 3, "3 json manifests, .txt skipped");
        // Sorted by display name: ButterCut, Coordinate Grounding, Film Looks.
        assert_eq!(loaded[0].id, "buttercut");
        assert_eq!(loaded[1].id, "coords");
    }

    fn sample_json(id: &str, kind: &str) -> String {
        match kind {
            "mcp" => format!(r#"{{"id":"{id}","kind":"mcp","displayName":"Coordinate Grounding","author":"m","description":"d","license":"GPL-3.0-or-later","repository":"https://x/y.git","commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run":{{"command":"cargo","args":["run"]}}}}"#),
            "agent" => format!(r#"{{"id":"{id}","kind":"agent","displayName":"ButterCut","author":"bf","description":"d","license":"PolyForm-Noncommercial-1.0.0","repository":"https://x/b.git","commit":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","install":"clone and /setup"}}"#),
            _ => format!(r#"{{"id":"{id}","kind":"bundle","displayName":"Film Looks","author":"m","description":"d","license":"GPL-3.0-or-later","bundle":{{"effects":[{{"category":"Look","name":"Noir","filter":"hue=s=0"}}]}}}}"#),
        }
    }

    #[test]
    fn bundle_effects_appear_only_when_enabled() {
        let ms = manifests();
        let mut st = InstallState::default();
        assert!(active_bundle_effects(&ms, &st).is_empty());
        st.0.insert("film-looks".into(), true);
        let fx = active_bundle_effects(&ms, &st);
        assert_eq!(fx.len(), 1);
        assert_eq!(fx[0], ("Look".into(), "Noir".into(), "hue=s=0".into()));
    }
}
