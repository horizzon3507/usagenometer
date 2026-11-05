//! Local installation and credential usability only; no server validation or token refresh.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::types::coerce_unix_seconds;
use crate::jwt::{jwt_claims, normalize_bearer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialStatus {
    Ready,
    Missing,
    Expired,
    Invalid,
    Unknown,
}

impl fmt::Display for CredentialStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::Invalid => "invalid",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Availability {
    pub installed: bool,
    pub credentials: CredentialStatus,
    pub detail: String,
    pub setup: String,
}

impl Availability {
    pub fn ready(&self) -> bool {
        self.installed && self.credentials == CredentialStatus::Ready
    }
}

type Check = (CredentialStatus, &'static str);
const READY: Check = (
    CredentialStatus::Ready,
    "Locally usable credentials; server acceptance has not been checked",
);
const MISSING: Check = (
    CredentialStatus::Missing,
    "No usable credential found in the native authentication sources",
);
const INVALID: Check = (
    CredentialStatus::Invalid,
    "Native credentials are malformed, empty, or unreadable",
);
const EXPIRED: Check = (
    CredentialStatus::Expired,
    "Credentials expired; this integration cannot refresh them",
);
const REFRESHABLE: Check = (
    CredentialStatus::Ready,
    "Expired access token has a local refresh credential; refresh and server acceptance have not been checked",
);
const UNKNOWN: Check = (
    CredentialStatus::Unknown,
    "Native credential storage could not be inspected; login validity is unknown",
);

pub fn inspect(id: &str) -> Availability {
    let (installed, check, setup) = match id {
        "claude" => (
            has_command("claude")
                || app_installed("Claude", &["claude.desktop", "claude-code.desktop"]),
            claude_auth(),
            "claude auth login",
        ),
        "codex" => (
            has_command("codex")
                || app_installed("Codex", &["codex.desktop", "codex-app-linux.desktop"]),
            codex_auth(),
            "codex login",
        ),
        "cursor" => (
            has_any_command(&["cursor", "cursor-agent", "agent"])
                || app_installed("Cursor", &["cursor.desktop"]),
            description_check(super::cursor::auth_description()),
            "cursor-agent login",
        ),
        "antigravity" => (
            has_any_command(&["antigravity", "agy"])
                || app_installed("Antigravity", &["antigravity.desktop"]),
            description_check(super::antigravity::auth_description()),
            "antigravity",
        ),
        "grok" => (has_command("grok"), grok_auth(), "grok login"),
        "glm" => (
            has_command("claude"),
            glm_auth(),
            "Set ZAI_API_KEY or configure Claude Code for z.ai",
        ),
        "fx" => (has_command("fx"), fx_auth(), "fx login or fx setup"),
        "commandcode" => (
            has_any_command(&["commandcode", "command-code"]),
            commandcode_auth(),
            "commandcode login",
        ),
        "kimi" => (
            has_any_command(&["kimi", "kimi-cli", "kimi-code"]),
            kimi_auth(),
            "kimi login",
        ),
        "opencode" => (opencode_installed(), opencode_auth(), "opencode auth login"),
        "opencode-go" => (
            opencode_installed(),
            description_check(super::opencode::auth_description_go()),
            "opencode auth login (select OpenCode Go)",
        ),
        "opencode-zen" => (
            opencode_installed(),
            description_check(super::opencode::auth_description_zen()),
            "opencode auth login opencode --method device",
        ),
        "omp" => (
            has_command("omp"),
            omp_auth(),
            "omp /login or configure a model API key",
        ),
        "pi" => (
            has_command("pi"),
            pi_auth(),
            "pi /login or configure a model API key",
        ),
        "gemini" => (
            has_command("gemini"),
            gemini_auth(),
            "gemini (select an authentication method)",
        ),
        "droid" => (
            has_command("droid"),
            droid_auth(),
            "droid or set FACTORY_API_KEY",
        ),
        "devin-local" => (
            has_command("devin")
                || app_installed(
                    "Devin",
                    &["devin-desktop.desktop", "devin-desktop-next.desktop"],
                ),
            devin_auth(),
            "devin auth login",
        ),
        // Cloud APIs have no separate local agent installation requirement.
        "devin-cloud" => (true, devin_auth(), "devin auth login or set DEVIN_API_KEY"),
        _ => (false, (CredentialStatus::Invalid, "Unknown agent"), ""),
    };
    let detail = if id == "devin-cloud" {
        format!(
            "Cloud integration requires no local agent installation. {}",
            check.1
        )
    } else if !installed {
        format!(
            "No executable or native application installation found. {}",
            check.1
        )
    } else {
        check.1.to_owned()
    };
    Availability {
        installed,
        credentials: check.0,
        detail,
        setup: setup.to_owned(),
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn env_path(name: &str, fallback: PathBuf) -> PathBuf {
    env_value(name).map(|v| expand_home(&v)).unwrap_or(fallback)
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        home()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(value)
    }
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn has_command(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        if executable(&dir.join(command)) {
            return true;
        }
        #[cfg(windows)]
        {
            std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .any(|ext| executable(&dir.join(format!("{command}{ext}"))))
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn has_any_command(commands: &[&str]) -> bool {
    commands.iter().any(|command| has_command(command))
}

fn app_installed(name: &str, desktops: &[&str]) -> bool {
    let mut roots = vec![dirs::data_local_dir().unwrap_or_else(|| home().join(".local/share"))];
    if let Some(value) = std::env::var_os("XDG_DATA_DIRS") {
        roots.extend(std::env::split_paths(&value));
    } else {
        roots.extend([
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ]);
    }
    if roots.iter().any(|root| {
        desktops
            .iter()
            .any(|desktop| desktop_installed(&root.join("applications").join(desktop)))
    }) {
        return true;
    }
    [PathBuf::from("/Applications"), home().join("Applications")]
        .iter()
        .any(|root| root.join(format!("{name}.app/Contents/MacOS")).is_dir())
}

fn desktop_installed(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let mut desktop_entry = false;
    for line in raw.lines().map(str::trim) {
        if line.starts_with('[') {
            desktop_entry = line == "[Desktop Entry]";
        }
        if !desktop_entry {
            continue;
        }
        if let Some(exec) = line.strip_prefix("Exec=") {
            let command = if let Some(quoted) = exec.strip_prefix('"') {
                quoted.split('"').next().unwrap_or("")
            } else {
                exec.split_whitespace().next().unwrap_or("")
            };
            return if command.contains('/') {
                executable(Path::new(command))
            } else {
                has_command(command)
            };
        }
    }
    false
}

fn opencode_installed() -> bool {
    has_any_command(&["opencode", "opencode-desktop"])
        || app_installed(
            "OpenCode",
            &["opencode.desktop", "opencode-desktop.desktop"],
        )
}

fn json_file(path: &Path) -> Result<Value, Check> {
    match fs::read(path) {
        Ok(raw) => serde_json::from_slice::<Value>(&raw).map_err(|_| INVALID),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(MISSING),
        Err(_) => Err(INVALID),
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn expiry_field(value: &Value, key: &str) -> Result<Option<f64>, Check> {
    match value.get(key) {
        None => Ok(None),
        Some(value) => coerce_unix_seconds(value)
            .filter(|expiry| expiry.is_finite())
            .map(Some)
            .ok_or(INVALID),
    }
}

fn safe_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn token_check(token: &str, expiry: Option<f64>, refreshable: bool, require_jwt: bool) -> Check {
    let token = normalize_bearer(token);
    if !safe_token(&token) {
        return INVALID;
    }
    let claims = if (require_jwt || token.starts_with("ey")) && token.split('.').count() == 3 {
        match jwt_claims(&token) {
            Ok(claims) if claims.is_object() => Some(claims),
            _ => return INVALID,
        }
    } else {
        None
    };
    let claim_exp = claims.as_ref().and_then(|c| c.get("exp"));
    if claim_exp.is_some_and(|value| !value.is_number()) {
        return INVALID;
    }
    let expires = expiry.or_else(|| claim_exp.and_then(Value::as_f64));
    if require_jwt && claims.is_some() && expires.is_none() {
        return INVALID;
    }
    if expires.is_some_and(|value| !value.is_finite()) {
        return INVALID;
    }
    if expires.is_some_and(|value| value <= now() + 30.0) {
        if refreshable { REFRESHABLE } else { EXPIRED }
    } else {
        READY
    }
}

fn description_check(description: Result<String, String>) -> Check {
    match description {
        Ok(_) => READY,
        Err(error) => {
            let error = error.to_ascii_lowercase();
            if error.contains("expired") || error.contains("no usable access or refresh") {
                EXPIRED
            } else if error.contains("invalid")
                || error.contains("failed to read")
                || error.contains("not valid")
                || error.contains("malformed")
            {
                INVALID
            } else {
                MISSING
            }
        }
    }
}

fn claude_auth() -> Check {
    let path = env_path(
        "CLAUDE_CREDENTIALS_PATH",
        home().join(".claude/.credentials.json"),
    );
    let value = match json_file(&path) {
        Ok(value) => value,
        Err(check) if check.0 == CredentialStatus::Missing => {
            let payload = if has_command("secret-tool") {
                Command::new("secret-tool")
                    .args(["lookup", "service", "Claude Code-credentials"])
                    .output()
                    .ok()
                    .filter(|out| out.status.success())
                    .map(|out| out.stdout)
            } else {
                None
            };
            match payload.filter(|raw| !raw.iter().all(u8::is_ascii_whitespace)) {
                Some(raw) => match serde_json::from_slice(&raw) {
                    Ok(value) => value,
                    Err(_) => return INVALID,
                },
                None => return claude_fallback(check),
            }
        }
        Err(check) => return claude_fallback(check),
    };
    let oauth = value.get("claudeAiOauth").unwrap_or(&value);
    let check = match field(oauth, "accessToken").or_else(|| field(oauth, "access_token")) {
        Some(token) => match expiry_field(oauth, "expiresAt") {
            Ok(expiry) => token_check(token, expiry, false, false),
            Err(check) => check,
        },
        None => MISSING,
    };
    claude_fallback(check)
}

fn claude_fallback(check: Check) -> Check {
    if check.0 == CredentialStatus::Ready {
        return check;
    }
    if super::antigravity::auth_description().is_ok() {
        (
            CredentialStatus::Ready,
            "Antigravity credentials can supply Claude/GPT pools; pool availability and server acceptance have not been checked",
        )
    } else {
        check
    }
}

fn codex_auth() -> Check {
    let path = env_path("CODEX_HOME", home().join(".codex")).join("auth.json");
    let value = match json_file(&path) {
        Ok(value) => value,
        Err(check) => return check,
    };
    match value
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
    {
        Some(token) => token_check(token, None, false, true),
        None => (
            CredentialStatus::Missing,
            "Codex subscription meters require a ChatGPT login; an API key alone is insufficient",
        ),
    }
}

fn grok_auth() -> Check {
    let path = env_path("GROK_HOME", home().join(".grok")).join("auth.json");
    let value = match json_file(&path) {
        Ok(value) => value,
        Err(check) => return check,
    };
    let Some(entries) = value.as_object() else {
        return INVALID;
    };
    let candidate = entries
        .iter()
        .filter(|(key, value)| {
            value.get("auth_mode").and_then(Value::as_str) == Some("oidc")
                && value.get("oidc_issuer").and_then(Value::as_str) == Some("https://auth.x.ai")
                && field(value, "oidc_client_id")
                    .is_some_and(|client| **key == format!("https://auth.x.ai::{client}"))
                && value
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.is_empty())
        })
        .min_by_key(|(key, _)| *key);
    let Some((_, entry)) = candidate else {
        return MISSING;
    };
    match expiry_field(entry, "expires_at") {
        Ok(expiry) => token_check(
            field(entry, "key").unwrap_or(""),
            expiry,
            field(entry, "refresh_token").is_some_and(safe_token),
            false,
        ),
        Err(check) => check,
    }
}

fn glm_auth() -> Check {
    if let Some(key) = env_value("ZAI_API_KEY") {
        return token_check(&key, None, false, false);
    }
    let path = env_path("CLAUDE_CONFIG_DIR", home().join(".claude")).join("settings.json");
    let value = match json_file(&path) {
        Ok(value) => value,
        Err(check) => return check,
    };
    let env = value.get("env").unwrap_or(&Value::Null);
    let base = field(env, "ANTHROPIC_BASE_URL").unwrap_or("");
    if !base.contains("z.ai") && !base.contains("bigmodel.cn") {
        return MISSING;
    }
    match field(env, "ANTHROPIC_AUTH_TOKEN") {
        Some(key) => token_check(key, None, false, false),
        None => MISSING,
    }
}

fn fx_auth() -> Check {
    for name in ["VERCEL_OIDC_TOKEN", "AI_GATEWAY_API_KEY"] {
        if let Some(token) = env_value(name) {
            return token_check(&token, None, false, name == "VERCEL_OIDC_TOKEN");
        }
    }
    let root = super::fx::fx_home();
    let mut failure = MISSING;
    match json_file(&root.join("auth.json")) {
        Ok(value) => {
            if let (Some(token), Some(expiry)) = (
                field(&value, "access_token"),
                value.get("expires_at_ms").and_then(Value::as_i64),
            ) {
                let check = token_check(token, Some(expiry as f64 / 1000.0 - 30.0), false, false);
                if check.0 == CredentialStatus::Ready {
                    return check;
                }
                failure = check;
            } else {
                failure = INVALID;
            }
        }
        Err(check) => {
            if check.0 != CredentialStatus::Missing {
                failure = check;
            }
        }
    }
    match fs::read_to_string(root.join("api-key")) {
        Ok(key) => token_check(key.trim(), None, false, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => failure,
        Err(_) => INVALID,
    }
}

fn commandcode_auth() -> Check {
    if let Some(key) = env_value("COMMANDCODE_API_KEY") {
        return token_check(&key, None, false, false);
    }
    let path = home().join(".commandcode/auth.json");
    let value = match json_file(&path) {
        Ok(value) => value,
        Err(check) => return check,
    };
    match field(&value, "apiKey").or_else(|| field(&value, "api_key")) {
        Some(key) => token_check(key, None, false, false),
        None => MISSING,
    }
}

fn kimi_auth() -> Check {
    let mut dirs = Vec::new();
    for name in ["KIMI_SHARE_DIR", "KIMI_CODE_HOME"] {
        if let Some(value) = env_value(name) {
            dirs.push(expand_home(&value).join("credentials"));
        }
    }
    dirs.extend([
        home().join(".kimi/credentials"),
        home().join(".kimi-code/credentials"),
    ]);
    let mut failure = MISSING;
    for dir in dirs {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                failure = INVALID;
                continue;
            }
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort_by_key(|path| path.file_name().is_none_or(|name| name != "kimi-code.json"));
        for path in files {
            let value = match json_file(&path) {
                Ok(value) => value,
                Err(check) => {
                    failure = check;
                    continue;
                }
            };
            let Some(access) = field(&value, "access_token") else {
                failure = INVALID;
                continue;
            };
            return token_check(
                access,
                Some(
                    value
                        .get("expires_at")
                        .and_then(coerce_unix_seconds)
                        .unwrap_or(0.0),
                ),
                field(&value, "refresh_token").is_some_and(safe_token),
                false,
            );
        }
    }
    failure
}

fn droid_auth() -> Check {
    if let Some(key) = env_value("FACTORY_API_KEY") {
        return token_check(&key, None, false, false);
    }
    let dev = env_value("FACTORY_ENV").as_deref() == Some("development");
    let root = env_path(
        "FACTORY_HOME",
        home().join(if dev { ".factory-dev" } else { ".factory" }),
    );
    let keyring_disabled = env_value("FACTORY_DISABLE_KEYRING").as_deref() == Some("1");
    droid_auth_at(&root, dev, keyring_disabled)
}

fn droid_auth_at(root: &Path, dev: bool, keyring_disabled: bool) -> Check {
    let mut failure = MISSING;
    if !keyring_disabled {
        let name = if cfg!(target_os = "macos") {
            "auth.v2.loginkeychain"
        } else {
            "auth.v2.keyring"
        };
        let encrypted = root.join(name);
        if encrypted.exists() {
            let base = if cfg!(target_os = "macos") {
                "auth-encryption-key-security-cli"
            } else {
                "auth-encryption-key"
            };
            let account = if dev {
                format!("{base}-dev")
            } else {
                base.to_owned()
            };
            let key = if cfg!(target_os = "macos") {
                Command::new("/usr/bin/security")
                    .args([
                        "find-generic-password",
                        "-s",
                        "Factory CLI",
                        "-a",
                        &account,
                        "-w",
                    ])
                    .output()
                    .ok()
            } else if has_command("secret-tool") {
                Command::new("secret-tool")
                    .args(["lookup", "service", "Factory CLI", "account", &account])
                    .output()
                    .ok()
            } else {
                None
            };
            match key.filter(|output| output.status.success() && !output.stdout.is_empty()) {
                Some(key) => match factory_decrypt(&encrypted, &key.stdout) {
                    Ok(value) => return factory_payload_auth(&value),
                    Err(check) => failure = check,
                },
                None => failure = UNKNOWN,
            }
        }
    }
    let encrypted = root.join("auth.v2.file");
    if encrypted.exists() {
        match fs::read(root.join("auth.v2.key")) {
            Ok(key) => match factory_decrypt(&encrypted, &key) {
                Ok(value) => return factory_payload_auth(&value),
                Err(check) => return check,
            },
            Err(_) => return UNKNOWN,
        }
    }
    failure
}

fn factory_payload_auth(value: &Value) -> Check {
    match (
        field(value, "access_token"),
        value.get("refresh_token").and_then(Value::as_str),
    ) {
        (Some(access), Some(refresh)) => {
            token_check(access, None, safe_token(refresh.trim()), true)
        }
        _ => INVALID,
    }
}

fn factory_decrypt(path: &Path, key: &[u8]) -> Result<Value, Check> {
    let ciphertext = fs::read_to_string(path).map_err(|_| UNKNOWN)?;
    factory_decrypt_payload(&ciphertext, key)
}

fn factory_decrypt_payload(ciphertext: &str, key: &[u8]) -> Result<Value, Check> {
    use aes_gcm::{AeadInPlace, AesGcm, KeyInit, Nonce, Tag, aead::consts::U16, aes::Aes256};
    use base64::{Engine, engine::general_purpose::STANDARD};
    let key = std::str::from_utf8(key).map_err(|_| UNKNOWN)?.trim();
    let mut parts = ciphertext.trim().split(':');
    let iv = parts.next().ok_or(UNKNOWN)?;
    let tag = parts.next().ok_or(UNKNOWN)?;
    let ciphertext = parts.next().ok_or(UNKNOWN)?;
    if parts.next().is_some() {
        return Err(UNKNOWN);
    }
    let mut key_bytes = [0u8; 48];
    let mut iv_bytes = [0u8; 24];
    let mut tag_bytes = [0u8; 24];
    let key_len = STANDARD
        .decode_slice(key, &mut key_bytes)
        .map_err(|_| UNKNOWN)?;
    let iv_len = STANDARD
        .decode_slice(iv, &mut iv_bytes)
        .map_err(|_| UNKNOWN)?;
    let tag_len = STANDARD
        .decode_slice(tag, &mut tag_bytes)
        .map_err(|_| UNKNOWN)?;
    if key_len != 32 || iv_len != 16 || tag_len != 16 {
        return Err(UNKNOWN);
    }
    let mut plaintext = STANDARD.decode(ciphertext).map_err(|_| UNKNOWN)?;
    // Factory v2 uses a 16-byte nonce, not AES-GCM's usual 12-byte nonce.
    let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key_bytes[..32]).map_err(|_| UNKNOWN)?;
    cipher
        .decrypt_in_place_detached(
            Nonce::<U16>::from_slice(&iv_bytes[..16]),
            b"",
            &mut plaintext,
            Tag::from_slice(&tag_bytes[..16]),
        )
        .map_err(|_| UNKNOWN)?;
    serde_json::from_slice(&plaintext).map_err(|_| INVALID)
}

fn devin_auth() -> Check {
    if let Some(key) = env_value("DEVIN_API_KEY") {
        return token_check(&key, None, false, false);
    }
    let path = super::devin_cloud::credentials_path();
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return MISSING,
        Err(_) => return INVALID,
    };
    let value = match toml::from_str::<toml::Value>(&raw) {
        Ok(value) => value,
        Err(_) => return INVALID,
    };
    let mut secrets = Vec::new();
    collect_devin_tokens(&value, &mut secrets);
    let token = secrets
        .iter()
        .find(|token| token.starts_with("cog_") || token.starts_with("apk_"))
        .or_else(|| secrets.iter().find(|token| token.starts_with("devin-")))
        .or_else(|| secrets.first());
    match token {
        Some(token) => token_check(token, None, false, false),
        None => MISSING,
    }
}

fn collect_devin_tokens<'a>(value: &'a toml::Value, tokens: &mut Vec<&'a str>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, item) in table {
        let key = key.to_ascii_lowercase();
        if (key.contains("token") || key.contains("api_key"))
            && let Some(token) = item.as_str().map(str::trim).filter(|v| !v.is_empty())
        {
            tokens.push(token);
        }
    }
    for item in table.values() {
        collect_devin_tokens(item, tokens);
    }
}

fn opencode_auth() -> Check {
    let override_value = std::env::var("OPENCODE_AUTH_CONTENT")
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    if override_value.is_none() {
        if let Some(native) = opencode_db_auth() {
            return opencode_or_env(native);
        }
    }
    let value = match override_value {
        Some(value) => value,
        None => match json_file(&super::opencode::auth_path()) {
            Ok(value) => value,
            Err(check) => return opencode_or_env(check),
        },
    };
    let Some(entries) = value.as_object() else {
        return INVALID;
    };
    opencode_or_env(best_check(entries.values().map(opencode_credential_check)))
}

fn opencode_credential_check(entry: &Value) -> Check {
    match field(entry, "type") {
        Some("api" | "key") => field(entry, "key")
            .map(|key| token_check(key, None, false, false))
            .unwrap_or(INVALID),
        Some("oauth") => match (field(entry, "access"), expiry_field(entry, "expires")) {
            (Some(token), Ok(Some(expiry))) => token_check(
                token,
                Some(expiry),
                field(entry, "refresh").is_some_and(safe_token),
                false,
            ),
            _ => INVALID,
        },
        Some("wellknown") => match (field(entry, "key"), field(entry, "token")) {
            (Some(_), Some(token)) => token_check(token, None, false, false),
            _ => INVALID,
        },
        _ => INVALID,
    }
}

fn opencode_db_auth() -> Option<Check> {
    let path = super::opencode::auth_path().with_file_name("opencode.db");
    if !path.exists() {
        return None;
    }
    let inspect = || -> rusqlite::Result<Option<Check>> {
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let supported: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='credential')",
            [],
            |row| row.get(0),
        )?;
        if !supported {
            return Ok(None);
        }
        let mut query = db.prepare(
            "SELECT value FROM credential WHERE integration_id IS NOT NULL AND (active IS NULL OR active=1)",
        )?;
        let checks = query.query_map([], |row| {
            let raw = row.get_ref(0)?.as_str()?;
            Ok(serde_json::from_str::<Value>(raw)
                .map(|entry| opencode_credential_check(&entry))
                .unwrap_or(INVALID))
        })?;
        Ok(Some(best_check(
            checks.map(|check| check.unwrap_or(UNKNOWN)),
        )))
    };
    inspect().unwrap_or(Some(UNKNOWN))
}

fn opencode_or_env(native: Check) -> Check {
    if native.0 == CredentialStatus::Ready {
        native
    } else {
        model_env_auth()
            .or_else(|| {
                env_value("OPENCODE_API_KEY").map(|key| token_check(&key, None, false, false))
            })
            .unwrap_or(native)
    }
}

fn best_check(checks: impl IntoIterator<Item = Check>) -> Check {
    let mut result = MISSING;
    for check in checks {
        if check.0 == CredentialStatus::Ready {
            return check;
        }
        if check.0 == CredentialStatus::Expired
            || (matches!(
                check.0,
                CredentialStatus::Invalid | CredentialStatus::Unknown
            ) && result.0 == CredentialStatus::Missing)
        {
            result = check;
        }
    }
    result
}

fn model_env_auth() -> Option<Check> {
    // Model API credentials, not search/tool-only keys.
    [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_OAUTH_TOKEN",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GROQ_API_KEY",
        "CEREBRAS_API_KEY",
        "XAI_API_KEY",
        "OPENROUTER_API_KEY",
        "MISTRAL_API_KEY",
        "ZAI_API_KEY",
        "AI_GATEWAY_API_KEY",
        "COPILOT_GITHUB_TOKEN",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "DEEPSEEK_API_KEY",
        "MINIMAX_API_KEY",
        "KIMI_API_KEY",
        "MOONSHOT_API_KEY",
    ]
    .iter()
    .filter_map(|name| env_value(name))
    .map(|key| token_check(&key, None, false, false))
    .find(|check| check.0 == CredentialStatus::Ready)
}

fn stored_model_auth(value: &Value, kind: &str) -> Check {
    match kind {
        "api_key" => match field(value, "key") {
            Some(key) if key.starts_with('!') => (
                CredentialStatus::Invalid,
                "Credential command is not executed during local inspection",
            ),
            Some(key) => match resolve_key_template(key, value.get("env")) {
                Some(key) => token_check(&key, None, false, false),
                None => MISSING,
            },
            None => INVALID,
        },
        "oauth" => match field(value, "access") {
            Some(access) if value.get("expires").and_then(Value::as_f64).is_some() => token_check(
                access,
                value.get("expires").and_then(coerce_unix_seconds),
                field(value, "refresh")
                    .is_some_and(|refresh| refresh != "__remote__" && safe_token(refresh)),
                false,
            ),
            _ => INVALID,
        },
        _ => INVALID,
    }
}

fn resolve_key_template(key: &str, env: Option<&Value>) -> Option<String> {
    let mut result = String::with_capacity(key.len());
    let mut chars = key.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('$' | '!') => result.push(chars.next()?),
            Some('{') => {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    name.push(ch);
                }
                if !closed {
                    result.push_str("${");
                    result.push_str(&name);
                    continue;
                }
                if valid_env_name(&name) {
                    result.push_str(&template_env(&name, env)?);
                } else {
                    result.push_str("${");
                    result.push_str(&name);
                    result.push('}');
                }
            }
            Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {
                let mut name = String::new();
                while chars
                    .peek()
                    .is_some_and(|ch| *ch == '_' || ch.is_ascii_alphanumeric())
                {
                    name.push(chars.next()?);
                }
                result.push_str(&template_env(&name, env)?);
            }
            _ => result.push('$'),
        }
    }
    Some(result)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn template_env(name: &str, values: Option<&Value>) -> Option<String> {
    values
        .and_then(|values| field(values, name))
        .map(str::to_owned)
        .or_else(|| env_value(name))
}

