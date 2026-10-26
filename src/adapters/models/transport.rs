use super::{Provider, error};
use crate::app::{ModelError, ModelErrorKind};
use reqwest::{
    Url,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;
use std::{fmt, io::Read, num::NonZeroUsize, time::Duration};

/// Connection configuration. A base path is retained; each adapter appends its native route.
/// Redirects, retries and environment proxies are disabled. Secrets are header-only.
#[derive(Clone)]
pub struct ClientConfig {
    pub(super) base: Url,
    key: HeaderValue,
    timeout: Duration,
    limit: NonZeroUsize,
}
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientConfig([redacted])")
    }
}
impl ClientConfig {
    pub fn new(
        base_url: &str,
        api_key: &str,
        timeout: Duration,
        max_response_bytes: NonZeroUsize,
    ) -> Result<Self, ModelError> {
        let base = Url::parse(base_url).map_err(|_| error(ModelErrorKind::InvalidConfig))?;
        let has_userinfo = base_url.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
        if has_userinfo
            || !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || timeout.is_zero()
            || timeout.as_millis() > u64::MAX as u128
            || api_key.trim().is_empty()
            || max_response_bytes.get() == usize::MAX
        {
            return Err(error(ModelErrorKind::InvalidConfig));
        }
        let mut key =
            HeaderValue::from_str(api_key).map_err(|_| error(ModelErrorKind::InvalidConfig))?;
        key.set_sensitive(true);
        Ok(Self {
            base,
            key,
            timeout,
            limit: max_response_bytes,
        })
    }
}
#[derive(Clone)]
pub(super) struct Transport {
    config: ClientConfig,
    client: Client,
}
impl Transport {
    pub fn new(config: ClientConfig) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| error(ModelErrorKind::InvalidConfig))?;
        Ok(Self { config, client })
    }
    pub fn endpoint(&self, provider: Provider) -> Url {
        let mut url = self.config.base.clone();
        let mut segments = url
            .path_segments_mut()
            .expect("validated hierarchical HTTP URL");
        segments.pop_if_empty();
        match provider {
            Provider::OpenAI => {
                segments.push("responses");
            }
            Provider::Anthropic => {
                segments.push("messages");
            }
        }
        drop(segments);
        url
    }
    pub fn send(
        &self,
        provider: Provider,
        endpoint: Url,
        body: &Value,
    ) -> Result<Value, ModelError> {
        let mut headers = HeaderMap::new();
        match provider {
            Provider::OpenAI => {
                let key = self
                    .config
                    .key
                    .to_str()
                    .map_err(|_| error(ModelErrorKind::InvalidConfig))?;
                let mut bearer = HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| error(ModelErrorKind::InvalidConfig))?;
                bearer.set_sensitive(true);
                headers.insert("authorization", bearer);
            }
            Provider::Anthropic => {
                headers.insert("x-api-key", self.config.key.clone());
                headers.insert("anthropic-version", HeaderValue::from_static("2026-10-21"));
            }
        }
        let response = self
            .client
            .post(endpoint)
            .headers(headers)
            .json(body)
            .send()
            .map_err(|e| {
                error(if e.is_timeout() {
                    ModelErrorKind::Timeout
                } else {
                    ModelErrorKind::Transport
                })
            })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(ModelError::http(
                match status {
                    400 => ModelErrorKind::InvalidSettings,
                    401 | 403 => ModelErrorKind::Authentication,
                    429 => ModelErrorKind::RateLimited,
                    500..=599 => ModelErrorKind::ProviderFailure,
                    _ => ModelErrorKind::Http,
                },
                status,
            ));
        }
        let max = self.config.limit.get();
        if response.content_length().is_some_and(|n| n > max as u64) {
            return Err(error(ModelErrorKind::ResponseTooLarge));
        }
        let mut bytes = Vec::new();
        response
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                error(
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.get_ref()
                            .and_then(|source| source.downcast_ref::<reqwest::Error>())
                            .is_some_and(reqwest::Error::is_timeout)
                    {
                        ModelErrorKind::Timeout
                    } else {
                        ModelErrorKind::Transport
                    },
                )
            })?;
        if bytes.len() > max {
            return Err(error(ModelErrorKind::ResponseTooLarge));
        }
        serde_json::from_slice(&bytes).map_err(|_| error(ModelErrorKind::MalformedResponse))
    }
}
