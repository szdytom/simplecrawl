use std::{
	collections::BTreeMap,
	path::{Path, PathBuf},
	sync::{Arc, Mutex},
	time::Duration,
};

use futures::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use crate::{
	config::{Config, FeedConfig},
	extract::{extract, localize_images},
	feed::{FeedEntry, fetch_feed},
	hook::{CommandContext, run_command},
	http::{HttpClient, HttpError},
	index,
	reporter::Reporter,
	state::{EntryState, SourceState, StateError},
	util::{
		atomic_write, local_date, local_rfc3339, normalize_url, now_unix,
		slugify,
	},
};

pub struct SyncOptions {
	pub selectors: Vec<String>,
	pub dry_run: bool,
	pub limit: Option<usize>,
	pub mark_failed: bool,
	pub reporter: Reporter,
}

#[derive(Debug, Default)]
pub struct SyncReport {
	pub feeds: usize,
	pub new: usize,
	pub failed: usize,
	pub skipped: usize,
	pub errors: Vec<String>,
}

struct Failure {
	key: String,
	url: String,
	title: String,
	reason: String,
}

struct FilenameAllocator {
	used: Mutex<BTreeMap<String, usize>>,
}

impl FilenameAllocator {
	fn allocate(&self, dir: &Path, date: &str, title: &str) -> String {
		let base = format!("{date}-{}", slugify(title));
		let mut used = self.used.lock().expect("filename map is not poisoned");
		let mut n = used.get(&base).copied().unwrap_or(0) + 1;
		loop {
			let name = if n == 1 {
				format!("{base}.md")
			} else {
				format!("{base}-{n}.md")
			};
			if !dir.join(&name).exists() {
				used.insert(base.clone(), n);
				return name;
			}
			n += 1;
		}
	}
}

pub async fn run_sync(
	root: &Path,
	config: &Config,
	options: SyncOptions,
) -> Result<SyncReport, SyncError> {
	let feeds = crate::select::select(config, &options.selectors)?;
	let http = HttpClient::new(&config.runtime)?;
	let hook_gate = Arc::new(Semaphore::new(config.runtime.hook_concurrency));
	let mut report = SyncReport {
		feeds: feeds.len(),
		..SyncReport::default()
	};
	for feed in feeds {
		match sync_feed(root, config, feed, &http, &hook_gate, &options).await {
			Ok(outcome) => {
				report.new += outcome.new;
				report.failed += outcome.failed;
				report.skipped += outcome.skipped;
			}
			Err(err) => {
				report.failed += 1;
				report.errors.push(format!("{}: {err}", feed.dir));
			}
		}
	}
	if !options.dry_run {
		index::rebuild_today(root, config)?;
	}
	Ok(report)
}

struct FeedOutcome {
	new: usize,
	failed: usize,
	skipped: usize,
}

#[allow(clippy::too_many_arguments)]
async fn sync_feed(
	root: &Path,
	config: &Config,
	feed: &FeedConfig,
	http: &HttpClient,
	hook_gate: &Arc<Semaphore>,
	options: &SyncOptions,
) -> Result<FeedOutcome, SyncError> {
	let dir = root.join(&feed.dir);
	std::fs::create_dir_all(&dir).map_err(SyncError::Io)?;
	let mut state = SourceState::load(&dir)?;
	if let Some(existing) = state.feed_url.as_deref()
		&& existing != feed.url
	{
		return Err(SyncError::FeedUrlMismatch(
			feed.dir.clone(),
			existing.to_string(),
			feed.url.clone(),
		));
	}
	state.feed_url = Some(feed.url.clone());
	let previous_etag = state.etag.clone();
	let previous_last_modified = state.last_modified.clone();
	let headers = config.headers_for(Some(feed));
	let data = fetch_feed(http, &feed.url, &headers, &state).await?;
	let now = now_unix();
	state.etag = data.etag;
	state.last_modified = data.last_modified;
	if let Some(title) = data.title {
		state.title = Some(title);
	}
	state.last_sync = Some(local_rfc3339(now));

	let state_arc = Arc::new(AsyncMutex::new(state));
	if data.not_modified {
		options
			.reporter
			.detail(format!("{}: not modified", feed.dir));
		let outcome = FeedOutcome {
			new: 0,
			failed: 0,
			skipped: 0,
		};
		if !options.dry_run {
			fill_missing_summaries(
				root, config, feed, &state_arc, &dir, hook_gate, options,
			)
			.await?;
			save_state(&state_arc, &dir).await?;
		}
		return Ok(outcome);
	}

	let mut entries = data.entries;
	let skipped = {
		let state = state_arc.lock().await;
		let before = entries.len();
		entries.retain(|entry| !state.is_known(&entry.key));
		before - entries.len()
	};
	entries.sort_by(|a, b| {
		b.published
			.cmp(&a.published)
			.then_with(|| a.title.cmp(&b.title))
	});
	let mut truncated = false;
	if let Some(limit) = options.limit
		&& entries.len() > limit
	{
		truncated = true;
		entries.truncate(limit);
	}
	options.reporter.info(format!(
		"{}: {} new article(s)",
		feed.dir,
		entries.len()
	));
	if options.dry_run {
		for entry in &entries {
			options
				.reporter
				.info(format!("would download {} {}", feed.dir, entry.title));
		}
		return Ok(FeedOutcome {
			new: entries.len(),
			failed: 0,
			skipped,
		});
	}

	let allocator = Arc::new(FilenameAllocator {
		used: Mutex::new(BTreeMap::new()),
	});
	let concurrency = config.runtime.download_concurrency.max(1);
	let results = futures::stream::iter(entries.into_iter().map(|entry| {
		process_entry(
			root, config, feed, http, hook_gate, &dir, &state_arc, &allocator,
			entry, options,
		)
	}))
	.buffer_unordered(concurrency)
	.collect::<Vec<_>>()
	.await;

	let mut failures = Vec::new();
	let mut new = 0;
	for result in results {
		match result {
			Ok(()) => new += 1,
			Err(failure) => failures.push(failure),
		}
	}
	if options.mark_failed {
		let mut state = state_arc.lock().await;
		for failure in &failures {
			state.mark_ignored(
				failure.key.clone(),
				failure.url.clone(),
				failure.title.clone(),
				failure.reason.clone(),
			);
		}
	}
	for failure in &failures {
		options.reporter.warn(format!(
			"{}: {}: {}",
			feed.dir, failure.title, failure.reason
		));
	}
	if !failures.is_empty() || truncated {
		let mut state = state_arc.lock().await;
		state.etag = previous_etag;
		state.last_modified = previous_last_modified;
	}
	fill_missing_summaries(
		root, config, feed, &state_arc, &dir, hook_gate, options,
	)
	.await?;
	save_state(&state_arc, &dir).await?;
	Ok(FeedOutcome {
		new,
		failed: failures.len(),
		skipped,
	})
}

