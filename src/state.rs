//! Pill state file polled by the Quickshell Overlay:
//! $XDG_RUNTIME_DIR/hyprvoice-state.json {status, text, updated_at}

use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

fn state_path() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("hyprvoice-state.json")
}

#[derive(Serialize)]
struct State<'a> {
    status: &'a str,
    text: &'a str,
    updated_at: String,
}

pub fn write_state(status: &str, text: &str) -> Result<()> {
    let s = State {
        status,
        text,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let body = serde_json::to_string(&s)?;
    std::fs::write(state_path(), body)?;
    Ok(())
}

pub fn is_transcribing() -> bool {
    let Ok(raw) = std::fs::read_to_string(state_path()) else {
        return false;
    };
    raw.contains("\"transcribing\"")
}
