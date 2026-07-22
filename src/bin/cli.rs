//! `morreel` — drive the running MorReel editor from the terminal.
//!
//! A thin client for the app's live-control port (127.0.0.1:8177, the same one
//! the MCP server's live path talks to). It reshapes human-friendly `key=value`
//! args into the `{plugin,tool,params}` line protocol, sends one command, and
//! prints the reply — so a power user can nudge a layer without touching the GUI,
//! and an agent can drive the editor with a plain shell command instead of an MCP
//! client. For anything the flat args can't express (arrays), `--json` passes a
//! full params object straight through.
//!
//! ponytail: no engine here — every edit routes through the app's existing
//! Registry over the wire, so the CLI is dumb on purpose and can't drift from what
//! the GUI and MCP paths do. Start the editor first; this only talks to it.

use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

const USAGE: &str = "\
morreel — drive the running MorReel editor from the terminal

USAGE:
    morreel <tool> [key=value ...] [--json '<obj>'] [--plugin NAME] [--port N]

DISCOVERY (asks the running app):
    morreel tools              list every plugin and tool the editor exposes
    morreel items              list V1 clips / V2 overlays with their indices

EXAMPLES:
    morreel place_point lane=V1 index=0 x=0.5 y=0.4
    morreel place_box  lane=V2 index=1 x0=0.1 y0=0.1 x1=0.9 y1=0.9 cover=true
    morreel track_point lane=V1 index=0 --json '{\"samples\":[{\"t\":0,\"x\":0.2,\"y\":0.5},{\"t\":2,\"x\":0.8,\"y\":0.5}],\"zoom\":1.5}'
    morreel set_speed  lane=V1 index=0 speed=2 reverse=true
    morreel trim       lane=V1 index=0 in=1.5 out=4.0
    morreel set_effect lane=V2 index=0 effect=\"Teal & Orange\" amount=0.8
    morreel set_volume lane=V1 index=0 mute=true
    morreel enable     lane=V1 index=1 on=false
    morreel remove     lane=V2 index=0
    morreel get_frame  lane=V1 index=0            # → prints a PNG path to look at

SMART CONFORM (reframe a horizontal clip into 9:16):
    1. morreel get_frame lane=V1 index=0     # read the PNG: where's the subject?
    2. morreel place_box lane=V1 index=0 x0=.. y0=.. x1=.. y1=..   # frame it
       (or track_point to follow a moving subject)

NOTES:
    The plugin is picked from the tool name (place_*/track_* → coords,
    set_*/trim/enable/remove → edit); override with --plugin.
    lane/index are folded into the target the plugins expect.
    A value that parses as JSON (0.5, true, [..]) is sent as that; else a string.
    Port defaults to 8177 or $MORREEL_LIVE_PORT.";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let first = args.first().map(String::as_str).unwrap_or("");
    if matches!(first, "" | "-h" | "--help" | "help") {
        return Ok(USAGE.to_string());
    }
    let (req, port) = build_request(args)?;
    send(port, &req)
}

/// Turn the argv into a `{plugin,tool,params}` request and the port to send it on.
/// Split out so the reshaping (the only real logic here) is unit-testable without
/// a socket.
fn build_request(args: &[String]) -> Result<(Value, u16), String> {
    let mut port = default_port();
    let mut plugin: Option<String> = None;
    let mut tool: Option<String> = None;
    let mut params = Map::new();
    let mut json_base: Option<Value> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(flag) = a.strip_prefix("--") {
            let mut next = || it.next().ok_or_else(|| format!("--{flag} needs a value")).cloned();
            match flag {
                "port" => port = next()?.parse().map_err(|_| "bad --port".to_string())?,
                "plugin" => plugin = Some(next()?),
                "json" => json_base = Some(serde_json::from_str(&next()?).map_err(|e| format!("bad --json: {e}"))?),
                other => return Err(format!("unknown flag --{other}")),
            }
        } else if tool.is_none() {
            tool = Some(a.clone());
        } else if let Some((k, v)) = a.split_once('=') {
            params.insert(k.to_string(), parse_val(v));
        } else {
            return Err(format!("expected key=value, got '{a}'"));
        }
    }

    // Friendly discovery aliases → the tool names the app's live coroutine knows.
    let tool = match tool.ok_or("missing tool name (try `morreel tools`)")?.as_str() {
        "tools" => "list_tools",
        "items" | "list" => "list_items",
        t => t,
    }
    .to_string();

    // `--json` is the params base; explicit key=value pairs override it.
    let mut params = match json_base {
        Some(Value::Object(m)) => {
            let mut m = m;
            m.extend(params);
            m
        }
        Some(_) => return Err("--json must be a JSON object".into()),
        None => params,
    };

    // Fold flat lane/index into the nested `target` the plugins deserialize — the
    // same reshape the MCP server's live path does, so humans never type the nesting.
    let lane = params.remove("lane");
    let index = params.remove("index");
    if lane.is_some() || index.is_some() {
        let mut target = params.remove("target").and_then(|v| v.as_object().cloned()).unwrap_or_default();
        if let Some(l) = lane {
            target.insert("lane".into(), l);
        }
        if let Some(i) = index {
            target.insert("index".into(), i);
        }
        params.insert("target".into(), Value::Object(target));
    }

    // Unless the user forced one with --plugin, pick the plugin from the tool name
    // so `morreel set_speed …` and `morreel place_point …` both just work.
    let plugin = plugin.unwrap_or_else(|| plugin_for(&tool).to_string());

    Ok((json!({ "plugin": plugin, "tool": tool, "params": Value::Object(params) }), port))
}

