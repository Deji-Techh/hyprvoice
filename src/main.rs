//! hyprvoice — audio-only notch voice-to-text for Hyprland.
//!
//! Pipeline: toggle → pw-record 16kHz mono → duration gate →
//! Groq whisper-large-v3-turbo → wl-copy + ydotool Ctrl+V paste.
//!
//! Key resolution (ported from Caide-final
//! `apps/server/src/voice/transcriptionService.ts` + `dyad/providers/secrets.ts`):
//! 1. `GROQ_API_KEY` env (override)
//! 2. `~/.caide/dyad-providers.json` v2 AES-256-GCM envelope (key: `~/.caide/.providers.key`)
//!    or v1 plaintext `{providers:{groq:{apiKey}}}`
//! 3. Legacy `~/.caide/userdata/secrets/provider-groq-api-key.bin`

mod keys;
mod paste;
mod state;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

const MODEL: &str = "whisper-large-v3-turbo";
const GROQ_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MIN_DURATION_SECS: f64 = 0.4;
const MIN_FILE_BYTES: u64 = 8_000;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn wav_path() -> PathBuf {
    runtime_dir().join("hyprvoice.wav")
}
fn pid_path() -> PathBuf {
    runtime_dir().join("hyprvoice.pid")
}
fn last_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or(PathBuf::from("/tmp"))
        .join(".cache/hyprvoice-last.txt")
}

#[derive(Parser)]
#[command(name = "hyprvoice", about = "Audio-notch voice-to-text for Hyprland")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Toggle: start recording if idle, else stop → transcribe → paste.
    Toggle,
    /// Start recording (no-op if already recording).
    Start,
    /// Stop recording, transcribe and paste.
    Stop,
    /// Print current status (idle|recording|transcribing).
    Status,
    /// Transcribe an existing wav file and paste the text.
    Transcribe { file: PathBuf },
    /// Print resolved key presence (never prints the key itself).
    CheckKey,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Toggle => toggle().await?,
        Cmd::Start => start_recording()?,
        Cmd::Stop => stop_transcribe_paste().await?,
        Cmd::Status => println!("{}", status()),
        Cmd::Transcribe { file } => transcribe_paste_file(&file).await?,
        Cmd::CheckKey => match keys::resolve_groq_key() {
            Some(_) => println!("groq key: present"),
            None => {
                println!("groq key: MISSING (set GROQ_API_KEY or configure Caide providers)");
                std::process::exit(1);
            }
        },
    }
    Ok(())
}

fn status() -> &'static str {
    if pid_path().exists() {
        "recording"
    } else if state::is_transcribing() {
        "transcribing"
    } else {
        "idle"
    }
}

async fn toggle() -> Result<()> {
    if pid_path().exists() {
        stop_transcribe_paste().await
    } else {
        start_recording()
    }
}

