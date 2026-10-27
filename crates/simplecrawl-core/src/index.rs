use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use minijinja::Environment;
use serde_json::json;

use crate::{
	config::Config,
	state::{EntryState, SourceState},
	util::{
		atomic_write, local_date, markdown_escape_text, now_unix, relative_path,
	},
};

const BEGIN: &str = "<!-- simplecrawl:begin -->";
const END: &str = "<!-- simplecrawl:end -->";

const DEFAULT_DAILY: &str = r"{% for entry in entries -%}
- [{{ entry.title }}](<{{ entry.href }}>) · {{ entry.source }}{% if entry.published_date %} · {{ entry.published_date }}{% endif %}{% if entry.summary %} — {{ entry.summary }}{% endif %}
{% endfor -%}
";

const DEFAULT_INDEX: &str = r"{% for group in groups -%}
## {{ group.title }}

{% for entry in group.entries -%}
- {% if entry.published_date %}{{ entry.published_date }} {% endif %}[{{ entry.title }}](<{{ entry.href }}>)
{% endfor %}
{% endfor -%}
";

struct EntryRef {
	title: String,
	href: String,
	source: String,
	published_date: String,
	published_ts: Option<i64>,
	summary: Option<String>,
}

struct Group {
	dir: String,
	title: String,
	entries: Vec<EntryRef>,
}

struct SourceInfo {
	dir: String,
	title: String,
	state: SourceState,
}

pub fn rebuild_today(root: &Path, config: &Config) -> Result<(), IndexError> {
	let today = local_date(now_unix());
	rebuild(root, config, Some(&today))
}

pub fn rebuild_all(root: &Path, config: &Config) -> Result<(), IndexError> {
	rebuild(root, config, None)
}

fn rebuild(
	root: &Path,
	config: &Config,
	only_day: Option<&str>,
) -> Result<(), IndexError> {
	let sources = load_sources(root, config)?;
	let daily_template = load_template(root, "daily.md.j2", DEFAULT_DAILY)?;
	let index_template = load_template(root, "index.md.j2", DEFAULT_INDEX)?;

	let mut groups = Vec::new();
	let mut days: BTreeMap<String, Vec<(String, EntryRef)>> = BTreeMap::new();
	for source in sources {
		let mut group = Group {
			dir: source.dir.clone(),
			title: source.title.clone(),
			entries: Vec::new(),
		};
		let mut articles = source.state.entries.into_iter().collect::<Vec<_>>();
		articles.sort_by(|(_, a), (_, b)| {
			b.published_ts
				.cmp(&a.published_ts)
				.then_with(|| a.title.cmp(&b.title))
		});
		for (_, entry) in articles {
			let href = relative_path(
				&root.join("index"),
				&root.join(&source.dir).join(&entry.path),
			);
			let article = entry_ref(&entry, href, &source.dir);
			group.entries.push(EntryRef {
				title: article.title.clone(),
				href: article.href.clone(),
				source: article.source.clone(),
				published_date: article.published_date.clone(),
				published_ts: article.published_ts,
				summary: article.summary.clone(),
			});
			days.entry(entry.downloaded_on)
				.or_default()
				.push((source.dir.clone(), article));
		}
		if !group.entries.is_empty() {
			groups.push(group);
		}
	}

	let mut env = Environment::new();
	env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
	let daily_days = if let Some(day) = only_day {
		vec![day.to_string()]
	} else {
		days.keys().cloned().collect()
	};
	for day in daily_days {
		let mut entries = days.remove(&day).unwrap_or_default();
		entries.sort_by(|(source_a, a), (source_b, b)| {
			b.published_ts
				.cmp(&a.published_ts)
				.then_with(|| source_a.cmp(source_b))
				.then_with(|| a.title.cmp(&b.title))
		});
		let has_entries = !entries.is_empty();
		let entries = entries
			.into_iter()
			.map(|(_, entry)| {
				json!({
					"title": entry.title,
					"href": entry.href,
					"source": entry.source,
					"published_date": entry.published_date,
					"summary": entry.summary,
				})
			})
			.collect::<Vec<_>>();
		let rendered = env
			.render_str(
				&daily_template,
				minijinja::value::Serde(
					json!({ "date": day, "entries": entries }),
				),
			)
			.map_err(|e| IndexError::Template(e.to_string()))?;
		let path = root.join("index").join(format!("{day}.md"));
		if has_entries || path.exists() {
			write_marked(&path, &format!("# {day}\n\n"), &rendered)?;
		}
	}

	let groups = groups
		.into_iter()
		.map(|group| {
			json!({
				"dir": group.dir,
				"title": group.title,
				"entries": group.entries.into_iter().map(|entry| json!({
					"title": entry.title,
					"href": entry.href,
					"published_date": entry.published_date,
				})).collect::<Vec<_>>(),
			})
		})
		.collect::<Vec<_>>();
	let rendered = env
		.render_str(
			&index_template,
			minijinja::value::Serde(json!({ "groups": groups })),
		)
		.map_err(|e| IndexError::Template(e.to_string()))?;
	write_marked(&root.join("INDEX.md"), "# Index\n\n", &rendered)?;
	Ok(())
}