#[allow(clippy::too_many_arguments)]
async fn process_entry(
	root: &Path,
	config: &Config,
	feed: &FeedConfig,
	http: &HttpClient,
	hook_gate: &Arc<Semaphore>,
	dir: &Path,
	state: &Arc<AsyncMutex<SourceState>>,
	allocator: &Arc<FilenameAllocator>,
	entry: FeedEntry,
	options: &SyncOptions,
) -> Result<(), Failure> {
	let Some(url) = entry.url.clone() else {
		return Err(Failure {
			key: entry.key,
			url: String::new(),
			title: entry.title,
			reason: "article has no URL".to_string(),
		});
	};
	let headers = config.headers_for(Some(feed));
	let response = match http
		.get(
			&url,
			&headers,
			Some("text/html,application/xhtml+xml"),
			None,
		)
		.await
	{
		Ok(response) => response,
		Err(err) => return Err(failure_from_http(&entry, &err)),
	};
	if let Some(content_type) = response.content_type.as_deref()
		&& !content_type.contains("html")
	{
		return Err(Failure {
			key: entry.key,
			url,
			title: entry.title,
			reason: format!("unexpected content type {content_type}"),
		});
	}
	let final_url = response.final_url.clone();
	let html = response.text();
	let extracted =
		match tokio::task::spawn_blocking(move || extract(&html, &final_url))
			.await
		{
			Ok(Ok(extracted)) => extracted,
			Ok(Err(err)) => {
				return Err(Failure {
					key: entry.key,
					url,
					title: entry.title,
					reason: err.to_string(),
				});
			}
			Err(err) => {
				return Err(Failure {
					key: entry.key,
					url,
					title: entry.title,
					reason: format!("extraction task failed: {err}"),
				});
			}
		};
	let (body, warnings) = localize_images(
		http,
		&headers,
		&extracted.markdown,
		dir,
		&response.final_url,
	)
	.await;
	for warning in warnings {
		options.reporter.warn(format!("{}: {warning}", feed.dir));
	}
	let now = now_unix();
	let published_ts = entry
		.published
		.or(entry.updated)
		.or(extracted.published)
		.unwrap_or(now);
	let updated_ts = entry.updated.or(extracted.updated);
	let title = entry.title.clone();
	let date = local_date(published_ts);
	let file_name = allocator.allocate(dir, &date, &title);
	let published = local_rfc3339(published_ts);
	let updated = updated_ts.map(local_rfc3339);
	let markdown = crate::render::render_article(
		&crate::render::ArticleMeta {
			title: &title,
			url: &url,
			author: entry.author.as_deref().or(extracted.byline.as_deref()),
			published: Some(published.as_str()),
			updated: updated.as_deref(),
			feed: Some(feed.url.as_str()),
			tags: &entry.tags,
		},
		&body,
	);
	let path = dir.join(&file_name);
	atomic_write(&path, markdown.as_bytes()).map_err(|err| Failure {
		key: entry.key.clone(),
		url: url.clone(),
		title: title.clone(),
		reason: format!("failed to write article: {err}"),
	})?;

	let entry_state = EntryState {
		title: title.clone(),
		url: url.clone(),
		author: entry.author.clone().or(extracted.byline.clone()),
		published: Some(published.clone()),
		published_date: Some(date),
		published_ts: Some(published_ts),
		updated,
		tags: entry.tags.clone(),
		path: file_name.clone(),
		downloaded_on: local_date(now),
		downloaded_at: local_rfc3339(now),
		summary: None,
	};
	{
		let mut state = state.lock().await;
		state.entries.insert(entry.key.clone(), entry_state);
		state.save(dir).map_err(|err| Failure {
			key: entry.key.clone(),
			url: url.clone(),
			title: title.clone(),
			reason: format!("failed to save state: {err}"),
		})?;
	}
	options
		.reporter
		.info(format!("{}: {} -> {}", feed.dir, title, file_name));

	if let Some(command) = config.post_download_for(Some(feed)) {
		let _permit = hook_gate.acquire().await.expect("hook semaphore open");
		let context = CommandContext {
			root,
			source: &feed.dir,
			file: &path,
			url: &url,
			title: &title,
		};
		if let Err(err) = run_command(
			&command,
			&context,
			Duration::from_secs(config.runtime.hook_timeout_secs),
		)
		.await
		{
			options
				.reporter
				.warn(format!("{}: post_download failed: {err}", feed.dir));
		}
	}
	Ok(())
}

