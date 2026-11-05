//! Grok Build usage — cli-chat-proxy billing (OIDC session from ~/.grok/auth.json).

use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::http::{HttpClient, HttpError};
use crate::jwt::normalize_bearer;
use crate::providers::types::{
    ProviderSnapshot, SnapshotStatus, coerce_unix_seconds, create_meter, meter_from_used_percent,
};

const USER_URL: &str = "https://cli-chat-proxy.grok.com/v1/user";
const BILLING_CREDITS_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
const AUTH_ISSUER: &str = "https://auth.x.ai";
const ID: &str = "grok";
const LABEL: &str = "Grok";

pub fn fetch(client: &HttpClient) -> ProviderSnapshot {
    match fetch_inner(client) {
        Ok(snap) => snap,
        Err(err) => ProviderSnapshot::fail(ID, LABEL, err.status, err.message),
    }
}

struct FetchErr {
    status: SnapshotStatus,
    message: String,
}
fn fetch_inner(client: &HttpClient) -> Result<ProviderSnapshot, FetchErr> {
    let auth = load_auth().map_err(|(status, message)| FetchErr { status, message })?;

    let bearer = format!("Bearer {}", auth.access_token);
    let headers_base = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("User-Agent", "usagenometer/0.1"),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
    ];

    let user: Value = client.get_json(USER_URL, &headers_base).map_err(map_http)?;

    let user_id = user
        .get("userId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FetchErr {
            status: SnapshotStatus::Error,
            message: "Grok /v1/user response missing userId.".into(),
        })?;

    let email = user
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or(auth.email);

    let credits_headers = [
        ("Accept", "application/json"),
        ("Authorization", bearer.as_str()),
        ("User-Agent", "usagenometer/0.1"),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
        ("x-userid", user_id),
    ];

    let credits: Value = client
        .get_json(BILLING_CREDITS_URL, &credits_headers)
        .map_err(map_http)?;

    let monthly = client.get_json(BILLING_URL, &credits_headers).ok();

    Ok(snapshot_from_billing(
        &credits,
        monthly.as_ref(),
        email.as_deref(),
    ))
}

fn map_http(e: HttpError) -> FetchErr {
    let status = if e.is_auth_error() {
        SnapshotStatus::Auth
    } else {
        SnapshotStatus::Error
    };
    let message = if e.is_auth_error() {
        "Grok session was rejected. Run grok login.".into()
    } else {
        e.to_string()
    };
    FetchErr { status, message }
}

struct Auth {
    access_token: String,
    email: Option<String>,
}

struct Credential {
    map_key: String,
    entry: Value,
}

fn load_auth() -> Result<Auth, (SnapshotStatus, String)> {
    let path = auth_path();
    if !path.exists() {
        return Err((
            SnapshotStatus::Auth,
            format!("Grok auth not found at {}. Run grok login.", path.display()),
        ));
    }
    let payload = read_auth(&path)?;
    let credential = select_credential(&payload).ok_or_else(|| {
        (
            SnapshotStatus::Auth,
            format!(
                "Grok auth at {} has no x.ai CLI OIDC session. Run grok login.",
                path.display()
            ),
        )
    })?;
    let credential = if is_expired(&credential.entry, now_secs()) {
        refresh_credential(&path, credential)?
    } else {
        credential
    };
    let access_token = credential
        .entry
        .get("key")
        .and_then(Value::as_str)
        .map(normalize_bearer)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            (
                SnapshotStatus::Auth,
                "Grok CLI session token is empty. Run grok login.".to_string(),
            )
        })?;
    Ok(Auth {
        access_token,
        email: credential
            .entry
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn read_auth(path: &Path) -> Result<Value, (SnapshotStatus, String)> {
    let raw = fs::read_to_string(path).map_err(|_| {
        (
            SnapshotStatus::Auth,
            format!("Failed to read Grok auth at {}.", path.display()),
        )
    })?;
    serde_json::from_str(&raw).map_err(|_| {
        (
            SnapshotStatus::Auth,
            format!("Grok auth at {} is not valid JSON.", path.display()),
        )
    })
}

fn select_credential(payload: &Value) -> Option<Credential> {
    let map = payload.as_object()?;
    map.iter()
        .filter(|(map_key, entry)| {
            let client_id = entry.get("oidc_client_id").and_then(Value::as_str);
            entry.get("auth_mode").and_then(Value::as_str) == Some("oidc")
                && entry.get("oidc_issuer").and_then(Value::as_str) == Some(AUTH_ISSUER)
                && client_id.is_some_and(|id| !id.is_empty())
                && entry
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|token| !token.is_empty())
                && client_id
                    .map(|id| *map_key == &format!("{AUTH_ISSUER}::{id}"))
                    .unwrap_or(false)
        })
        .min_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(key, entry)| Credential {
            map_key: key.clone(),
            entry: entry.clone(),
        })
}