fn entry_ref(entry: &EntryState, href: String, source: &str) -> EntryRef {
	EntryRef {
		title: markdown_escape_text(&entry.title),
		href,
		source: source.to_string(),
		published_date: entry
			.published_date
			.clone()
			.or_else(|| entry.published.clone())
			.unwrap_or_default(),
		published_ts: entry.published_ts,
		summary: entry.summary.clone(),
	}
}

fn load_sources(
	root: &Path,
	config: &Config,
) -> Result<Vec<SourceInfo>, IndexError> {
	let mut sources = Vec::new();
	for feed in &config.feeds {
		let dir = root.join(&feed.dir);
		let state = SourceState::load(&dir)
			.map_err(|e| IndexError::State(dir.clone(), e))?;
		if state.entries.is_empty() && state.feed_url.is_none() {
			continue;
		}
		let title = state.title.clone().unwrap_or_else(|| feed.dir.clone());
		let title = if title == feed.dir {
			title
		} else {
			format!("{title} ({})", feed.dir)
		};
		let title = markdown_escape_text(&title);
		sources.push(SourceInfo {
			dir: feed.dir.clone(),
			title,
			state,
		});
	}
	let misc = root.join("misc");
	let state = SourceState::load(&misc)
		.map_err(|e| IndexError::State(misc.clone(), e))?;
	if !state.entries.is_empty() {
		sources.push(SourceInfo {
			dir: "misc".to_string(),
			title: "misc".to_string(),
			state,
		});
	}
	Ok(sources)
}

fn load_template(
	root: &Path,
	name: &str,
	default: &str,
) -> Result<String, IndexError> {
	let path = root.join("templates").join(name);
	if path.exists() {
		fs::read_to_string(&path).map_err(|e| IndexError::TemplateFile(path, e))
	} else {
		Ok(default.to_string())
	}
}

fn write_marked(
	path: &Path,
	prelude: &str,
	rendered: &str,
) -> Result<(), IndexError> {
	let existing = fs::read_to_string(path).unwrap_or_default();
	let block = format!("{BEGIN}\n{}\n{END}\n", rendered.trim_end());
	let output = if let Some(begin) = existing.find(BEGIN) {
		if let Some(end_rel) = existing[begin..].find(END) {
			let end = begin + end_rel + END.len();
			format!("{}{}{}", &existing[..begin], block, &existing[end..])
		} else {
			format!("{existing}\n{block}")
		}
	} else if existing.is_empty() {
		format!("{prelude}{block}")
	} else {
		format!("{existing}\n{block}")
	};
	atomic_write(path, output.as_bytes())
		.map_err(|e| IndexError::Write(path.to_path_buf(), e))
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
	#[error("failed to read state for {0}: {1}")]
	State(PathBuf, crate::state::StateError),
	#[error("failed to read template {0}: {1}")]
	TemplateFile(PathBuf, std::io::Error),
	#[error("failed to render template: {0}")]
	Template(String),
	#[error("failed to write {0}: {1}")]
	Write(PathBuf, std::io::Error),
}
