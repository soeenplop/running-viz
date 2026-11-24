//! Minimal Dropbox client: refresh-token auth, folder listing with cursor, download by id.

use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

#[derive(Clone)]
pub struct DropboxConfig {
    pub app_key: String,
    pub app_secret: String,
    pub refresh_token: String,
    pub folder: String,
}

pub struct Dropbox {
    http: Client,
    cfg: DropboxConfig,
    api: String,
    content: String,
    token: Mutex<Option<(String, DateTime<Utc>)>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    #[serde(rename = ".tag")]
    pub tag: String,
    pub id: Option<String>,
    pub name: String,
    pub path_display: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub cursor: String,
    pub has_more: bool,
}

impl Dropbox {
    pub fn new(http: Client, cfg: DropboxConfig) -> Self {
        Self::with_hosts(
            http,
            cfg,
            "https://api.dropboxapi.com",
            "https://content.dropboxapi.com",
        )
    }

    /// For tests: point the client at a mock server.
    pub fn with_hosts(http: Client, cfg: DropboxConfig, api: &str, content: &str) -> Self {
        Self {
            http,
            cfg,
            api: api.to_owned(),
            content: content.to_owned(),
            token: Mutex::new(None),
        }
    }

    pub fn app_secret(&self) -> &str {
        &self.cfg.app_secret
    }

    async fn access_token(&self) -> Result<String> {
        if let Some((t, exp)) = self.token.lock().expect("token lock").clone()
            && exp > Utc::now()
        {
            return Ok(t);
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: i64,
        }
        let res = self
            .http
            .post(format!("{}/oauth2/token", self.api))
            .basic_auth(&self.cfg.app_key, Some(&self.cfg.app_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.cfg.refresh_token.as_str()),
            ])
            .send()
            .await
            .context("dropbox token request failed")?;
        if !res.status().is_success() {
            bail!("dropbox token refresh answered {}", res.status());
        }
        let t: Token = res
            .json()
            .await
            .context("unexpected dropbox token response")?;
        let expires = Utc::now() + Duration::seconds(t.expires_in - 60);
        *self.token.lock().expect("token lock") = Some((t.access_token.clone(), expires));
        Ok(t.access_token)
    }

    async fn rpc<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        let res = self
            .http
            .post(format!("{}/2/{endpoint}", self.api))
            .bearer_auth(self.access_token().await?)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("dropbox {endpoint} failed"))?;
        let status = res.status();
        if !status.is_success() {
            let summary = res.text().await.unwrap_or_default();
            bail!(
                "dropbox {endpoint} answered {status}: {}",
                summary.chars().take(200).collect::<String>()
            );
        }
        res.json()
            .await
            .with_context(|| format!("unexpected dropbox {endpoint} response"))
    }

    /// First page of the configured folder (non-recursive).
    pub async fn list(&self) -> Result<Page> {
        self.rpc(
            "files/list_folder",
            json!({ "path": self.cfg.folder, "recursive": false }),
        )
        .await
    }

    /// Changes since `cursor`.
    pub async fn list_continue(&self, cursor: &str) -> Result<Page> {
        self.rpc("files/list_folder/continue", json!({ "cursor": cursor }))
            .await
    }

    /// Downloads a file by its Dropbox id (`id:…`, ASCII-safe for the API header).
    pub async fn download(&self, id: &str) -> Result<Vec<u8>> {
        let res = self
            .http
            .post(format!("{}/2/files/download", self.content))
            .bearer_auth(self.access_token().await?)
            .header("Dropbox-API-Arg", json!({ "path": id }).to_string())
            .send()
            .await
            .context("dropbox download failed")?;
        if !res.status().is_success() {
            bail!("dropbox download answered {}", res.status());
        }
        Ok(res.bytes().await.context("dropbox download body")?.to_vec())
    }
}

/// Verifies a webhook body against `X-Dropbox-Signature` (hex HMAC-SHA256 with the app secret).
pub fn signature_valid(app_secret: &str, body: &[u8], signature_hex: &str) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    let Ok(expected) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(app_secret.as_bytes()).expect("hmac key");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_signature() {
        use hmac::{Hmac, KeyInit, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"secret").unwrap();
        mac.update(b"{}");
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(signature_valid("secret", b"{}", &sig));
        assert!(!signature_valid("secret", b"{ }", &sig));
        assert!(!signature_valid("other", b"{}", &sig));
        assert!(!signature_valid("secret", b"{}", "zz"));
    }
}