fn is_expired(entry: &Value, now: f64) -> bool {
    entry
        .get("expires_at")
        .and_then(coerce_unix_seconds)
        .is_some_and(|expires| expires <= now + 30.0)
}

fn has_refresh_token(entry: &Value) -> bool {
    entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
}

fn refresh_credential(
    path: &Path,
    original: Credential,
) -> Result<Credential, (SnapshotStatus, String)> {
    refresh_credential_at(
        path,
        original,
        &format!("{AUTH_ISSUER}/.well-known/openid-configuration"),
        AUTH_ISSUER,
    )
}
fn refresh_credential_at(
    path: &Path,
    original: Credential,
    metadata_url: &str,
    trusted_issuer: &str,
) -> Result<Credential, (SnapshotStatus, String)> {
    let _lock = AuthFileLock::acquire(path).map_err(|message| (SnapshotStatus::Error, message))?;
    let mut payload = read_auth(path)?;
    let mut current = select_credential(&payload).ok_or_else(|| {
        (
            SnapshotStatus::Auth,
            "Grok session changed while waiting to refresh; run grok again.".into(),
        )
    })?;
    if current.map_key != original.map_key {
        return Err((
            SnapshotStatus::Auth,
            "Grok CLI OIDC account changed while refreshing; retry usage.".into(),
        ));
    }
    if current.entry != original.entry && is_expired(&current.entry, now_secs()) {
        return Err((
            SnapshotStatus::Auth,
            "Grok credentials changed during refresh; retry usage.".into(),
        ));
    }
    if !is_expired(&current.entry, now_secs()) {
        return Ok(current);
    }
    if !has_refresh_token(&current.entry) {
        return Err((
            SnapshotStatus::Auth,
            "Grok session is expired and has no refresh token. Run grok login.".into(),
        ));
    }
    let refresh_token = current.entry["refresh_token"].as_str().ok_or_else(|| {
        (
            SnapshotStatus::Auth,
            "Grok session is expired and has no refresh token. Run grok login.".into(),
        )
    })?;
    let client_id = current.entry["oidc_client_id"].as_str().ok_or_else(|| {
        (
            SnapshotStatus::Auth,
            "Grok OIDC session is missing its client ID. Run grok login.".into(),
        )
    })?;
    let refreshed = request_refresh_at(metadata_url, trusted_issuer, client_id, refresh_token)?;
    current.entry["key"] = refreshed.access_token;
    if let Some(refresh_token) = refreshed.refresh_token {
        current.entry["refresh_token"] = refresh_token;
    }
    current.entry["expires_at"] = refreshed.expires_at;
    let object = payload.as_object_mut().ok_or_else(|| {
        (
            SnapshotStatus::Auth,
            "Grok auth file has an unsupported structure.".into(),
        )
    })?;
    object.insert(current.map_key.clone(), current.entry.clone());
    write_auth_atomic(path, &payload).map_err(|message| (SnapshotStatus::Error, message))?;
    Ok(current)
}

struct Refreshed {
    access_token: Value,
    refresh_token: Option<Value>,
    expires_at: Value,
}