fn failure_from_http(entry: &FeedEntry, err: &HttpError) -> Failure {
	Failure {
		key: entry.key.clone(),
		url: entry.url.clone().unwrap_or_default(),
		title: entry.title.clone(),
		reason: err.to_string(),
	}
}

async fn fill_missing_summaries(
	root: &Path,
	config: &Config,
	feed: &FeedConfig,
	state: &Arc<AsyncMutex<SourceState>>,
	dir: &Path,
	hook_gate: &Arc<Semaphore>,
	options: &SyncOptions,
) -> Result<(), SyncError> {
	let Some(command) = config.summarize_for(Some(feed)) else {
		return Ok(());
	};
	let missing = {
		let state = state.lock().await;
		state
			.entries
			.iter()
			.filter(|(_, entry)| entry.summary.is_none())
			.filter(|(_, entry)| dir.join(&entry.path).exists())
			.map(|(key, entry)| {
				(
					key.clone(),
					entry.path.clone(),
					entry.url.clone(),
					entry.title.clone(),
				)
			})
			.collect::<Vec<_>>()
	};
	if missing.is_empty() {
		return Ok(());
	}
	let concurrency = config.runtime.hook_concurrency.max(1);
	let results = futures::stream::iter(missing.into_iter().map(
		|(key, path, url, title)| {
			let state = state.clone();
			let hook_gate = hook_gate.clone();
			let command = command.clone();
			let dir = dir.to_path_buf();
			let root = root.to_path_buf();
			let source = feed.dir.clone();
			async move {
				let _permit =
					hook_gate.acquire().await.expect("hook semaphore open");
				let file = dir.join(&path);
				let context = CommandContext {
					root: &root,
					source: &source,
					file: &file,
					url: &url,
					title: &title,
				};
				let output = run_command(
					&command,
					&context,
					Duration::from_secs(config.runtime.summary_timeout_secs),
				)
				.await;
				let Ok(output) = output else {
					return;
				};
				let Some(summary) = crate::render::normalize_summary(&output)
				else {
					return;
				};
				let mut state = state.lock().await;
				if let Some(entry) = state.entries.get_mut(&key) {
					entry.summary = Some(summary);
					let _ = state.save(&dir);
				}
			}
		},
	))
	.buffer_unordered(concurrency);
	futures::pin_mut!(results);
	while let Some(()) = results.next().await {}
	options
		.reporter
		.detail(format!("{}: summaries checked", feed.dir));
	Ok(())
}

async fn save_state(
	state: &Arc<AsyncMutex<SourceState>>,
	dir: &Path,
) -> Result<(), SyncError> {
	let state = state.lock().await;
	state.save(dir)?;
	Ok(())
}

pub fn key_for_url(url: &str) -> String {
	crate::util::short_hash(&normalize_url(url), 32)
}

pub fn source_dir(root: &Path, dir: &str) -> PathBuf {
	root.join(dir)
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
	#[error(transparent)]
	Select(#[from] crate::select::SelectError),
	#[error(transparent)]
	Http(#[from] HttpError),
	#[error(transparent)]
	Feed(#[from] crate::feed::FeedError),
	#[error(transparent)]
	State(#[from] StateError),
	#[error(transparent)]
	Index(#[from] index::IndexError),
	#[error("failed to access files: {0}")]
	Io(std::io::Error),
	#[error("feed dir `{0}` already belongs to {1}, not {2}")]
	FeedUrlMismatch(String, String, String),
}
