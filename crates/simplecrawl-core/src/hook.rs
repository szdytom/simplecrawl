use std::{collections::BTreeMap, path::Path, time::Duration};

use tokio::process::Command;

use crate::util::fill_template;

pub struct CommandContext<'a> {
	pub root: &'a Path,
	pub source: &'a str,
	pub file: &'a Path,
	pub url: &'a str,
	pub title: &'a str,
}

pub async fn run_command(
	template: &str,
	context: &CommandContext<'_>,
	timeout: Duration,
) -> Result<String, CommandError> {
	let file = context.file.to_string_lossy();
	let root = context.root.to_string_lossy();
	let mut values = BTreeMap::new();
	values.insert("file", file.as_ref());
	values.insert("source", context.source);
	values.insert("url", context.url);
	values.insert("title", context.title);
	values.insert("root", root.as_ref());
	let command = fill_template(template, &values);

	let mut child = Command::new("sh");
	child
		.arg("-c")
		.arg(&command)
		.current_dir(context.root)
		.env("SIMPLECRAWL_FILE", context.file)
		.env("SIMPLECRAWL_ROOT", context.root)
		.env("SIMPLECRAWL_SOURCE", context.source)
		.env("SIMPLECRAWL_URL", context.url)
		.env("SIMPLECRAWL_TITLE", context.title)
		.kill_on_drop(true)
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped());
	let output = tokio::time::timeout(timeout, child.output())
		.await
		.map_err(|_| CommandError::Timeout)?
		.map_err(CommandError::Io)?;
	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);
		return Err(CommandError::Status(
			output.status.code().unwrap_or(-1),
			stderr.trim().to_string(),
		));
	}
	Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
	#[error("command timed out")]
	Timeout,
	#[error("failed to run command: {0}")]
	Io(std::io::Error),
	#[error("command exited with {0}: {1}")]
	Status(i32, String),
}
