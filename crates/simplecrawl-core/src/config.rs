use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use jiff::civil::Date;
use serde::{Deserialize, Deserializer, de};
use toml::Value;

use crate::util::ensure_relative_segment;

pub const CONFIG_FILE: &str = "subscriptions.toml";
pub const GLOBAL_CONFIG_DIR: &str = "scrawl";
pub const GLOBAL_CONFIG_FILE: &str = "config.toml";
pub const RESERVED_NAMES: &[&str] = &[
	"index",
	"misc",
	"templates",
	"INDEX.md",
	"subscriptions.toml",
];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	#[serde(default)]
	pub headers: BTreeMap<String, String>,
	#[serde(default)]
	pub hooks: Hooks,
	#[serde(default)]
	pub summarize: Summarize,
	#[serde(default)]
	pub runtime: Runtime,
	#[serde(default)]
	pub feeds: Vec<FeedConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedConfig {
	pub url: String,
	pub dir: String,
	#[serde(default)]
	pub tags: Vec<String>,
	#[serde(default, deserialize_with = "deserialize_after")]
	pub after: Option<String>,
	#[serde(default)]
	pub headers: BTreeMap<String, String>,
	#[serde(default)]
	pub hooks: Hooks,
	#[serde(default)]
	pub summarize: Summarize,
}