fn request_refresh_at(
    metadata_url: &str,
    trusted_issuer: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<Refreshed, (SnapshotStatus, String)> {
    let oidc_client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| {
            (
                SnapshotStatus::Error,
                "Could not initialize Grok's OIDC refresh client.".into(),
            )
        })?;
    let metadata: Value = oidc_client
        .get(metadata_url)
        .send()
        .map_err(|_| {
            (
                SnapshotStatus::Error,
                "Could not retrieve Grok's OIDC configuration for session refresh.".into(),
            )
        })?
        .error_for_status()
        .map_err(|_| {
            (
                SnapshotStatus::Error,
                "Could not retrieve Grok's OIDC configuration for session refresh.".into(),
            )
        })?
        .json()
        .map_err(|_| {
            (
                SnapshotStatus::Error,
                "Grok OIDC configuration was not valid JSON.".into(),
            )
        })?;
    let trusted_prefix = format!("{trusted_issuer}/");
    let token_endpoint = metadata
        .get("token_endpoint")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with(&trusted_prefix))
        .ok_or_else(|| {
            (
                SnapshotStatus::Error,
                "Grok OIDC metadata did not provide a trusted token endpoint.".into(),
            )
        })?;
    let mut form = HashMap::new();
    form.insert("grant_type", "refresh_token");
    form.insert("client_id", client_id);
    form.insert("refresh_token", refresh_token);
    let response = oidc_client
        .post(token_endpoint)
        .form(&form)
        .send()
        .map_err(|_| (SnapshotStatus::Error, "Grok session refresh failed.".into()))?;
    let status = response.status();
    if !status.is_success() {
        let auth_error = matches!(status.as_u16(), 401 | 403);
        return Err((
            if auth_error {
                SnapshotStatus::Auth
            } else {
                SnapshotStatus::Error
            },
            if auth_error {
                "Grok refresh token was rejected. Run grok login.".into()
            } else {
                "Grok session refresh failed.".into()
            },
        ));
    }
    let response: Value = response.json().map_err(|_| {
        (
            SnapshotStatus::Error,
            "Grok OIDC refresh response was not valid JSON.".into(),
        )
    })?;
    let access_token = response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(|token| Value::String(token.to_string()))
        .ok_or_else(|| {
            (
                SnapshotStatus::Error,
                "Grok OIDC refresh response contained no access token.".into(),
            )
        })?;
    let expires_at = refreshed_expiry(&response).ok_or_else(|| {
        (
            SnapshotStatus::Error,
            "Grok OIDC refresh response contained no usable expiry.".into(),
        )
    })?;
    Ok(Refreshed {
        access_token,
        refresh_token: response
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(|token| Value::String(token.to_string())),
        expires_at,
    })
}

fn refreshed_expiry(response: &Value) -> Option<Value> {
    let expires_in = response
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)?;
    let timestamp = (now_secs() as i64).checked_add(expires_in)?;
    let expiry = time::OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    let formatted = expiry
        .format(&time::format_description::well_known::Rfc3339)
        .ok()?;
    Some(Value::String(formatted))
}

fn write_auth_atomic(path: &Path, payload: &Value) -> Result<(), String> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Grok auth path has no valid filename.".to_string())?;
    let metadata = fs::metadata(path).map_err(|_| {
        format!(
            "Could not inspect Grok auth permissions at {}.",
            path.display()
        )
    })?;
    let mut temporary = None;
    for attempt in 0..8 {
        let candidate = parent.join(format!(".{name}.usg-{}-{attempt}.tmp", std::process::id()));
        match create_private_new(&candidate) {
            Ok(Some(file)) => {
                temporary = Some((candidate, file));
                break;
            }
            Ok(None) => continue,
            Err(_) => break,
        }
    }
    let (temporary_path, mut file) = temporary
        .ok_or_else(|| "Could not create a private Grok auth temporary file.".to_string())?;
    let result = (|| {
        fs::set_permissions(&temporary_path, metadata.permissions())
            .map_err(|_| "Could not preserve Grok auth file permissions.".to_string())?;
        serde_json::to_writer(&mut file, payload)
            .map_err(|_| "Could not serialize refreshed Grok credentials.".to_string())?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|_| "Could not persist refreshed Grok credentials.".to_string())?;
        fs::rename(&temporary_path, path)
            .map_err(|_| "Could not atomically replace Grok auth credentials.".to_string())?;
        sync_parent(parent);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary_path);
    }
    result
}

