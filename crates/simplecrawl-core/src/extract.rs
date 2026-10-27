use std::{
	collections::{BTreeMap, BTreeSet},
	path::Path,
	sync::OnceLock,
};

use chrono::DateTime;
use dom_query::{Document, Matcher, NodeId};
use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;

use crate::{
	http::HttpClient,
	util::{atomic_write, hash_hex, one_line, short_hash},
};

#[derive(Debug)]
pub struct Extracted {
	pub title: String,
	pub byline: Option<String>,
	pub published: Option<i64>,
	pub updated: Option<i64>,
	pub markdown: String,
}

pub fn extract(
	html: &str,
	url: &str,
	selector: Option<&str>,
) -> Result<Extracted, ExtractError> {
	let doc = match selector {
		Some(selector) => scoped_document(html, selector)?,
		None => Document::from(html),
	};
	let mut readability = Readability::with_document(doc, Some(url), None)
		.map_err(|e| ExtractError::Readability(e.to_string()))?;
	readability.config = Config {
		text_mode: TextMode::Markdown,
		max_elements_to_parse: 100_000,
		..Config::default()
	};
	let article = readability
		.parse()
		.map_err(|e| ExtractError::Readability(e.to_string()))?;
	let title = one_line(&article.title);
	let body = article.text_content.trim().to_string();
	if body.is_empty() {
		return Err(ExtractError::Empty);
	}
	let markdown = strip_leading_title(&body, &title);
	if markdown.trim().is_empty() {
		return Err(ExtractError::Empty);
	}
	let published = article.published_time.as_deref().and_then(parse_timestamp);
	let updated = article.modified_time.as_deref().and_then(parse_timestamp);
	Ok(Extracted {
		title,
		byline: article.byline.map(|value| one_line(&value)),
		published,
		updated,
		markdown,
	})
}

/// Restricts the document body to the elements matched by `selector`.
/// Nested matches are kept once, and everything outside the matches is removed.
fn scoped_document(
	html: &str,
	selector: &str,
) -> Result<Document, ExtractError> {
	let doc = Document::from(html);
	let matcher = Matcher::new(selector)
		.map_err(|_| ExtractError::InvalidSelector(selector.to_string()))?;
	let selection = doc.select_matcher(&matcher);
	if selection.is_empty() {
		return Err(ExtractError::SelectorMiss(selector.to_string()));
	}
	let mut kept = BTreeSet::<NodeId>::new();
	let mut scoped = String::new();
	for node in selection.nodes() {
		if node
			.ancestors(None)
			.iter()
			.any(|ancestor| kept.contains(&ancestor.id))
		{
			continue;
		}
		kept.insert(node.id);
		scoped.push_str(&node.inner_html());
	}
	doc.select_single("body").set_html(scoped);
	Ok(doc)
}

fn parse_timestamp(value: &str) -> Option<i64> {
	DateTime::parse_from_rfc3339(value)
		.ok()
		.map(|value| value.timestamp())
}

fn strip_leading_title(body: &str, title: &str) -> String {
	let mut lines = body.lines().peekable();
	let Some(first) = lines.peek() else {
		return body.to_string();
	};
	let first = first.trim();
	if let Some(heading) = first.strip_prefix("# ")
		&& normalize_heading(heading) == normalize_heading(title)
	{
		lines.next();
		let rest = lines.collect::<Vec<_>>().join("\n");
		return rest.trim_start_matches('\n').to_string();
	}
	body.to_string()
}

fn normalize_heading(value: &str) -> String {
	value
		.chars()
		.filter(|ch| ch.is_alphanumeric())
		.flat_map(char::to_lowercase)
		.collect()
}

pub async fn localize_images(
	http: &HttpClient,
	headers: &BTreeMap<String, String>,
	markdown: &str,
	source_dir: &Path,
	base_url: &str,
) -> (String, Vec<String>) {
	let regex = image_regex();
	let base = url::Url::parse(base_url).ok();
	let mut urls = Vec::new();
	for captures in regex.captures_iter(markdown) {
		let Some(url) =
			captures.name("angle").or_else(|| captures.name("plain"))
		else {
			continue;
		};
		let raw = url.as_str();
		if raw.starts_with("data:") {
			continue;
		}
		let resolved = base
			.as_ref()
			.and_then(|base| base.join(raw).ok())
			.map_or_else(|| raw.to_string(), |url| url.to_string());
		if !urls.iter().any(|(existing, _)| existing == &resolved) {
			urls.push((resolved, raw.to_string()));
		}
	}
	if urls.is_empty() {
		return (markdown.to_string(), Vec::new());
	}

	let futures = urls.into_iter().map(|(url, raw)| {
		let source_dir = source_dir.to_path_buf();
		async move {
			let local =
				match download_image(http, headers, &url, &source_dir).await {
					Ok(local) => local,
					Err(err) => return (raw, url, None, Some(err)),
				};
			(raw, url, Some(local), None)
		}
	});
	let results = futures::future::join_all(futures).await;
	let mut mapping = BTreeMap::new();
	let mut warnings = Vec::new();
	for (raw, url, local, error) in results {
		if let Some(local) = local {
			mapping.insert(raw, local.clone());
			mapping.insert(url, local);
		}
		if let Some(error) = error {
			warnings.push(error);
		}
	}
	(rewrite_images(markdown, &mapping), warnings)
}

