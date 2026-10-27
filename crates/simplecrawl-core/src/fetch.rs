use std::{path::Path, time::Duration};

use serde::Serialize;
use tokio::sync::Semaphore;

use crate::{
	config::Config,
	extract::{extract, localize_images},
	hook::{CommandContext, run_command},
	http::HttpClient,
	index,
	reporter::Reporter,
	state::{EntryState, SourceState, StateError},
	sync::key_for_url,
	util::{
		atomic_write, choose_file_name, local_date, local_rfc3339, now_unix,
	},
};

pub struct FetchOutcome {
	pub path: std::path::PathBuf,
	pub skipped: bool,
}

pub async fn run_fetch(
	root: &Path,
	config: &Config,
	url: &str,
	reporter: Reporter,
) -> Result<FetchOutcome, FetchError> {
	let parsed = url::Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
	if !matches!(parsed.scheme(), "http" | "https")
		|| parsed.host_str().is_none()
	{
		return Err(FetchError::InvalidUrl);
	}
	let dir = root.join("misc");
	std::fs::create_dir_all(&dir).map_err(FetchError::Io)?;
	let mut state = SourceState::load(&dir)?;
	let key = key_for_url(url);
	if let Some(entry) = state.entries.get(&key) {
		return Ok(FetchOutcome {
			path: dir.join(&entry.path),
			skipped: true,
		});
	}
	if state.ignored.iter().any(|entry| entry.key == key) {
		return Err(FetchError::Ignored(url.to_string()));
	}
	let http = HttpClient::new(&config.runtime)?;
	let headers = config.headers_for(None);
	let response = http
		.get(url, &headers, Some("text/html,application/xhtml+xml"), None)
		.await?;
	if let Some(content_type) = response.content_type.as_deref()
		&& !content_type.contains("html")
	{
		return Err(FetchError::ContentType(content_type.to_string()));
	}
	let final_url = response.final_url.clone();
	let html = response.text();
	let extracted =
		tokio::task::spawn_blocking(move || extract(&html, &final_url))
			.await
			.map_err(|e| FetchError::Join(e.to_string()))??;
	let (body, warnings) = localize_images(
		&http,
		&headers,
		&extracted.markdown,
		&dir,
		&response.final_url,
	)
	.await;
	for warning in warnings {
		reporter.warn(format!("misc: {warning}"));
	}
	let now = now_unix();
	let published_ts = extracted.published.unwrap_or(now);
	let updated_ts = extracted.updated;
	let title = if extracted.title.is_empty() {
		url.to_string()
	} else {
		extracted.title.clone()
	};
	let date = local_date(published_ts);
	let file_name = choose_file_name(&dir, &date, &title);
	let published = local_rfc3339(published_ts);
	let updated = updated_ts.map(local_rfc3339);
	let markdown = crate::render::render_article(
		&crate::render::ArticleMeta {
			title: &title,
			url,
			author: extracted.byline.as_deref(),
			published: Some(published.as_str()),
			updated: updated.as_deref(),
			feed: None,
			tags: &[],
		},
		&body,
	);
	let path = dir.join(&file_name);
	atomic_write(&path, markdown.as_bytes()).map_err(FetchError::Io)?;
	state.entries.insert(
		key.clone(),
		EntryState {
			title: title.clone(),
			url: url.to_string(),
			author: extracted.byline,
			published: Some(published),
			published_date: Some(date),
			published_ts: Some(published_ts),
			updated,
			tags: Vec::new(),
			path: file_name.clone(),
			downloaded_on: local_date(now),
			downloaded_at: local_rfc3339(now),
			summary: None,
		},
	);
	state.last_sync = Some(local_rfc3339(now));
	state.save(&dir)?;
	reporter.info(format!("misc: {title} -> {file_name}"));

	let hook_gate = Semaphore::new(config.runtime.hook_concurrency.max(1));
	if let Some(command) = config.post_download_for(None) {
		let _permit = hook_gate.acquire().await.expect("hook semaphore open");
		let context = CommandContext {
			root,
			source: "misc",
			file: &path,
			url,
			title: &title,
		};
		if let Err(err) = run_command(
			&command,
			&context,
			Duration::from_secs(config.runtime.hook_timeout_secs),
		)
		.await
		{
			reporter.warn(format!("misc: post_download failed: {err}"));
		}
	}
	if let Some(command) = config.summarize_for(None) {
		let _permit = hook_gate.acquire().await.expect("hook semaphore open");
		let context = CommandContext {
			root,
			source: "misc",
			file: &path,
			url,
			title: &title,
		};
		if let Ok(output) = run_command(
			&command,
			&context,
			Duration::from_secs(config.runtime.summary_timeout_secs),
		)
		.await && let Some(summary) =
			crate::render::normalize_summary(&output)
			&& let Some(entry) = state.entries.get_mut(&key)
		{
			entry.summary = Some(summary);
			state.save(&dir)?;
		}
	}
	index::rebuild_today(root, config)?;
	Ok(FetchOutcome {
		path,
		skipped: false,
	})
}

#[derive(Debug, Serialize)]
pub struct ListEntry {
	pub dir: String,
	pub title: Option<String>,
	pub url: String,
	pub tags: Vec<String>,
	pub articles: usize,
	pub ignored: usize,
	pub last_sync: Option<String>,
}

pub fn list_sources(
	root: &Path,
	config: &Config,
) -> Result<Vec<ListEntry>, ListError> {
	let mut out = Vec::new();
	for feed in &config.feeds {
		let dir = root.join(&feed.dir);
		let state = SourceState::load(&dir).map_err(ListError::State)?;
		out.push(ListEntry {
			dir: feed.dir.clone(),
			title: state.title,
			url: feed.url.clone(),
			tags: feed.tags.clone(),
			articles: state.entries.len(),
			ignored: state.ignored.len(),
			last_sync: state.last_sync,
		});
	}
	let misc_dir = root.join("misc");
	if misc_dir.join(crate::state::STATE_FILE).exists() {
		let state = SourceState::load(&misc_dir).map_err(ListError::State)?;
		out.push(ListEntry {
			dir: "misc".to_string(),
			title: Some("misc".to_string()),
			url: String::new(),
			tags: Vec::new(),
			articles: state.entries.len(),
			ignored: state.ignored.len(),
			last_sync: state.last_sync,
		});
	}
	Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
	#[error("invalid article URL; only http(s) is supported")]
	InvalidUrl,
	#[error("article is ignored in misc state")]
	Ignored(String),
	#[error("unexpected content type {0}")]
	ContentType(String),
	#[error("failed to access files: {0}")]
	Io(std::io::Error),
	#[error(transparent)]
	Http(#[from] crate::http::HttpError),
	#[error(transparent)]
	State(#[from] StateError),
	#[error(transparent)]
	Extract(#[from] crate::extract::ExtractError),
	#[error(transparent)]
	Index(#[from] index::IndexError),
	#[error("extraction task failed: {0}")]
	Join(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ListError {
	#[error(transparent)]
	State(#[from] StateError),
}