fn sync_parent(parent: &Path) {
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

/// Grok's native lock is a `PID:Unix-seconds` file; flock only serializes our stale-lock checks.
struct AuthFileLock {
    path: PathBuf,
    file: fs::File,
    marker: String,
}

impl AuthFileLock {
    fn acquire(auth_path: &Path) -> Result<Self, String> {
        let path = auth_path.with_file_name(format!(
            "{}.lock",
            auth_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("auth.json")
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match create_native_lock(&path) {
                Ok(Some((file, marker))) => return Ok(Self { path, file, marker }),
                Ok(None) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("Could not create Grok auth lock file.".into()),
            }
            let file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("Could not open Grok auth lock file.".into()),
            };
            lock_file_mutex(&file)?;
            if !same_lock_file(&path, &file) {
                unlock_file_mutex(&file);
                continue;
            }
            let contents = fs::read_to_string(&path)
                .map_err(|_| "Could not read Grok auth lock metadata.".to_string())?;
            match parse_native_lock(&contents) {
                Some((pid, _)) if pid_is_alive(pid) => {
                    unlock_file_mutex(&file);
                    if std::time::Instant::now() >= deadline {
                        return Err("Timed out waiting for the active Grok auth lock.".into());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Some((_, timestamp)) if timestamp <= now_secs() as u64 => {
                    if fs::read_to_string(&path).ok().as_deref() != Some(contents.as_str())
                        || !same_lock_file(&path, &file)
                    {
                        unlock_file_mutex(&file);
                        continue;
                    }
                    fs::remove_file(&path)
                        .map_err(|_| "Could not reclaim stale Grok auth lock.".to_string())?;
                    unlock_file_mutex(&file);
                }
                _ => {
                    unlock_file_mutex(&file);
                    return Err("Grok auth lock is malformed or has an unverified owner.".into());
                }
            }
        }
    }
}

impl Drop for AuthFileLock {
    fn drop(&mut self) {
        if same_lock_file(&self.path, &self.file)
            && fs::read_to_string(&self.path).ok().as_deref() == Some(self.marker.as_str())
        {
            let _ = fs::remove_file(&self.path);
        }
        unlock_file_mutex(&self.file);
    }
}

fn create_native_lock(path: &Path) -> std::io::Result<Option<(fs::File, String)>> {
    use std::io::Write;
    let marker = format!("{}:{}", std::process::id(), now_secs() as u64);
    let Some(mut file) = create_private_new(path)? else {
        return Ok(None);
    };
    if let Err(error) = file
        .write_all(marker.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    if let Err(error) = lock_file_mutex(&file) {
        let _ = fs::remove_file(path);
        return Err(std::io::Error::other(error));
    }
    Ok(Some((file, marker)))
}

#[cfg(unix)]
fn create_private_new(path: &Path) -> std::io::Result<Option<fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn create_private_new(path: &Path) -> std::io::Result<Option<fs::File>> {
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_native_lock(contents: &str) -> Option<(u32, u64)> {
    let (pid, timestamp) = contents.trim().split_once(':')?;
    let pid: u32 = pid.parse().ok()?;
    let timestamp: u64 = timestamp.parse().ok()?;
    if pid == 0 || pid > i32::MAX as u32 || timestamp == 0 {
        return None;
    }
    Some((pid, timestamp))
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    if unsafe { kill(pid as i32, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(3)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn lock_file_mutex(file: &fs::File) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("Timed out coordinating Grok auth lock access.".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
fn lock_file_mutex(_file: &fs::File) -> Result<(), String> {
    Err("Safe Grok auth refresh is not supported on this platform.".into())
}

#[cfg(unix)]
fn unlock_file_mutex(file: &fs::File) {
    use std::os::fd::AsRawFd;
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_UN: i32 = 8;
    let _ = unsafe { flock(file.as_raw_fd(), LOCK_UN) };
}

#[cfg(not(unix))]
fn unlock_file_mutex(_file: &fs::File) {}

#[cfg(unix)]
fn same_lock_file(path: &Path, file: &fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    path.metadata()
        .ok()
        .zip(file.metadata().ok())
        .is_some_and(|(path, file)| path.dev() == file.dev() && path.ino() == file.ino())
}

#[cfg(not(unix))]
fn same_lock_file(_path: &Path, _file: &fs::File) -> bool {
    false
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn grok_home() -> PathBuf {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|home| home.join(".grok")))
        .unwrap_or_else(|| PathBuf::from(".grok"))
}

fn auth_path_for(home: PathBuf) -> PathBuf {
    home.join("auth.json")
}

fn auth_path() -> PathBuf {
    auth_path_for(grok_home())
}

pub(crate) fn auth_description() -> Result<String, String> {
    auth_description_at(&auth_path())
}

fn auth_description_at(path: &Path) -> Result<String, String> {
    let payload = read_auth(path).map_err(|(_, message)| message)?;
    let credential = select_credential(&payload)
        .ok_or_else(|| format!("No Grok x.ai OIDC session found at {}.", path.display()))?;
    let expiry = credential
        .entry
        .get("expires_at")
        .and_then(coerce_unix_seconds);
    let expired = expiry.is_some_and(|value| value <= now_secs() + 30.0);
    let refreshable = has_refresh_token(&credential.entry);
    let state = if expired && refreshable {
        "expired; refreshable"
    } else if expired {
        "expired"
    } else if expiry.is_some() {
        "not expired"
    } else {
        "expiry unknown"
    };
    let expiry_text = expiry
        .map(|value| format!("expires at Unix {value:.0}"))
        .unwrap_or_else(|| "expiry unknown".to_string());
    Ok(format!(
        "Grok OIDC credentials: {} ({state}; {expiry_text}); refresh token {}.",
        path.display(),
        if refreshable {
            "available"
        } else {
            "unavailable"
        }
    ))
}

pub fn snapshot_from_billing(
    credits: &Value,
    monthly: Option<&Value>,
    email: Option<&str>,
) -> ProviderSnapshot {
    let config = credits.get("config").unwrap_or(credits);
    let period_end = config
        .pointer("/currentPeriod/end")
        .or_else(|| config.get("billingPeriodEnd"))
        .and_then(coerce_unix_seconds);

    let mut meters = Vec::new();

    if let Some(percent) = config.get("creditUsagePercent").and_then(|v| v.as_f64()) {
        meters.push(meter_from_used_percent(
            "weekly_credits",
            "Weekly credits",
            percent,
            period_end,
        ));
    }

    if let Some(products) = config.get("productUsage").and_then(|v| v.as_array()) {
        for product in products {
            let name = product
                .get("product")
                .and_then(|v| v.as_str())
                .unwrap_or("Product");
            let Some(percent) = product.get("usagePercent").and_then(|v| v.as_f64()) else {
                continue;
            };
            meters.push(meter_from_used_percent(
                &format!("product_{}", name.to_lowercase()),
                &humanize_product(name),
                percent,
                period_end,
            ));
        }
    }

    let on_cap = config
        .pointer("/onDemandCap/val")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let on_used = config
        .pointer("/onDemandUsed/val")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if on_cap > 0.0 {
        meters.push(create_meter(
            "on_demand",
            "On-demand",
            Some(on_used / on_cap),
            Some((1.0 - on_used / on_cap).max(0.0)),
            Some(on_used),
            Some((on_cap - on_used).max(0.0)),
            Some(on_cap),
            "credits",
            period_end,
            None,
            None,
        ));
    }

    // Unified-billing fallback: monthly included budget
    if meters.is_empty()
        && let Some(monthly) = monthly
    {
        let mcfg = monthly.get("config").unwrap_or(monthly);
        let used = mcfg.pointer("/used/val").and_then(|v| v.as_f64());
        let limit = mcfg.pointer("/monthlyLimit/val").and_then(|v| v.as_f64());
        let reset_at = mcfg.get("billingPeriodEnd").and_then(coerce_unix_seconds);
        if let (Some(u), Some(l)) = (used, limit) {
            if l > 0.0 {
                meters.push(create_meter(
                    "monthly",
                    "Monthly",
                    Some(u / l),
                    Some((1.0 - u / l).max(0.0)),
                    Some(u),
                    Some((l - u).max(0.0)),
                    Some(l),
                    "credits",
                    reset_at,
                    None,
                    None,
                ));
            } else if u > 0.0 {
                // Limit omitted/zero on some unified plans — still surface used credits.
                meters.push(create_meter(
                    "monthly_used",
                    "Monthly used",
                    None,
                    None,
                    Some(u),
                    None,
                    None,
                    "credits",
                    reset_at,
                    None,
                    None,
                ));
            }
        }
    }

    let plan = credits
        .get("subscriptionTier")
        .or_else(|| credits.get("planName"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    ProviderSnapshot {
        id: ID.into(),
        label: LABEL.into(),
        status: SnapshotStatus::Ok,
        error: if meters.is_empty() {
            Some("Logged in, but this Grok plan did not expose percentage quotas.".into())
        } else {
            None
        },
        account: email.map(str::to_string),
        plan,
        meters,
        stale_age_secs: None,
    }
}

fn humanize_product(name: &str) -> String {
    match name {
        "GrokBuild" => "Grok Build".into(),
        "GrokChat" => "Grok Chat".into(),
        "Api" => "API".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_auth() -> Value {
        serde_json::json!({
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "auth_mode": "oidc",
                "key": "access-token",
                "refresh_token": "refresh-token",
                "expires_at": "2026-10-07T20:04:03.322291571Z",
                "email": "local@example.invalid",
                "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828",
                "oidc_issuer": "https://auth.x.ai",
                "user_id": "native-user"
            }
        })
    }

    #[test]
    fn selects_native_oidc_principal_not_api_key_or_other_oidc() {
        let mut payload = native_auth();
        let map = payload.as_object_mut().unwrap();
        map.insert(
            "api-key-entry".into(),
            serde_json::json!({"auth_mode": "api_key", "key": "api-secret"}),
        );
        map.insert(
            "https://other.example::client".into(),
            serde_json::json!({
                "auth_mode": "oidc",
                "key": "other-token",
                "oidc_issuer": "https://other.example",
                "oidc_client_id": "client"
            }),
        );
        let credential = select_credential(&payload).unwrap();
        assert_eq!(credential.entry["key"], "access-token");
        assert_eq!(credential.entry["oidc_issuer"], "https://auth.x.ai");
    }

    #[test]
    fn native_expired_session_is_refreshable() {
        let payload = native_auth();
        let credential = select_credential(&payload).unwrap();
        assert!(is_expired(&credential.entry, 1_791_405_000.0));
        assert!(has_refresh_token(&credential.entry));
    }
    #[test]
    fn auth_description_does_not_expose_credentials_or_account_data() {
        let temp = std::env::temp_dir().join(format!(
            "usg-grok-description-{}-{}",
            std::process::id(),
            now_secs() as u64
        ));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("auth.json");
        fs::write(&path, serde_json::to_vec(&native_auth()).unwrap()).unwrap();
        let description = auth_description_at(&path).unwrap();
        assert!(!description.contains("local@example.invalid"));
        assert!(!description.contains("access-token"));
        assert!(!description.contains("refresh-token"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn expired_session_without_refresh_token_is_not_refreshable() {
        let mut payload = native_auth();
        let entry = payload
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        entry.as_object_mut().unwrap().remove("refresh_token");
        let credential = select_credential(&payload).unwrap();
        assert!(is_expired(&credential.entry, 1_791_405_000.0));
        assert!(!has_refresh_token(&credential.entry));
    }
    #[test]
    fn unrefreshable_expired_auth_returns_auth_error_without_network() {
        let temp = std::env::temp_dir().join(format!(
            "usg-grok-unrefreshable-{}-{}",
            std::process::id(),
            now_secs() as u64
        ));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("auth.json");
        let mut payload = native_auth();
        payload
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        let error = match refresh_credential_at(
            &path,
            select_credential(&payload).unwrap(),
            "http://127.0.0.1:1/.well-known/openid-configuration",
            "http://127.0.0.1:1",
        ) {
            Err(error) => error,
            Ok(_) => panic!("expired credentials without a refresh token must fail"),
        };
        assert!(error.1.contains("no refresh token"));
        assert!(!path.with_file_name("auth.json.lock").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn grok_home_auth_file_path() {
        assert_eq!(
            auth_path_for(PathBuf::from("/tmp/grok-home")),
            PathBuf::from("/tmp/grok-home/auth.json")
        );
    }
    #[test]
    fn refresh_persists_rotated_native_credentials_without_touching_other_entries() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        fn read_request(stream: &mut TcpStream) -> String {
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                let request = String::from_utf8_lossy(&bytes);
                if let Some(end_headers) = request.find("\r\n\r\n") {
                    let headers = &request[..end_headers];
                    if request.starts_with("GET ")
                        || headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .is_some_and(|length| bytes.len() >= end_headers + 4 + length)
                    {
                        break;
                    }
                }
            }
            String::from_utf8(bytes).unwrap()
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let metadata_url = format!("{issuer}/.well-known/openid-configuration");
        let token_endpoint = format!("{issuer}/oauth2/token");
        let server_endpoint = token_endpoint.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                let (status, body) = if request.starts_with("GET ") {
                    (
                        "200 OK",
                        format!(r#"{{"token_endpoint":"{server_endpoint}"}}"#),
                    )
                } else {
                    assert!(request.contains("grant_type=refresh_token"));
                    assert!(request.contains("client_id=local-client"));
                    assert!(request.contains("refresh_token=old-refresh-token"));
                    (
                        "200 OK",
                        r#"{"access_token":"new-access-token","refresh_token":"rotated-refresh-token","expires_in":7200}"#.to_string(),
                    )
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });

        let temp = std::env::temp_dir().join(format!(
            "usg-grok-auth-{}-{}",
            std::process::id(),
            now_secs() as u64
        ));
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("auth.json");
        let mut payload = native_auth();
        let key = format!("{AUTH_ISSUER}::b1a00492-073a-47ea-816f-4c329264a828");
        let mut entry = payload.as_object().unwrap().get(&key).unwrap().clone();
        entry["oidc_client_id"] = Value::String("local-client".into());
        entry["key"] = Value::String("expired-access-token".into());
        entry["refresh_token"] = Value::String("old-refresh-token".into());
        entry["expires_at"] = Value::String("2020-01-01T00:00:00Z".into());
        let map = payload.as_object_mut().unwrap();
        map.remove(&key);
        map.insert(format!("{AUTH_ISSUER}::local-client"), entry);
        map.insert(
            "unrelated-entry".into(),
            serde_json::json!({"preserve": true}),
        );
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let original = select_credential(&payload).unwrap();
        let refreshed = refresh_credential_at(&path, original, &metadata_url, &issuer).unwrap();
        server.join().unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let saved_entry = &saved["https://auth.x.ai::local-client"];
        assert_eq!(saved_entry["key"], "new-access-token");
        assert_eq!(saved_entry["refresh_token"], "rotated-refresh-token");
        assert!(
            coerce_unix_seconds(&saved_entry["expires_at"])
                .is_some_and(|expiry| expiry > now_secs())
        );
        assert_eq!(saved_entry["email"], "local@example.invalid");
        assert_eq!(saved["unrelated-entry"]["preserve"], true);
        assert_eq!(refreshed.entry["key"], "new-access-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(!path.with_file_name("auth.json.lock").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn weekly_credits_meter() {
        let credits = serde_json::json!({
            "subscriptionTier": "SuperGrok",
            "config": {
                "creditUsagePercent": 34.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-05T01:12:18.000Z"
                },
                "productUsage": [
                    {"product": "GrokBuild", "usagePercent": 45.0},
                    {"product": "GrokChat"}
                ],
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0}
            }
        });
        let snap = snapshot_from_billing(&credits, None, Some("g@example.com"));
        assert_eq!(snap.meters.len(), 2);
        assert_eq!(snap.meters[0].id, "weekly_credits");
        assert!((snap.meters[0].percent.unwrap() - 0.34).abs() < 1e-9);
        assert_eq!(snap.meters[1].id, "product_grokbuild");
    }

    #[test]
    fn monthly_fallback() {
        let credits = serde_json::json!({
            "config": {
                "isUnifiedBillingUser": true,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-08-05T00:00:00Z"},
                "onDemandCap": {"val": 0},
                "onDemandUsed": {"val": 0}
            }
        });
        let monthly = serde_json::json!({
            "config": {
                "monthlyLimit": {"val": 150000},
                "used": {"val": 75000},
                "billingPeriodEnd": "2026-08-01T00:00:00Z"
            }
        });
        let snap = snapshot_from_billing(&credits, Some(&monthly), None);
        assert_eq!(snap.meters.len(), 1);
        assert_eq!(snap.meters[0].id, "monthly");
        assert!((snap.meters[0].percent.unwrap() - 0.5).abs() < 1e-9);
    }
}
