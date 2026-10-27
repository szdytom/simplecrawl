use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::util::atomic_write;

pub const STATE_FILE: &str = ".simplecrawl.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryState {
	pub title: String,
	pub url: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub author: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub published: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub published_date: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub published_ts: Option<i64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub updated: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
	pub path: String,
	pub downloaded_on: String,
	pub downloaded_at: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IgnoredEntry {
	pub key: String,
	pub url: String,
	pub title: String,
	pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceState {
	#[serde(default = "state_version")]
	pub version: u32,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub feed_url: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub title: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub etag: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub last_modified: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub last_sync: Option<String>,
	#[serde(default)]
	pub entries: BTreeMap<String, EntryState>,
	#[serde(default)]
	pub ignored: Vec<IgnoredEntry>,
}

const fn state_version() -> u32 {
	1
}

impl Default for SourceState {
	fn default() -> Self {
		Self {
			version: state_version(),
			feed_url: None,
			title: None,
			etag: None,
			last_modified: None,
			last_sync: None,
			entries: BTreeMap::new(),
			ignored: Vec::new(),
		}
	}
}

impl SourceState {
	pub fn load(source_dir: &Path) -> Result<Self, StateError> {
		let path = source_dir.join(STATE_FILE);
		let data = match fs::read(&path) {
			Ok(data) => data,
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
				return Ok(Self::default());
			}
			Err(err) => return Err(StateError::Io(path, err)),
		};
		let state: Self = serde_json::from_slice(&data)
			.map_err(|e| StateError::Json(path, e))?;
		if state.version != state_version() {
			return Err(StateError::Version(state.version));
		}
		Ok(state)
	}

	pub fn save(&self, source_dir: &Path) -> Result<(), StateError> {
		let path = source_dir.join(STATE_FILE);
		let mut data = serde_json::to_vec_pretty(self)
			.map_err(|e| StateError::Json(path.clone(), e))?;
		data.push(b'\n');
		atomic_write(&path, &data).map_err(|e| StateError::Io(path, e))
	}

	pub fn is_known(&self, key: &str) -> bool {
		self.entries.contains_key(key)
			|| self.ignored.iter().any(|entry| entry.key == key)
	}

	pub fn mark_ignored(
		&mut self,
		key: String,
		url: String,
		title: String,
		reason: String,
	) {
		if self.ignored.iter().any(|entry| entry.key == key) {
			return;
		}
		self.ignored.push(IgnoredEntry {
			key,
			url,
			title,
			reason,
		});
	}
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
	#[error("failed to read or write {0}: {1}")]
	Io(PathBuf, std::io::Error),
	#[error("invalid state file {0}: {1}")]
	Json(PathBuf, serde_json::Error),
	#[error("unsupported state version {0}")]
	Version(u32),
}