impl FeedConfig {
	pub fn after_date(&self) -> Option<Date> {
		self.after
			.as_deref()
			.map(|value| value.parse().expect("feed `after` date is validated"))
	}
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
	#[serde(default)]
	pub post_download: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summarize {
	#[serde(default)]
	pub command: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runtime {
	pub download_concurrency: usize,
	pub hook_concurrency: usize,
	pub http_timeout_secs: u64,
	pub hook_timeout_secs: u64,
	pub summary_timeout_secs: u64,
	pub per_host_delay_ms: u64,
	pub retry_base_ms: u64,
	#[serde(skip)]
	pub tls_extra_roots: Option<PathBuf>,
}

impl Default for Runtime {
	fn default() -> Self {
		Self {
			download_concurrency: 8,
			hook_concurrency: 2,
			http_timeout_secs: 30,
			hook_timeout_secs: 30,
			summary_timeout_secs: 60,
			per_host_delay_ms: 200,
			retry_base_ms: 1000,
			tls_extra_roots: None,
		}
	}
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GlobalConfig {
	tls_extra_roots: Option<PathBuf>,
}

pub fn global_config_path() -> Option<PathBuf> {
	let config_home = std::env::var_os("XDG_CONFIG_HOME")
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.filter(|path| path.is_absolute())
		.or_else(|| {
			std::env::var_os("HOME")
				.filter(|value| !value.is_empty())
				.map(|home| PathBuf::from(home).join(".config"))
		})?;
	Some(config_home.join(GLOBAL_CONFIG_DIR).join(GLOBAL_CONFIG_FILE))
}

impl Config {
	pub fn load(root: &Path) -> Result<Self, ConfigError> {
		let path = root.join(CONFIG_FILE);
		let text = fs::read_to_string(&path)
			.map_err(|e| ConfigError::Io(path.clone(), e))?;
		let mut config: Self = toml::from_str(&text)
			.map_err(|e| ConfigError::Toml(path.clone(), e))?;
		config.expand_headers()?;
		config.load_global()?;
		config.validate()?;
		Ok(config)
	}

	pub fn load_optional(root: &Path) -> Result<Self, ConfigError> {
		let path = root.join(CONFIG_FILE);
		if !path.exists() {
			let mut config = Self::default();
			config.load_global()?;
			return Ok(config);
		}
		Self::load(root)
	}

	pub fn validate(&self) -> Result<(), ConfigError> {
		let mut dirs = BTreeMap::new();
		let mut urls = BTreeMap::new();
		for feed in &self.feeds {
			if !ensure_relative_segment(&feed.dir)
				|| RESERVED_NAMES
					.iter()
					.any(|name| feed.dir.eq_ignore_ascii_case(name))
				|| feed.dir.starts_with('#')
				|| feed.dir.starts_with('^')
			{
				return Err(ConfigError::InvalidDir(feed.dir.clone()));
			}
			if let Some(previous) =
				dirs.insert(feed.dir.clone(), feed.url.clone())
			{
				return Err(ConfigError::DuplicateDir(
					feed.dir.clone(),
					previous,
				));
			}
			if let Some(previous) =
				urls.insert(feed.url.clone(), feed.dir.clone())
			{
				return Err(ConfigError::DuplicateUrl(
					feed.url.clone(),
					previous,
				));
			}
			let url = url::Url::parse(&feed.url)
				.map_err(|_| ConfigError::InvalidUrl(feed.url.clone()))?;
			if !matches!(url.scheme(), "http" | "https")
				|| url.host_str().is_none()
			{
				return Err(ConfigError::InvalidUrl(feed.url.clone()));
			}
			for tag in &feed.tags {
				if tag.is_empty()
					|| tag.starts_with('#')
					|| tag.starts_with('^')
					|| tag.chars().any(char::is_whitespace)
				{
					return Err(ConfigError::InvalidTag(tag.clone()));
				}
			}
			if let Some(after) = &feed.after
				&& after.parse::<Date>().is_err()
			{
				return Err(ConfigError::InvalidAfter(after.clone()));
			}
		}
		if self.runtime.download_concurrency == 0
			|| self.runtime.hook_concurrency == 0
		{
			return Err(ConfigError::InvalidRuntime);
		}
		Ok(())
	}

	pub fn headers_for(
		&self,
		feed: Option<&FeedConfig>,
	) -> BTreeMap<String, String> {
		let mut headers = self.headers.clone();
		if let Some(feed) = feed {
			headers.extend(feed.headers.clone());
		}
		headers
	}

	pub fn post_download_for(
		&self,
		feed: Option<&FeedConfig>,
	) -> Option<String> {
		feed.and_then(|f| f.hooks.post_download.clone())
			.or_else(|| self.hooks.post_download.clone())
	}

	pub fn summarize_for(&self, feed: Option<&FeedConfig>) -> Option<String> {
		feed.and_then(|f| f.summarize.command.clone())
			.or_else(|| self.summarize.command.clone())
	}

	fn load_global(&mut self) -> Result<(), ConfigError> {
		let Some(path) = global_config_path() else {
			return Ok(());
		};
		let text = match fs::read_to_string(&path) {
			Ok(text) => text,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
				return Ok(());
			}
			Err(error) => return Err(ConfigError::Io(path, error)),
		};
		let global: GlobalConfig = toml::from_str(&text)
			.map_err(|error| ConfigError::Toml(path.clone(), error))?;
		if let Some(extra) = global.tls_extra_roots {
			self.runtime.tls_extra_roots = Some(if extra.is_relative() {
				path.parent().map_or(extra.clone(), |dir| dir.join(&extra))
			} else {
				extra
			});
		}
		Ok(())
	}

	fn expand_headers(&mut self) -> Result<(), ConfigError> {
		for value in self.headers.values_mut() {
			*value = expand_env(value)?;
		}
		for feed in &mut self.feeds {
			for value in feed.headers.values_mut() {
				*value = expand_env(value)?;
			}
		}
		Ok(())
	}
}

fn deserialize_after<'de, D>(
	deserializer: D,
) -> Result<Option<String>, D::Error>
where
	D: Deserializer<'de>,
{
	match toml::Value::deserialize(deserializer)? {
		Value::String(value) => Ok(Some(value)),
		Value::Datetime(value) => Ok(Some(value.to_string())),
		_ => Err(de::Error::custom("`after` must be a date")),
	}
}

fn expand_env(value: &str) -> Result<String, ConfigError> {
	let mut out = String::with_capacity(value.len());
	let mut chars = value.chars().peekable();
	while let Some(ch) = chars.next() {
		if ch != '$' {
			out.push(ch);
			continue;
		}
		if chars.peek() == Some(&'$') {
			chars.next();
			out.push('$');
			continue;
		}
		if chars.peek() != Some(&'{') {
			out.push('$');
			continue;
		}
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
		if !closed || name.is_empty() {
			return Err(ConfigError::InvalidEnv(value.to_string()));
		}
		match std::env::var(&name) {
			Ok(value) => out.push_str(&value),
			Err(_) => return Err(ConfigError::MissingEnv(name)),
		}
	}
	Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
	#[error("failed to read {0}: {1}")]
	Io(std::path::PathBuf, std::io::Error),
	#[error("invalid TOML in {0}: {1}")]
	Toml(std::path::PathBuf, toml::de::Error),
	#[error("invalid feed dir `{0}`; use one plain path segment")]
	InvalidDir(String),
	#[error("duplicate feed dir `{0}` (also used by {1})")]
	DuplicateDir(String, String),
	#[error("duplicate feed URL `{0}` (also used by {1})")]
	DuplicateUrl(String, String),
	#[error("invalid feed URL `{0}`; only http(s) is supported")]
	InvalidUrl(String),
	#[error("invalid feed tag `{0}`")]
	InvalidTag(String),
	#[error("invalid feed after date `{0}`; use YYYY-MM-DD")]
	InvalidAfter(String),
	#[error("runtime concurrency and timeouts must be greater than zero")]
	InvalidRuntime,
	#[error("missing environment variable `{0}` used in headers")]
	MissingEnv(String),
	#[error("invalid environment interpolation in header value `{0}`")]
	InvalidEnv(String),
}
