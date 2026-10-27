use std::{
	collections::{BTreeMap, HashMap},
	path::PathBuf,
	sync::{Arc, Mutex},
	time::Duration,
};

use reqwest::{
	StatusCode,
	header::{
		ACCEPT, CONTENT_TYPE, ETAG, HeaderMap, HeaderName, HeaderValue,
		IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER,
	},
};
use tokio::sync::Semaphore;

use crate::config::Runtime;

const MAX_BODY: usize = 20 * 1024 * 1024;
const MAX_RETRIES: u32 = 5;

pub struct HttpClient {
	client: reqwest::Client,
	global: Arc<Semaphore>,
	hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
	per_host_delay: Duration,
	retry_base: Duration,
}

pub struct HttpResponse {
	pub status: StatusCode,
	pub final_url: String,
	pub body: Vec<u8>,
	pub content_type: Option<String>,
	pub etag: Option<String>,
	pub last_modified: Option<String>,
}

impl HttpResponse {
	pub fn is_not_modified(&self) -> bool {
		self.status == StatusCode::NOT_MODIFIED
	}

	pub fn text(&self) -> String {
		decode_text(&self.body, self.content_type.as_deref())
	}
}

impl HttpClient {
	pub fn new(runtime: &Runtime) -> Result<Self, HttpError> {
		let mut builder = reqwest::Client::builder()
			.timeout(Duration::from_secs(runtime.http_timeout_secs))
			.user_agent(concat!("scrawl/", env!("CARGO_PKG_VERSION")))
			.redirect(reqwest::redirect::Policy::limited(10));
		if let Some(path) = &runtime.tls_extra_roots {
			let pem = std::fs::read(path).map_err(|error| {
				HttpError::ExtraRoots(path.clone(), error.to_string())
			})?;
			let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(
				|error| HttpError::ExtraRoots(path.clone(), error.to_string()),
			)?;
			for cert in certs {
				builder = builder.add_root_certificate(cert);
			}
		}
		let client = builder.build()?;
		Ok(Self {
			client,
			global: Arc::new(Semaphore::new(runtime.download_concurrency)),
			hosts: Mutex::new(HashMap::new()),
			per_host_delay: Duration::from_millis(runtime.per_host_delay_ms),
			retry_base: Duration::from_millis(runtime.retry_base_ms),
		})
	}

	pub async fn get(
		&self,
		url: &str,
		headers: &BTreeMap<String, String>,
		accept: Option<&str>,
		validators: Option<(&str, &str)>,
	) -> Result<HttpResponse, HttpError> {
		let parsed = url::Url::parse(url).map_err(|_| HttpError::InvalidUrl)?;
		let host = format!(
			"{}:{}",
			parsed.host_str().unwrap_or(""),
			parsed.port_or_known_default().unwrap_or(0)
		);
		let host_gate = {
			let mut hosts =
				self.hosts.lock().expect("host map is not poisoned");
			hosts
				.entry(host)
				.or_insert_with(|| Arc::new(Semaphore::new(1)))
				.clone()
		};

		let mut last_error = None;
		for attempt in 0..=MAX_RETRIES {
			let _global = self
				.global
				.acquire()
				.await
				.expect("global semaphore is never closed");
			let _host = host_gate
				.acquire()
				.await
				.expect("host semaphore is never closed");
			if !self.per_host_delay.is_zero() {
				tokio::time::sleep(self.per_host_delay).await;
			}

			let mut request = self.client.get(parsed.clone());
			let header_map = build_headers(headers, accept, validators)?;
			request = request.headers(header_map);
			match request.send().await {
				Ok(response) => {
					let status = response.status();
					if status == StatusCode::NOT_MODIFIED {
						return Ok(HttpResponse {
							status,
							final_url: response.url().to_string(),
							body: Vec::new(),
							content_type: header_string(
								&response,
								CONTENT_TYPE,
							),
							etag: header_string(&response, ETAG),
							last_modified: header_string(
								&response,
								LAST_MODIFIED,
							),
						});
					}
					if status == StatusCode::TOO_MANY_REQUESTS
						|| status.is_server_error()
					{
						let retry_after = response
							.headers()
							.get(RETRY_AFTER)
							.and_then(|value| value.to_str().ok())
							.and_then(|value| value.parse::<u64>().ok());
						last_error = Some(HttpError::Status(
							status.as_u16(),
							url.to_string(),
						));
						let wait = retry_after.map_or_else(
							|| self.backoff(attempt),
							|secs| Duration::from_secs(secs.min(60)),
						);
						tokio::time::sleep(wait).await;
						continue;
					}
					if !status.is_success() {
						return Err(HttpError::Status(
							status.as_u16(),
							url.to_string(),
						));
					}
					if response
						.content_length()
						.is_some_and(|len| len > MAX_BODY as u64)
					{
						return Err(HttpError::TooLarge);
					}
					let content_type = header_string(&response, CONTENT_TYPE);
					let etag = header_string(&response, ETAG);
					let last_modified = header_string(&response, LAST_MODIFIED);
					let final_url = response.url().to_string();
					let bytes = response.bytes().await?;
					if bytes.len() > MAX_BODY {
						return Err(HttpError::TooLarge);
					}
					return Ok(HttpResponse {
						status,
						final_url,
						body: bytes.to_vec(),
						content_type,
						etag,
						last_modified,
					});
				}
				Err(err) => {
					last_error = Some(HttpError::Request(err));
					if attempt < MAX_RETRIES {
						tokio::time::sleep(self.backoff(attempt)).await;
					}
				}
			}
		}
		Err(last_error.unwrap_or(HttpError::InvalidUrl))
	}

