//! Text injection: wl-copy + ydotool Ctrl+V (primary),
//! ydotool type --key-delay 0 (fallback).
//!
//! Both deliver to the currently focused client without refocusing,
//! so the pill/daemon never steals focus from the target text field.

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Paste `text` into the currently focused field.
pub fn paste_text(text: &str) -> Result<()> {
    // Save current clipboard for async restore.
    let old = Command::new("wl-paste")
        .arg("-n")
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();

    // Stage new text.
    let mut child = Command::new("wl-copy")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .context("wl-copy failed")?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(text.as_bytes())?;
    let _ = child.wait();
    // Electron/Chrome need a beat to observe the clipboard.
    std::thread::sleep(std::time::Duration::from_millis(80));

    let terminal = active_window_is_terminal();
    let status = if terminal {
        // Ctrl+Shift+V — keycodes: 42=Shift 29=Ctrl 47=V
        Command::new("ydotool")
            .args(["key", "42:1", "29:1", "47:1", "47:0", "29:0", "42:0"])
            .status()
    } else {
        Command::new("ydotool")
            .args(["key", "29:1", "47:1", "47:0", "29:0"])
            .status()
    };
    match status {
        Ok(s) if s.success() => {}
        _ => {
            // Fallback: direct typing (slower, ASCII-biased).
            let _ = Command::new("ydotool")
                .args(["type", "--key-delay", "0"])
                .stdin(Stdio::piped())
                .spawn()
                .and_then(|mut c| {
                    c.stdin.as_mut().unwrap().write_all(text.as_bytes())?;
                    c.wait()
                });
        }
    }

    // Restore previous clipboard asynchronously.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        if old.is_empty() {
            return;
        }
        if let Ok(mut c) = Command::new("wl-copy")
            .arg("-n")
            .stdin(Stdio::piped())
            .spawn()
        {
            let _ = c.stdin.as_mut().unwrap().write_all(&old);
            let _ = c.wait();
        }
    });
    Ok(())
}

fn active_window_is_terminal() -> bool {
    let out = Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output();
    let Ok(out) = out else { return false };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let class = v.get("class").and_then(|c| c.as_str()).unwrap_or("").to_lowercase();
    ["kitty", "foot", "alacritty", "wezterm", "gnome-terminal", "konsole", "ghostty"]
        .iter()
        .any(|t| class.contains(t))
}