fn pi_auth() -> Check {
    let path = env_path("PI_CODING_AGENT_DIR", home().join(".pi/agent")).join("auth.json");
    let file = match json_file(&path) {
        Ok(value) => match value.as_object() {
            Some(entries) => best_check(
                entries
                    .values()
                    .map(|entry| stored_model_auth(entry, field(entry, "type").unwrap_or(""))),
            ),
            None => INVALID,
        },
        Err(check) => check,
    };
    if file.0 == CredentialStatus::Ready {
        file
    } else {
        model_env_auth().unwrap_or(file)
    }
}

fn omp_auth() -> Check {
    let profile = std::env::var("OMP_PROFILE")
        .ok()
        .or_else(|| std::env::var("PI_PROFILE").ok())
        .filter(|value| !value.trim().is_empty());
    if profile
        .as_ref()
        .is_some_and(|value| value != value.trim() || !valid_profile(value))
    {
        return INVALID;
    }
    let mut root = home().join(env_value("PI_CONFIG_DIR").unwrap_or_else(|| ".omp".into()));
    if let Some(profile) = profile.as_ref() {
        root = root.join("profiles").join(profile);
    }
    let default_agent = root.join("agent");
    let override_agent = if profile.is_none() {
        env_value("PI_CODING_AGENT_DIR").map(|value| expand_home(&value))
    } else {
        None
    };
    let agent = override_agent.clone().unwrap_or(default_agent.clone());
    let mut db_root = agent.clone();
    if override_agent.is_none() || agent == default_agent {
        if let Some(data) = env_value("XDG_DATA_HOME") {
            let mut xdg = PathBuf::from(data).join("omp");
            if let Some(profile) = profile.as_ref() {
                xdg = xdg.join("profiles").join(profile);
            }
            if xdg.is_dir() {
                db_root = xdg;
            }
        }
    }
    let check = omp_database_auth(&db_root.join("agent.db"));
    let check = if check.0 == CredentialStatus::Missing {
        // Older OMP releases used the same typed auth.json schema as Pi.
        match json_file(&agent.join("auth.json")) {
            Ok(value) => match value.as_object() {
                Some(entries) => best_check(
                    entries
                        .values()
                        .map(|entry| stored_model_auth(entry, field(entry, "type").unwrap_or(""))),
                ),
                None => INVALID,
            },
            Err(check) => check,
        }
    } else {
        check
    };
    if check.0 == CredentialStatus::Ready {
        check
    } else {
        model_env_auth().unwrap_or(check)
    }
}