fn start_recording() -> Result<()> {
    if pid_path().exists() {
        return Ok(());
    }
    let wav = wav_path();
    if wav.exists() {
        let _ = std::fs::remove_file(&wav);
    }
    // pw-record: 16kHz mono s16 — what Whisper resamples to anyway.
    // Detach stdio: the recorder outlives us (killed via pid file on
    // stop). Inheriting stdout would hold the caller's pipe open.
    use std::process::Stdio;
    let child = Command::new("pw-record")
        .args([
            "--rate",
            "16000",
            "--channels",
            "1",
            "--format",
            "s16",
            wav.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn pw-record (is PipeWire installed?)")?;
    std::fs::write(pid_path(), child.id().to_string())?;
    // Leak the Child so the recorder keeps running after we exit;
    // stop() kills it via the pid file.
    std::mem::forget(child);
    state::write_state("recording", "")?;
    notify("hyprvoice: recording… (toggle again to stop)")?;
    Ok(())
}

async fn stop_transcribe_paste() -> Result<()> {
    let pid_file = pid_path();
    if pid_file.exists() {
        let pid: String = std::fs::read_to_string(&pid_file).unwrap_or_default();
        if let Ok(pid) = pid.trim().parse::<i32>() {
            // SIGINT lets pw-record finalize the WAV header.
            unsafe { libc_kill(pid) };
        }
        let _ = std::fs::remove_file(&pid_file);
        // Give pw-record a moment to flush.
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let wav = wav_path();
    if !wav.exists() {
        state::write_state("idle", "")?;
        bail!("no recording found");
    }
    transcribe_paste_file(&wav).await
}

async fn transcribe_paste_file(wav: &std::path::Path) -> Result<()> {
    // Duration + size gates (PTT is its own VAD).
    let meta = std::fs::metadata(wav)?;
    if meta.len() < MIN_FILE_BYTES {
        state::write_state("idle", "")?;
        notify("hyprvoice: too short, discarded")?;
        return Ok(());
    }
    if meta.len() > MAX_FILE_BYTES {
        state::write_state("idle", "")?;
        bail!("audio exceeds 10MB limit");
    }
    if let Some(dur) = wav_duration_secs(wav) {
        if dur < MIN_DURATION_SECS {
            state::write_state("idle", "")?;
            notify("hyprvoice: too short, discarded")?;
            return Ok(());
        }
    }
    // Silence gate: Whisper hallucinates ("Thank you.") on near-silence.
    if wav_rms(wav).map(|r| r < 150.0).unwrap_or(false) {
        state::write_state("idle", "")?;
        notify("hyprvoice: silence, discarded")?;
        return Ok(());
    }

    state::write_state("transcribing", "")?;
    notify("hyprvoice: transcribing…")?;

    let key = keys::resolve_groq_key().context(
        "no Groq key: set GROQ_API_KEY or configure it in Caide (~/.caide/dyad-providers.json)",
    )?;
    let text = transcribe_groq(wav, &key).await?;
    let text = text.trim().to_string();
    if text.is_empty() {
        state::write_state("idle", "")?;
        notify("hyprvoice: no speech detected")?;
        return Ok(());
    }

    // Always keep a copy.
    if let Some(parent) = last_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(last_path(), &text);

    paste::paste_text(&text)?;
    state::write_state("idle", &text)?;
    notify("hyprvoice: done")?;
    println!("{text}");
    Ok(())
}

async fn transcribe_groq(wav: &std::path::Path, key: &str) -> Result<String> {
    let bytes = std::fs::read(wav)?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", MODEL)
        .text("language", "en")
        .text("response_format", "json");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let resp = client
        .post(GROQ_URL)
        .bearer_auth(key)
        .multipart(form)
        .send()
        .await
        .context("groq request failed (network?)")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let head: String = body.chars().take(300).collect();
        bail!("groq returned HTTP {status}: {head}");
    }
    let v: serde_json::Value = resp.json().await?;
    Ok(v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string())
}

/// RMS energy of s16 samples (after the 44-byte header + data-chunk scan).
fn wav_rms(path: &std::path::Path) -> Option<f64> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 48 {
        return None;
    }
    // Samples start after "data" marker; fall back to byte 44.
    let mut start = 44usize;
    for i in 0..data.len().saturating_sub(8) {
        if &data[i..i + 4] == b"data" {
            start = i + 8;
            break;
        }
    }
    let samples: Vec<i16> = data[start..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    if samples.is_empty() {
        return None;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    Some((sum / samples.len() as f64).sqrt())
}

/// WAV header duration (s16 mono 16k expected from our pw-record invocation).
fn wav_duration_secs(path: &std::path::Path) -> Option<f64> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 44 {
        return None;
    }
    // Parse fmt chunk: channels (bytes 22-23), sample rate (24-27), bits (34-35).
    let channels = u16::from_le_bytes([data[22], data[23]]) as f64;
    let rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]) as f64;
    let bits = u16::from_le_bytes([data[34], data[35]]) as f64;
    if rate == 0.0 || channels == 0.0 {
        return None;
    }
    // Find data chunk size (first "data" marker).
    let mut data_bytes = (data.len() as f64) - 44.0;
    for i in 0..data.len().saturating_sub(8) {
        if &data[i..i + 4] == b"data" {
            data_bytes = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]])
                as f64;
            break;
        }
    }
    Some(data_bytes / (rate * channels * (bits / 8.0)))
}

fn notify(msg: &str) -> Result<()> {
    // hyprctl notify: compositor overlay, never steals focus.
    // (notify-send/libnotify is broken on this box — don't use it.)
    let _ = Command::new("hyprctl").args(["notify", "1", "2000", msg]).output();
    Ok(())
}

/// kill(pid, SIGINT) without pulling in a libc crate.
unsafe fn libc_kill(pid: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid, 2);
    }
}