	fn backoff(&self, attempt: u32) -> Duration {
		let factor = 1u32 << attempt.min(5);
		self.retry_base.saturating_mul(factor)
	}
}

fn build_headers(
	headers: &BTreeMap<String, String>,
	accept: Option<&str>,
	validators: Option<(&str, &str)>,
) -> Result<HeaderMap, HttpError> {
	let mut map = HeaderMap::new();
	for (name, value) in headers {
		let name = HeaderName::from_bytes(name.as_bytes())
			.map_err(|_| HttpError::InvalidHeader(name.clone()))?;
		let value = HeaderValue::from_str(value)
			.map_err(|_| HttpError::InvalidHeader(name.to_string()))?;
		map.insert(name, value);
	}
	if let Some(accept) = accept {
		map.entry(ACCEPT).or_insert(
			HeaderValue::from_str(accept)
				.map_err(|_| HttpError::InvalidHeader("Accept".into()))?,
		);
	}
	if let Some((etag, last_modified)) = validators {
		if !etag.is_empty() {
			map.insert(
				IF_NONE_MATCH,
				HeaderValue::from_str(etag).map_err(|_| {
					HttpError::InvalidHeader("If-None-Match".into())
				})?,
			);
		}
		if !last_modified.is_empty() {
			map.insert(
				IF_MODIFIED_SINCE,
				HeaderValue::from_str(last_modified).map_err(|_| {
					HttpError::InvalidHeader("If-Modified-Since".into())
				})?,
			);
		}
	}
	Ok(map)
}

fn header_string(
	response: &reqwest::Response,
	name: HeaderName,
) -> Option<String> {
	response
		.headers()
		.get(name)
		.and_then(|value| value.to_str().ok())
		.map(ToString::to_string)
}

fn decode_text(bytes: &[u8], content_type: Option<&str>) -> String {
	let charset = content_type
		.and_then(|value| {
			value
				.split(';')
				.find_map(|part| part.trim().strip_prefix("charset="))
		})
		.map(|value| value.trim_matches('"'));
	let encoding = charset
		.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
		.unwrap_or(encoding_rs::UTF_8);
	let (text, _, _) = encoding.decode(bytes);
	text.into_owned()
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
	#[error("HTTP request failed: {0}")]
	Request(#[from] reqwest::Error),
	#[error("HTTP {0} for {1}")]
	Status(u16, String),
	#[error("response exceeds the 20 MiB limit")]
	TooLarge,
	#[error("invalid URL")]
	InvalidUrl,
	#[error("invalid header `{0}`")]
	InvalidHeader(String),
	#[error("failed to load extra TLS roots from {0}: {1}")]
	ExtraRoots(PathBuf, String),
}
