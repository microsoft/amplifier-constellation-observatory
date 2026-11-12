use anyhow::{Context, Result, ensure};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bootstrap {
    pub schema: u32,
    pub observatory_url: String,
    pub observatory_id: Uuid,
    pub observatory_name: String,
    pub pairing_id: Uuid,
    pub expires_at: u64,
    #[serde(default)]
    pub ca_pem: Option<String>,
}
impl Bootstrap {
    pub fn validate(&self) -> Result<Url> {
        let url = Url::parse(&self.observatory_url)?;
        ensure!(
            self.schema == 1
                && url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "Use the unchanged setup download from your Observatory"
        );
        ensure!(
            !self.observatory_name.trim().is_empty()
                && self.observatory_name.len() <= 160
                && !self.observatory_name.chars().any(char::is_control),
            "Invalid Observatory name"
        );
        ensure!(
            self.ca_pem.as_ref().is_none_or(|p| p.len() <= 65536),
            "Certificate bundle is too large"
        );
        Ok(url)
    }
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Receipt {
    pub schema: u32,
    pub pairing_id: Uuid,
    pub observatory_id: Uuid,
    pub host_id: Uuid,
    pub machine_id: Uuid,
    pub access: Vec<String>,
    pub heartbeat_seconds: u64,
}
impl Receipt {
    pub fn validate(&self, bootstrap: &Bootstrap, machine: Uuid) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.pairing_id == bootstrap.pairing_id
                && self.observatory_id == bootstrap.observatory_id
                && self.machine_id == machine
                && self.access == ["observe"]
                && (5..=300).contains(&self.heartbeat_seconds),
            "Pairing reply did not match this computer and Observatory"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct HeartbeatReceipt {
    pub schema: u32,
    pub observatory_id: Uuid,
    pub host_id: Uuid,
    pub state: String,
    pub access: Vec<String>,
    pub heartbeat_seconds: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub request_id: Uuid,
    pub code: String,
    pub name: String,
    pub startup: bool,
}
#[derive(Serialize)]
struct Redeem<'a> {
    schema: u32,
    observatory_id: Uuid,
    code: &'a str,
    request_id: Uuid,
    machine_id: Uuid,
    machine_credential: &'a str,
    name: &'a str,
    os: &'static str,
    arch: &'static str,
    helper_version: &'static str,
}
#[derive(Serialize)]
struct Heartbeat<'a> {
    schema: u32,
    machine_id: Uuid,
    name: &'a str,
    os: &'static str,
    arch: &'static str,
    helper_version: &'static str,
}
#[derive(Debug)]
pub struct RemoteError {
    pub status: Option<u16>,
    pub message: String,
}
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RemoteError {}
#[derive(Clone)]
pub struct Remote {
    client: Client,
    base: Url,
    pub bootstrap: Bootstrap,
}
impl Remote {
    pub fn new(bootstrap: Bootstrap) -> Result<Self> {
        let base = bootstrap.validate()?;
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20));
        if let Some(pem) = &bootstrap.ca_pem {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(pem.as_bytes())
                    .context("The Observatory certificate is invalid")?,
            );
        }
        Ok(Self {
            client: builder.build()?,
            base,
            bootstrap,
        })
    }
    async fn post<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &T,
        secret: Option<&str>,
    ) -> std::result::Result<R, RemoteError> {
        let url = self.base.join(path).map_err(|_| RemoteError {
            status: None,
            message: "Invalid Observatory address".into(),
        })?;
        let mut request = self.client.post(url).json(body);
        if let Some(secret) = secret {
            request = request.bearer_auth(secret)
        }
        let mut response = request.send().await.map_err(|_| RemoteError {
            status: None,
            message: "Cannot reach Observatory. Your saved connection has been kept.".into(),
        })?;
        let status = response.status();
        if !status.is_success() {
            // Never echo server bodies: they can contain reflected credentials or proxy HTML.
            let message = match status {
                StatusCode::FORBIDDEN => {
                    "Observatory refused this connection. Check access in Observatory before pairing again."
                }
                StatusCode::UNAUTHORIZED => {
                    "The pairing code or saved credential was not accepted."
                }
                StatusCode::GONE => {
                    "This pairing code has expired or was cancelled. Get a new setup download from Observatory."
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    "Too many pairing attempts. Create a new pairing in Observatory."
                }
                StatusCode::CONFLICT => {
                    "This pairing conflicts with an existing connection. Check Observatory before trying again."
                }
                StatusCode::NOT_FOUND => {
                    "This pairing or computer is no longer available in Observatory."
                }
                _ => "Observatory could not confirm the request. Its outcome is still unknown.",
            };
            return Err(RemoteError {
                status: Some(status.as_u16()),
                message: message.into(),
            });
        }
        if response.content_length().is_some_and(|n| n > 32768) {
            return Err(RemoteError {
                status: None,
                message: "Observatory returned an oversized reply. Nothing was replaced.".into(),
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| RemoteError {
            status: None,
            message: "The reply was interrupted. The saved request can be checked again.".into(),
        })? {
            if bytes.len() + chunk.len() > 32768 {
                return Err(RemoteError {
                    status: None,
                    message: "Observatory returned an oversized reply.".into(),
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| RemoteError {
            status: None,
            message: "Observatory returned an unrecognized reply. The request outcome is unknown."
                .into(),
        })
    }
    pub async fn redeem(
        &self,
        machine_id: Uuid,
        secret: &str,
        pending: &Pending,
    ) -> std::result::Result<Receipt, RemoteError> {
        let body = Redeem {
            schema: 1,
            observatory_id: self.bootstrap.observatory_id,
            code: &pending.code,
            request_id: pending.request_id,
            machine_id,
            machine_credential: secret,
            name: &pending.name,
            os: crate::platform::os(),
            arch: crate::platform::arch(),
            helper_version: env!("CARGO_PKG_VERSION"),
        };
        let receipt: Receipt = self
            .post(
                &format!(
                    "api/v1/machine-pairings/{}/redeem",
                    self.bootstrap.pairing_id
                ),
                &body,
                None,
            )
            .await?;
        receipt
            .validate(&self.bootstrap, machine_id)
            .map_err(|e| RemoteError {
                status: None,
                message: e.to_string(),
            })?;
        Ok(receipt)
    }
    pub async fn heartbeat(
        &self,
        receipt: &Receipt,
        secret: &str,
        name: &str,
    ) -> std::result::Result<HeartbeatReceipt, RemoteError> {
        let body = Heartbeat {
            schema: 1,
            machine_id: receipt.machine_id,
            name,
            os: crate::platform::os(),
            arch: crate::platform::arch(),
            helper_version: env!("CARGO_PKG_VERSION"),
        };
        let reply: HeartbeatReceipt = self
            .post(
                &format!("api/v1/machines/{}/heartbeat", receipt.host_id),
                &body,
                Some(secret),
            )
            .await?;
        if reply.schema != 1
            || reply.observatory_id != receipt.observatory_id
            || reply.host_id != receipt.host_id
            || reply.state != "connected"
            || reply.access != ["observe"]
            || !(5..=300).contains(&reply.heartbeat_seconds)
        {
            return Err(RemoteError {
                status: None,
                message: "The heartbeat reply did not match this Observatory and computer.".into(),
            });
        }
        Ok(reply)
    }
    pub async fn revoke(
        &self,
        receipt: &Receipt,
        secret: &str,
    ) -> std::result::Result<(), RemoteError> {
        let reply: serde_json::Value = self
            .post(
                &format!("api/v1/machines/{}/revoke", receipt.host_id),
                &serde_json::json!({}),
                Some(secret),
            )
            .await?;
        if reply["host_id"] != receipt.host_id.to_string() || reply["state"] != "disconnected" {
            return Err(RemoteError {
                status: None,
                message: "Observatory did not confirm disconnection.".into(),
            });
        }
        Ok(())
    }
}
