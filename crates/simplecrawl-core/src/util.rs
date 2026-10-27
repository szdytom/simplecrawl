use jiff::{Timestamp, tz::TimeZone};
use sha2::{Digest, Sha256};
use std::{
	collections::BTreeMap,
	fs,
	io::Write as _,
	path::Path,
	sync::atomic::{AtomicU64, Ordering},
	time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn hash_hex(value: &str) -> String {
	let digest = Sha256::digest(value.as_bytes());
	hex::encode(digest)
}

pub fn short_hash(value: &str, len: usize) -> String {
	hash_hex(value).chars().take(len).collect()
}

pub fn normalize_url(value: &str) -> String {
	let Ok(mut url) = Url::parse(value) else {
		return value.trim().to_string();
	};
	url.set_fragment(None);
	let _ = url.set_scheme(&url.scheme().to_ascii_lowercase());
	if let Some(host) = url.host_str() {
		let host = host.to_ascii_lowercase();
		let _ = url.set_host(Some(&host));
	}
	if let Some(port) = url.port() {
		let default = match url.scheme() {
			"http" => 80,
			"https" => 443,
			_ => 0,
		};
		if port == default {
			let _ = url.set_port(None);
		}
	}
	url.to_string()
}

pub fn slugify(title: &str) -> String {
	let mut out = String::new();
	let mut dash = false;
	for ch in title.chars() {
		if ch.is_alphanumeric() {
			if dash && !out.is_empty() {
				out.push('-');
			}
			out.push(ch.to_lowercase().next().unwrap_or(ch));
			dash = false;
		} else if ch.is_whitespace()
			|| ch.is_ascii_punctuation()
			|| !ch.is_alphanumeric()
		{
			dash = true;
		}
	}
	let out = truncate_bytes(out.trim_matches('-'), 180)
		.trim_matches('-')
		.to_string();
	if out.is_empty() {
		"untitled".to_string()
	} else {
		out
	}
}

pub fn truncate_bytes(value: &str, max: usize) -> &str {
	if value.len() <= max {
		return value;
	}
	let mut end = max;
	while !value.is_char_boundary(end) {
		end -= 1;
	}
	&value[..end]
}

pub fn one_line(value: &str) -> String {
	value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn truncate_chars(value: &str, max: usize) -> String {
	if value.chars().count() <= max {
		return value.to_string();
	}
	let mut out: String = value.chars().take(max).collect();
	out.push('…');
	out
}

pub fn now_unix() -> i64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

pub fn local_zoned(secs: i64) -> jiff::Zoned {
	Timestamp::from_second(secs)
		.expect("timestamps from feeds are in range")
		.to_zoned(TimeZone::system())
}

pub fn local_date(secs: i64) -> String {
	local_zoned(secs).strftime("%Y-%m-%d").to_string()
}

pub fn local_rfc3339(secs: i64) -> String {
	local_zoned(secs)
		.strftime("%Y-%m-%dT%H:%M:%S%:z")
		.to_string()
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
	let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
	let tmp =
		path.with_file_name(format!(".{name}.tmp-{}-{n}", std::process::id()));
	{
		let mut file = fs::File::create(&tmp)?;
		file.write_all(bytes)?;
		file.sync_all()?;
	}
	match fs::rename(&tmp, path) {
		Ok(()) => Ok(()),
		Err(err) => {
			let _ = fs::remove_file(&tmp);
			Err(err)
		}
	}
}

pub fn shell_quote(value: &str) -> String {
	let mut out = String::with_capacity(value.len() + 2);
	out.push('\'');
	for ch in value.chars() {
		if ch == '\'' {
			out.push_str("'\\''");
		} else {
			out.push(ch);
		}
	}
	out.push('\'');
	out
}

pub fn fill_template(template: &str, values: &BTreeMap<&str, &str>) -> String {
	let mut out = String::with_capacity(template.len());
	let mut rest = template;
	while let Some(start) = rest.find('{') {
		out.push_str(&rest[..start]);
		let after = &rest[start + 1..];
		if let Some(end) = after.find('}') {
			let key = &after[..end];
			if let Some(value) = values.get(key) {
				out.push_str(&shell_quote(value));
				rest = &after[end + 1..];
				continue;
			}
		}
		out.push('{');
		rest = after;
	}
	out.push_str(rest);
	out
}

pub fn markdown_escape_text(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for ch in value.chars() {
		match ch {
			'\n' | '\r' | '\t' => out.push(' '),
			'[' | ']' => {
				out.push('\\');
				out.push(ch);
			}
			_ => out.push(ch),
		}
	}
	out
}

pub fn ensure_relative_segment(value: &str) -> bool {
	!value.is_empty()
		&& !value.starts_with('.')
		&& !value.contains('/')
		&& !value.contains('\\')
		&& !value.contains('\0')
}

pub fn relative_path(from_dir: &Path, to: &Path) -> String {
	let from = from_dir.components().collect::<Vec<_>>();
	let to = to.components().collect::<Vec<_>>();
	let mut common = 0;
	while common < from.len() && common < to.len() && from[common] == to[common]
	{
		common += 1;
	}
	let mut parts = Vec::new();
	for _ in common..from.len() {
		parts.push("..".to_string());
	}
	for component in &to[common..] {
		parts.push(component.as_os_str().to_string_lossy().into_owned());
	}
	parts.join("/")
}

pub fn source_file_name(
	date: &str,
	title: &str,
	taken: &mut BTreeMap<String, usize>,
) -> String {
	let slug = slugify(title);
	let base = format!("{date}-{slug}");
	let count = taken.entry(base.clone()).or_insert(0);
	*count += 1;
	if *count == 1 {
		format!("{base}.md")
	} else {
		format!("{base}-{count}.md")
	}
}

pub fn choose_file_name(dir: &Path, date: &str, title: &str) -> String {
	let base = format!("{date}-{}", slugify(title));
	let mut n = 1;
	loop {
		let name = if n == 1 {
			format!("{base}.md")
		} else {
			format!("{base}-{n}.md")
		};
		if !dir.join(&name).exists() {
			return name;
		}
		n += 1;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slug_keeps_unicode_and_collapses_punctuation() {
		assert_eq!(slugify("Hello, World!"), "hello-world");
		assert_eq!(slugify("中文 标题 / Test"), "中文-标题-test");
	}

	#[test]
	fn shell_quote_handles_single_quotes() {
		assert_eq!(shell_quote("a'b"), "'a'\\''b'");
	}

	#[test]
	fn template_values_are_quoted_and_unknown_braces_survive() {
		let mut values = BTreeMap::new();
		values.insert("file", "/tmp/a b.md");
		assert_eq!(
			fill_template("fmt {file} {unknown} {1..2}", &values),
			"fmt '/tmp/a b.md' {unknown} {1..2}"
		);
	}
}