fn valid_profile(value: &str) -> bool {
    let valid = !value.is_empty()
        && value.len() <= 64
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit())
        && value.bytes().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'.' | b'_' | b'-')
        })
        && !value.ends_with('.');
    let stem = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    valid
        && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

fn omp_database_auth(path: &Path) -> Check {
    if !path.exists() {
        return MISSING;
    }
    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(_) => return INVALID,
    };
    let mut statement = match connection.prepare("SELECT credential_type, data FROM auth_credentials WHERE disabled_cause IS NULL ORDER BY id ASC") { Ok(statement) => statement, Err(_) => return INVALID };
    let rows = match statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(_) => return INVALID,
    };
    best_check(rows.map(|row| match row {
        Ok((kind, raw)) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) => stored_model_auth(&value, &kind),
            Err(_) => INVALID,
        },
        Err(_) => INVALID,
    }))
}

fn gemini_auth() -> Check {
    let settings = match json_file(&home().join(".gemini/settings.json")) {
        Ok(value) => value,
        Err(check) if check.0 == CredentialStatus::Missing => Value::Null,
        Err(check) => return check,
    };
    let selected = settings
        .pointer("/security/auth/selectedType")
        .and_then(Value::as_str);
    if selected == Some("gemini-api-key")
        || (selected.is_none() && env_value("GEMINI_API_KEY").is_some())
    {
        return env_value("GEMINI_API_KEY")
            .map(|key| token_check(&key, None, false, false))
            .unwrap_or(MISSING);
    }
    if selected == Some("vertex-ai")
        || env_value("GOOGLE_GENAI_USE_VERTEXAI").as_deref() == Some("true")
    {
        if env_value("GOOGLE_CLOUD_PROJECT").is_none()
            || env_value("GOOGLE_CLOUD_LOCATION").is_none()
        {
            return (
                CredentialStatus::Missing,
                "Vertex AI requires a project and location as well as credentials",
            );
        }
        if let Some(key) = env_value("GOOGLE_API_KEY") {
            return token_check(&key, None, false, false);
        }
        let path = env_path(
            "GOOGLE_APPLICATION_CREDENTIALS",
            dirs::config_dir()
                .unwrap_or_else(|| home().join(".config"))
                .join("gcloud/application_default_credentials.json"),
        );
        return match json_file(&path) {
            Ok(value)
                if field(&value, "type") == Some("authorized_user")
                    && field(&value, "refresh_token").is_some_and(safe_token)
                    && field(&value, "client_id").is_some()
                    && field(&value, "client_secret").is_some() =>
            {
                READY
            }
            Ok(value)
                if field(&value, "type") == Some("service_account")
                    && field(&value, "client_email").is_some()
                    && field(&value, "private_key")
                        .is_some_and(|key| key.starts_with("-----BEGIN PRIVATE KEY-----")) =>
            {
                READY
            }
            Ok(_) => INVALID,
            Err(check) => check,
        };
    }
    let value = match json_file(&home().join(".gemini/oauth_creds.json")) {
        Ok(value) => value,
        Err(check) => return check,
    };
    let refreshable = field(&value, "refresh_token").is_some_and(safe_token);
    match field(&value, "access_token") {
        Some(token) => token_check(
            token,
            value.get("expiry_date").and_then(coerce_unix_seconds),
            refreshable,
            false,
        ),
        None if refreshable => REFRESHABLE,
        None => MISSING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    fn jwt(exp: f64) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::json!({"exp": exp}).to_string())
        )
    }

    #[test]
    fn readiness_requires_installation_and_usable_auth() {
        for installed in [false, true] {
            for credentials in [
                CredentialStatus::Ready,
                CredentialStatus::Missing,
                CredentialStatus::Expired,
                CredentialStatus::Invalid,
                CredentialStatus::Unknown,
            ] {
                let value = Availability {
                    installed,
                    credentials,
                    detail: String::new(),
                    setup: String::new(),
                };
                assert_eq!(
                    value.ready(),
                    installed && credentials == CredentialStatus::Ready
                );
            }
        }
    }

    #[test]
    fn malformed_and_expired_credentials_are_not_ready() {
        assert_eq!(
            token_check(" ", None, false, false).0,
            CredentialStatus::Invalid
        );
        assert_eq!(
            token_check("bad token", None, false, false).0,
            CredentialStatus::Invalid
        );
        assert_eq!(
            token_check("a.b.c", None, false, true).0,
            CredentialStatus::Invalid
        );
        assert_eq!(
            token_check(&jwt(now() - 60.0), None, false, true).0,
            CredentialStatus::Expired
        );
        assert_eq!(
            token_check(&jwt(now() - 60.0), None, true, true).0,
            CredentialStatus::Ready
        );
        assert_eq!(
            token_check(&jwt(now() + 3600.0), None, false, true).0,
            CredentialStatus::Ready
        );
        assert_eq!(
            token_check("opaque-local-token", None, false, true).0,
            CredentialStatus::Ready
        );
    }

    #[test]
    fn dynamic_credentials_do_not_execute_commands() {
        let value = serde_json::json!({"type": "api_key", "key": "!printf secret"});
        assert_eq!(
            stored_model_auth(&value, "api_key").0,
            CredentialStatus::Invalid
        );
        let env = serde_json::json!({"LOCAL_TEST_KEY": "key-value"});
        assert_eq!(
            resolve_key_template("prefix-${LOCAL_TEST_KEY}", Some(&env)).as_deref(),
            Some("prefix-key-value")
        );
        assert_eq!(
            resolve_key_template("$UNSET_USG_AVAILABILITY_TEST_KEY", None),
            None
        );
        assert_eq!(
            resolve_key_template("$$literal-$!", None).as_deref(),
            Some("$literal-!")
        );
    }

    #[test]
    fn oauth_refreshability_requires_a_real_refresh_token() {
        let expired =
            serde_json::json!({"access": "opaque-access", "refresh": "__remote__", "expires": 1});
        assert_eq!(
            stored_model_auth(&expired, "oauth").0,
            CredentialStatus::Expired
        );
        let refreshable = serde_json::json!({"access": "opaque-access", "refresh": "opaque-refresh", "expires": 1});
        assert_eq!(
            stored_model_auth(&refreshable, "oauth").0,
            CredentialStatus::Ready
        );
    }

    #[test]
    fn profile_names_cannot_escape_native_config_roots() {
        assert!(valid_profile("work-2"));
        assert!(!valid_profile("../work"));
        assert!(!valid_profile("work/other"));
        assert!(!valid_profile("con"));
        assert!(!valid_profile("work."));
    }

    #[test]
    fn factory_v2_native_nonce_and_detached_tag_decode() {
        use aes_gcm::{AeadInPlace, AesGcm, KeyInit, Nonce, aead::consts::U16, aes::Aes256};
        use base64::{Engine, engine::general_purpose::STANDARD};
        let key = [7u8; 32];
        let nonce = [9u8; 16];
        let payload =
            serde_json::json!({"access_token": "local-access", "refresh_token": "local-refresh"});
        let mut encrypted = serde_json::to_vec(&payload).unwrap();
        let cipher = AesGcm::<Aes256, U16>::new_from_slice(&key).unwrap();
        let tag = cipher
            .encrypt_in_place_detached(Nonce::<U16>::from_slice(&nonce), b"", &mut encrypted)
            .unwrap();
        let envelope = format!(
            "{}:{}:{}",
            STANDARD.encode(nonce),
            STANDARD.encode(tag),
            STANDARD.encode(&encrypted)
        );
        let key_encoded = STANDARD.encode(key);
        let decoded = factory_decrypt_payload(&envelope, key_encoded.as_bytes()).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(factory_payload_auth(&decoded).0, CredentialStatus::Ready);
        let root = std::env::temp_dir().join(format!(
            "usg-factory-auth-{}-{}",
            std::process::id(),
            now().to_bits()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("auth.v2.key"), &key_encoded).unwrap();
        fs::write(root.join("auth.v2.file"), &envelope).unwrap();
        assert_eq!(droid_auth_at(&root, false, true).0, CredentialStatus::Ready);
        fs::write(root.join("auth.v2.key"), "unreadable key").unwrap();
        assert_eq!(
            droid_auth_at(&root, false, true).0,
            CredentialStatus::Unknown
        );
        fs::remove_dir_all(root).unwrap();
        let wrong_key = STANDARD.encode([8u8; 32]);
        assert_eq!(
            factory_decrypt_payload(&envelope, wrong_key.as_bytes())
                .unwrap_err()
                .0,
            CredentialStatus::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn nonexecutable_files_and_old_logs_are_not_installations() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "usg-availability-{}-{}",
            std::process::id(),
            now().to_bits()
        ));
        fs::create_dir_all(&path).unwrap();
        let binary = path.join("agent");
        fs::write(&binary, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!executable(&binary));
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(executable(&binary));
        assert!(!executable(&path));
        let desktop = path.join("agent.desktop");
        fs::write(
            &desktop,
            format!("[Desktop Entry]\nExec={}/removed-agent\n", path.display()),
        )
        .unwrap();
        assert!(!desktop_installed(&desktop));
        fs::remove_dir_all(path).unwrap();
    }
}
