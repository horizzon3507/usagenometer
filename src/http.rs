//! Minimal blocking HTTP helpers (reqwest).

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("{message}")]
    Status {
        status: u16,
        message: String,
        body: Option<Value>,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl HttpError {
    pub fn is_auth_error(&self) -> bool {
        matches!(self, Self::Status { status: 401 | 403, .. })
    }
}

#[derive(Clone)]
pub struct HttpClient {
    inner: Client,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let inner = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("usagenometer/0.1")
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { inner })
    }

    pub fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        headers: &[(&str, &str)],
    ) -> Result<T, HttpError> {
        let response = self
            .inner
            .get(url)
            .headers(build_headers(headers)?)
            .send()
            .map_err(|e| HttpError::Other(e.into()))?;
        parse_json(response)
    }

    pub fn post_json<T: DeserializeOwned>(
        &self,
        url: &str,
        body: &Value,
        headers: &[(&str, &str)],
    ) -> Result<T, HttpError> {
        let response = self
            .inner
            .post(url)
            .headers(build_headers(headers)?)
            .json(body)
            .send()
            .map_err(|e| HttpError::Other(e.into()))?;
        parse_json(response)
    }

    pub fn post_form<T: DeserializeOwned>(
        &self,
        url: &str,
        form: &HashMap<&str, &str>,
    ) -> Result<T, HttpError> {
        let response = self
            .inner
            .post(url)
            .form(form)
            .send()
            .map_err(|e| HttpError::Other(e.into()))?;
        parse_json(response)
    }
}

fn build_headers(headers: &[(&str, &str)]) -> Result<HeaderMap, HttpError> {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| HttpError::Other(anyhow!("bad header name {k}: {e}")))?;
        let value = HeaderValue::from_str(v)
            .map_err(|e| HttpError::Other(anyhow!("bad header value for {k}: {e}")))?;
        map.append(name, value);
    }
    Ok(map)
}

fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T, HttpError> {
    let status = response.status().as_u16();
    let text = response
        .text()
        .map_err(|e| HttpError::Other(e.into()))?;
    let payload: Option<Value> = if text.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(&text).map_err(|e| {
            HttpError::Other(anyhow!("invalid JSON: {e}"))
        })?)
    };

    if !(200..300).contains(&status) {
        let message = payload
            .as_ref()
            .and_then(extract_error_message)
            .unwrap_or_else(|| format!("Request failed with HTTP {status}."));
        return Err(HttpError::Status {
            status,
            message,
            body: payload,
        });
    }

    let value = payload.unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|e| HttpError::Other(anyhow!("JSON shape: {e}")))
}

fn extract_error_message(payload: &Value) -> Option<String> {
    for key in ["message", "error", "detail", "title"] {
        if let Some(s) = payload.get(key).and_then(|v| {
            if let Some(s) = v.as_str() {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
            if let Some(obj) = v.as_object() {
                for nested in ["message", "detail", "title", "code", "type"] {
                    if let Some(s) = obj.get(nested).and_then(|x| x.as_str()) {
                        let t = s.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
            }
            None
        }) {
            return Some(s);
        }
    }
    None
}
