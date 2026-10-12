//! Groq key resolution — port of Caide-final
//! `apps/server/src/voice/transcriptionService.ts:getVoiceApiKey`
//! + `dyad/providers/secrets.ts` v2 AES-256-GCM envelope.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use serde_json::Value;
use std::path::PathBuf;

fn caide_home() -> PathBuf {
    std::env::var("CAIDE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or(PathBuf::from("/tmp")).join(".caide"))
}

/// GROQ_API_KEY env wins; then ~/.caide/dyad-providers.json (v2 encrypted or
/// v1 plaintext); then legacy `provider-groq-api-key.bin` files.
pub fn resolve_groq_key() -> Option<String> {
    // 1. env override
    if let Ok(v) = std::env::var("GROQ_API_KEY") {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    // 2. sharedProviderSecrets file
    let secrets_path = caide_home().join("dyad-providers.json");
    if let Some(k) = read_providers_file(&secrets_path, "groq") {
        return Some(k);
    }
    // 3. direct scan fallbacks (mirror of transcriptionService.ts)
    let home = dirs::home_dir().unwrap_or(PathBuf::from("/tmp"));
    for p in [
        home.join("dyad-providers.json"),
        home.join(".dyad/provider_secrets.json"),
    ] {
        if let Some(k) = read_providers_file(&p, "groq") {
            return Some(k);
        }
    }
    // 4. legacy .bin
    for name in ["groq", "Groq"] {
        let p = caide_home().join(format!("userdata/secrets/provider-{name}-api-key.bin"));
        if let Ok(raw) = std::fs::read_to_string(&p) {
            if !raw.trim().is_empty() {
                return Some(raw.trim().to_string());
            }
        }
    }
    None
}

fn read_providers_file(path: &PathBuf, provider: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    // v2 envelope: {version:2, encrypted:{iv,tag,data}}
    if v.get("version").and_then(|x| x.as_u64()) == Some(2) {
        if let Some(k) = decrypt_envelope(&v) {
            return extract_provider_key(&k, provider);
        }
        return None;
    }
    extract_provider_key(&v, provider)
}

fn extract_provider_key(v: &Value, provider: &str) -> Option<String> {
    let providers = v.get("providers")?;
    // exact + lowercase match
    for key in [provider, &provider.to_lowercase()] {
        if let Some(entry) = providers.get(key) {
            if let Some(s) = entry.get("apiKey").and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return Some(s.trim().to_string());
                }
            }
            // some files store the key as a bare string
            if let Some(s) = entry.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.trim().to_string());
                }
            }
        }
    }
    None
}

fn decrypt_envelope(v: &Value) -> Option<Value> {
    let enc = v.get("encrypted")?;
    let iv_b64 = enc.get("iv")?.as_str()?;
    let tag_b64 = enc.get("tag")?.as_str()?;
    let data_b64 = enc.get("data")?.as_str()?;
    let eng = base64::engine::general_purpose::STANDARD;
    let iv = eng.decode(iv_b64).ok()?;
    let tag = eng.decode(tag_b64).ok()?;
    let mut data = eng.decode(data_b64).ok()?;

    // Node aes-256-gcm: ciphertext || auth tag concatenated for open().
    data.extend_from_slice(&tag);

    // Read-only key load — never create on the read path (parity with secrets.ts).
    let key_path = caide_home().join(".providers.key");
    let key_bytes = std::fs::read(key_path).ok()?;
    if key_bytes.len() != 32 {
        return None;
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let nonce = Nonce::from_slice(&iv);
    let plain = cipher.decrypt(nonce, data.as_ref()).ok()?;
    serde_json::from_slice::<Value>(&plain).ok()
}
