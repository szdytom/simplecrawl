use std::collections::BTreeMap;

use feed_rs::model::Entry;
use url::Url;

use crate::{
	http::HttpClient,
	state::SourceState,
	util::{normalize_url, one_line, short_hash},
};

#[derive(Debug, Clone)]
pub struct FeedEntry {
	pub key: String,
	pub id: String,
	pub title: String,
	pub url: Option<String>,
	pub author: Option<String>,
	pub published: Option<i64>,
	pub updated: Option<i64>,
	pub tags: Vec<String>,
}

#[derive(Debug)]
pub struct FeedData {
	pub title: Option<String>,
	pub etag: Option<String>,
	pub last_modified: Option<String>,
	pub entries: Vec<FeedEntry>,
	pub not_modified: bool,
}

pub async fn fetch_feed(
	http: &HttpClient,
	feed_url: &str,
	headers: &BTreeMap<String, String>,
	state: &SourceState,
) -> Result<FeedData, FeedError> {
	let validators =
		match (state.etag.as_deref(), state.last_modified.as_deref()) {
			(Some(etag), Some(last_modified)) => Some((etag, last_modified)),
			(Some(etag), None) => Some((etag, "")),
			(None, Some(last_modified)) => Some(("", last_modified)),
			(None, None) => None,
		};
	let response = http
		.get(
			feed_url,
			headers,
			Some(
				"application/rss+xml, application/atom+xml, application/feed+json, application/xml, text/xml, */*",
			),
			validators,
		)
		.await?;
	if response.is_not_modified() {
		return Ok(FeedData {
			title: state.title.clone(),
			etag: response.etag.or(state.etag.clone()),
			last_modified: response
				.last_modified
				.or(state.last_modified.clone()),
			entries: Vec::new(),
			not_modified: true,
		});
	}
	let body = response.body;
	let feed_url_owned = response.final_url;
	let parsed = tokio::task::spawn_blocking(move || {
		feed_rs::parser::parse(std::io::Cursor::new(body))
	})
	.await
	.map_err(|e| FeedError::Join(e.to_string()))?
	.map_err(|e| FeedError::Parse(e.to_string()))?;
	let base = Url::parse(&feed_url_owned)
		.ok()
		.or_else(|| Url::parse(feed_url).ok());
	let feed_title = parsed.title.as_ref().map(|t| one_line(&t.content));
	let feed_author = parsed.authors.first().and_then(person_name);
	let entries = parsed
		.entries
		.iter()
		.map(|entry| {
			convert_entry(entry, base.as_ref(), feed_author.as_deref())
		})
		.collect();
	Ok(FeedData {
		title: feed_title.filter(|title| !title.is_empty()),
		etag: response.etag,
		last_modified: response.last_modified,
		entries,
		not_modified: false,
	})
}

fn convert_entry(
	entry: &Entry,
	base: Option<&Url>,
	feed_author: Option<&str>,
) -> FeedEntry {
	let url = pick_link(entry, base);
	let title = entry.title.as_ref().map_or_else(
		|| url.clone().unwrap_or_else(|| entry.id.clone()),
		|title| one_line(&title.content),
	);
	let title = if title.is_empty() {
		url.clone().unwrap_or_else(|| entry.id.clone())
	} else {
		title
	};
	let author = entry
		.authors
		.first()
		.and_then(person_name)
		.or_else(|| feed_author.map(ToString::to_string));
	let published = entry.published.map(|value| value.timestamp());
	let updated = entry.updated.map(|value| value.timestamp());
	let tags = entry
		.categories
		.iter()
		.map(|category| category.term.trim().to_string())
		.filter(|tag| !tag.is_empty())
		.collect();
	let key = entry_key(&entry.id, url.as_deref(), &title, published);
	FeedEntry {
		key,
		id: entry.id.clone(),
		title,
		url,
		author,
		published,
		updated,
		tags,
	}
}

fn pick_link(entry: &Entry, base: Option<&Url>) -> Option<String> {
	let mut fallback = None;
	for link in &entry.links {
		let Some(href) = resolve(&link.href, base) else {
			continue;
		};
		let Ok(parsed) = Url::parse(&href) else {
			continue;
		};
		if !matches!(parsed.scheme(), "http" | "https") {
			continue;
		}
		let alternate =
			link.rel.as_deref().is_none_or(|rel| rel == "alternate");
		let html = link
			.media_type
			.as_deref()
			.is_none_or(|media| media.contains("html"));
		if alternate && html {
			return Some(href);
		}
		if fallback.is_none() {
			fallback = Some(href);
		}
	}
	fallback
}

fn resolve(value: &str, base: Option<&Url>) -> Option<String> {
	if let Some(url) = base {
		return url.join(value).ok().map(|url| url.to_string());
	}
	Url::parse(value).ok().map(|url| url.to_string())
}

fn person_name(person: &feed_rs::model::Person) -> Option<String> {
	person
		.name
		.clone()
		.or_else(|| person.email.clone())
		.map(|name| one_line(&name))
		.map(|name| {
			name.strip_prefix('(')
				.and_then(|name| name.strip_suffix(')'))
				.map_or(name.clone(), ToString::to_string)
		})
		.filter(|name| !name.is_empty())
}

pub fn entry_key(
	id: &str,
	url: Option<&str>,
	title: &str,
	published: Option<i64>,
) -> String {
	let identity = if !id.is_empty() && !looks_generated_uuid(id) {
		id.to_string()
	} else if let Some(url) = url {
		normalize_url(url)
	} else {
		format!("{title}\u{1f}{}", published.unwrap_or_default())
	};
	short_hash(&identity, 32)
}

fn looks_generated_uuid(value: &str) -> bool {
	let bytes = value.as_bytes();
	if bytes.len() != 36 {
		return false;
	}
	for (i, byte) in bytes.iter().enumerate() {
		if matches!(i, 8 | 13 | 18 | 23) {
			if *byte != b'-' {
				return false;
			}
		} else if !byte.is_ascii_hexdigit() {
			return false;
		}
	}
	true
}

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
	#[error("feed request failed: {0}")]
	Http(#[from] crate::http::HttpError),
	#[error("failed to parse feed: {0}")]
	Parse(String),
	#[error("feed parser task failed: {0}")]
	Join(String),
}