/// The in-app plugin a tool belongs to — mirrors the server's own routing so a
/// bare `morreel <tool>` reaches the right handler. Discovery aliases resolve to
/// coords' read-only listers.
const EDIT_TOOLS: &[&str] = &["set_effect", "set_speed", "trim", "enable", "set_volume", "remove"];

fn plugin_for(tool: &str) -> &'static str {
    if EDIT_TOOLS.contains(&tool) {
        "edit"
    } else {
        "coords"
    }
}

/// A value that parses as JSON (number, bool, array, object) is sent as that;
/// anything else (e.g. `V1`) is a bare string.
fn parse_val(v: &str) -> Value {
    serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_string()))
}

fn default_port() -> u16 {
    std::env::var("MORREEL_LIVE_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8177)
}

/// Send one command and return the editor's one-line result (or its error).
fn send(port: u16, req: &Value) -> Result<String, String> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        format!("MorReel not reachable on 127.0.0.1:{port} ({e}); start the editor first")
    })?;
    let mut wr = &stream;
    writeln!(wr, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let resp: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply from editor: {e}"))?;
    if let Some(msg) = resp.get("ok").and_then(Value::as_str) {
        Ok(msg.to_string())
    } else {
        Err(resp.get("error").and_then(Value::as_str).unwrap_or("unknown editor error").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(args: &[&str]) -> Value {
        build_request(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap().0
    }

    #[test]
    fn flat_lane_index_fold_into_target_and_values_are_typed() {
        let req = build(&["place_point", "lane=V1", "index=0", "x=0.5", "y=0.4"]);
        assert_eq!(req["tool"], "place_point");
        assert_eq!(req["params"]["target"], json!({ "lane": "V1", "index": 0 }));
        // 0.5 parsed as a number, not the string "0.5".
        assert_eq!(req["params"]["x"], json!(0.5));
        assert!(req["params"].get("lane").is_none(), "lane must move under target");
    }

    #[test]
    fn json_base_is_overridden_by_explicit_pairs() {
        let req = build(&["track_point", "lane=V1", "index=0", "--json", r#"{"zoom":1.3,"samples":[]}"#, "zoom=1.5"]);
        assert_eq!(req["params"]["zoom"], json!(1.5)); // key=value wins over --json
        assert!(req["params"]["samples"].is_array());
        assert_eq!(req["params"]["target"], json!({ "lane": "V1", "index": 0 }));
    }

    #[test]
    fn plugin_is_inferred_from_the_tool_name() {
        // edit verb → edit plugin, no --plugin needed
        let req = build(&["set_speed", "lane=V1", "index=0", "speed=2"]);
        assert_eq!(req["plugin"], "edit");
        assert_eq!(req["params"]["target"], json!({ "lane": "V1", "index": 0 }));
        // coords verb stays coords
        assert_eq!(build(&["place_point", "lane=V1", "index=0", "x=0.5", "y=0.5"])["plugin"], "coords");
        // explicit --plugin wins
        assert_eq!(build(&["set_speed", "--plugin", "coords", "lane=V1", "index=0"])["plugin"], "coords");
    }

    #[test]
    fn discovery_aliases_and_port_flag() {
        let (req, port) = build_request(&["tools", "--port", "9000"].iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        assert_eq!(req["tool"], "list_tools");
        assert_eq!(port, 9000);
    }
}