async fn download_image(
	http: &HttpClient,
	headers: &BTreeMap<String, String>,
	url: &str,
	source_dir: &Path,
) -> Result<String, String> {
	let parsed =
		url::Url::parse(url).map_err(|_| format!("invalid image URL {url}"))?;
	if !matches!(parsed.scheme(), "http" | "https") {
		return Err(format!("unsupported image URL {url}"));
	}
	let normalized = crate::util::normalize_url(url);
	let hash = hash_hex(&normalized);
	let stem = short_hash(&normalized, 16);
	if let Some(existing) = existing_image(source_dir, &hash[..2], &stem) {
		return Ok(existing);
	}
	let response = http
		.get(url, headers, Some("image/*"), None)
		.await
		.map_err(|err| format!("image {url}: {err}"))?;
	let content_type = response.content_type.as_deref().unwrap_or("");
	if !content_type.starts_with("image/") {
		return Err(format!(
			"image {url}: unexpected content type {content_type}"
		));
	}
	let extension = image_extension(parsed.as_str(), Some(content_type));
	let relative = format!("images/{}/{}.{}", &hash[..2], stem, extension);
	let target = source_dir.join(&relative);
	atomic_write(&target, &response.body)
		.map_err(|err| format!("image {url}: {err}"))?;
	Ok(relative)
}

fn image_extension(url: &str, content_type: Option<&str>) -> String {
	let from_type = content_type.map(|value| {
		value
			.split(';')
			.next()
			.unwrap_or(value)
			.trim()
			.to_ascii_lowercase()
	});
	let ext = match from_type.as_deref() {
		Some("image/jpeg") => Some("jpg"),
		Some("image/png") => Some("png"),
		Some("image/gif") => Some("gif"),
		Some("image/webp") => Some("webp"),
		Some("image/svg+xml") => Some("svg"),
		Some("image/avif") => Some("avif"),
		Some("image/bmp") => Some("bmp"),
		Some("image/x-icon" | "image/vnd.microsoft.icon") => Some("ico"),
		_ => None,
	};
	if let Some(ext) = ext {
		return ext.to_string();
	}
	url::Url::parse(url)
		.ok()
		.and_then(|url| {
			Path::new(url.path())
				.extension()
				.and_then(|ext| ext.to_str())
				.map(str::to_ascii_lowercase)
		})
		.filter(|ext| {
			(1..=5).contains(&ext.len())
				&& ext.chars().all(|ch| ch.is_ascii_alphanumeric())
		})
		.unwrap_or_else(|| "img".to_string())
}

fn rewrite_images(
	markdown: &str,
	mapping: &BTreeMap<String, String>,
) -> String {
	let regex = image_regex();
	let mut out = String::with_capacity(markdown.len());
	let mut last = 0;
	for captures in regex.captures_iter(markdown) {
		let Some(url) =
			captures.name("angle").or_else(|| captures.name("plain"))
		else {
			continue;
		};
		let Some(replacement) = mapping.get(url.as_str()) else {
			continue;
		};
		out.push_str(&markdown[last..url.start()]);
		out.push_str(replacement);
		last = url.end();
	}
	out.push_str(&markdown[last..]);
	out
}

fn image_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| {
		Regex::new(
			r#"!\[[^\]]*\]\((?:<(?P<angle>[^>]+)>|(?P<plain>[^)\s]+))(?:\s+"[^"]*")?\)"#,
		)
		.expect("image regex is valid")
	})
}

fn existing_image(
	source_dir: &Path,
	prefix: &str,
	stem: &str,
) -> Option<String> {
	let dir = source_dir.join("images").join(prefix);
	let entries = std::fs::read_dir(&dir).ok()?;
	for entry in entries.filter_map(Result::ok) {
		let path = entry.path();
		if path.file_stem().and_then(|value| value.to_str()) == Some(stem) {
			let file = path.file_name()?.to_string_lossy();
			return Some(format!("images/{prefix}/{file}"));
		}
	}
	None
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
	#[error("readability extraction failed: {0}")]
	Readability(String),
	#[error("invalid CSS selector `{0}`")]
	InvalidSelector(String),
	#[error("CSS selector `{0}` matched no elements")]
	SelectorMiss(String),
	#[error("no readable article content found")]
	Empty,
}
