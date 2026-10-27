use std::{fs, path::Path};

use assert_cmd::Command;
use httpmock::prelude::*;
use tempfile::TempDir;

fn write_config(root: &Path, feed_url: &str) {
	fs::write(
		root.join("subscriptions.toml"),
		format!(
			r#"
[[feeds]]
url = "{feed_url}"
dir = "blog"
tags = ["rust", "test"]
"#
		),
	)
	.unwrap();
}

fn rss(feed_url: &str, post_url: &str) -> String {
	format!(
		r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Blog</title>
    <link>{feed_url}</link>
    <description>Test feed</description>
    <item>
      <title>Hello World</title>
      <link>{post_url}</link>
      <guid isPermaLink="false">post-1</guid>
      <pubDate>Tue, 02 Jan 2024 12:00:00 GMT</pubDate>
      <author>alice@example.com (Alice)</author>
      <category>rust</category>
    </item>
  </channel>
</rss>"#
	)
}

fn article() -> &'static str {
	r#"<!doctype html>
<html>
  <head>
    <title>Hello World</title>
    <meta name="author" content="Alice">
    <meta property="article:published_time" content="2024-01-02T12:00:00Z">
  </head>
  <body>
    <article>
      <h1>Hello World</h1>
      <p>This is the body of the article. It has enough text to be extracted by readability, and it should become Markdown.</p>
      <p><img src="/img.png" alt="picture"></p>
    </article>
  </body>
</html>"#
}

fn scrawl(root: &Path) -> Command {
	let mut command = Command::cargo_bin("scrawl").unwrap();
	command.arg("-C").arg(root).env("TZ", "UTC");
	command
}

fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
	let mut out = Vec::new();
	let Ok(entries) = fs::read_dir(dir) else {
		return out;
	};
	for entry in entries.filter_map(Result::ok) {
		let path = entry.path();
		if path.is_dir() {
			out.extend(walk_files(&path));
		} else {
			out.push(path);
		}
	}
	out
}

fn article_files(dir: &Path) -> Vec<String> {
	fs::read_dir(dir)
		.unwrap()
		.filter_map(Result::ok)
		.filter_map(|entry| {
			let path = entry.path();
			(path.extension().and_then(|ext| ext.to_str()) == Some("md")).then(
				|| path.file_name().unwrap().to_string_lossy().into_owned(),
			)
		})
		.collect()
}

#[test]
fn sync_archives_article_images_state_and_indexes() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	let feed_mock = server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	let post_mock = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html; charset=utf-8")
			.body(article());
	});
	let image_mock = server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});
	write_config(root.path(), &feed_url);

	scrawl(root.path()).arg("sync").assert().success();

	feed_mock.assert_calls(1);
	post_mock.assert_calls(1);
	image_mock.assert_calls(1);

	let dir = root.path().join("blog");
	let files = article_files(&dir);
	assert_eq!(files, vec!["2024-01-02-hello-world.md"]);
	let markdown = fs::read_to_string(dir.join(&files[0])).unwrap();
	assert!(markdown.contains("title: Hello World"));
	assert!(markdown.contains("author: Alice"));
	assert!(markdown.contains("feed: "));
	assert!(markdown.contains("- rust"));
	assert!(markdown.contains(&format!("Source: <{post_url}>")));
	assert!(markdown.contains("This is the body of the article"));
	assert!(markdown.contains("](images/"));
	let image_count = walk_files(&dir.join("images")).len();
	assert_eq!(image_count, 1);
	assert!(dir.join(".simplecrawl.json").exists());

	let index = fs::read_to_string(root.path().join("INDEX.md")).unwrap();
	assert!(index.contains("## Test Blog (blog)"));
	assert!(index.contains("Hello World"));

	let daily = fs::read_dir(root.path().join("index"))
		.unwrap()
		.filter_map(Result::ok)
		.map(|entry| fs::read_to_string(entry.path()).unwrap())
		.collect::<Vec<_>>()
		.join("\n");
	assert!(daily.contains("Hello World"));
	assert!(daily.contains("· blog · 2024-01-02"));

	scrawl(root.path())
		.args(["list", "--json"])
		.assert()
		.success()
		.stdout(predicates::str::contains("\"dir\": \"blog\""));
	scrawl(root.path())
		.args(["sync", "missing"])
		.assert()
		.code(2);
}

#[test]
fn repeated_sync_does_not_duplicate_articles() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});
	write_config(root.path(), &feed_url);

	scrawl(root.path()).arg("sync").assert().success();
	scrawl(root.path()).arg("sync").assert().success();
	assert_eq!(article_files(&root.path().join("blog")).len(), 1);
}

#[test]
fn fetch_writes_misc_and_skips_duplicate_urls() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let post_url = server.url("/post");
	let post_mock = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	let image_mock = server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});

	scrawl(root.path())
		.arg("fetch")
		.arg(&post_url)
		.assert()
		.success();
	scrawl(root.path())
		.arg("fetch")
		.arg(&post_url)
		.assert()
		.success()
		.stdout(predicates::str::contains("already fetched"));

	post_mock.assert_calls(1);
	image_mock.assert_calls(1);
	assert_eq!(article_files(&root.path().join("misc")).len(), 1);
	assert!(root.path().join("misc/.simplecrawl.json").exists());
	let index = fs::read_to_string(root.path().join("INDEX.md")).unwrap();
	assert!(index.contains("Hello World"));
}

#[test]
fn index_rebuild_preserves_manual_content_outside_markers() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});
	write_config(root.path(), &feed_url);
	scrawl(root.path()).arg("sync").assert().success();

	let index_path = root.path().join("INDEX.md");
	let mut index = fs::read_to_string(&index_path).unwrap();
	index.push_str("\nMy manual note\n");
	fs::write(&index_path, index).unwrap();
	scrawl(root.path()).arg("index").assert().success();
	let index = fs::read_to_string(&index_path).unwrap();
	assert!(index.contains("My manual note"));
	assert!(index.contains("Hello World"));

	fs::create_dir_all(root.path().join("templates")).unwrap();
	fs::write(
		root.path().join("templates/index.md.j2"),
		"# Custom\n{% for group in groups %}CUSTOM {{ group.title }}\n{% endfor %}",
	)
	.unwrap();
	scrawl(root.path()).arg("index").assert().success();
	let index = fs::read_to_string(&index_path).unwrap();
	assert!(index.contains("CUSTOM Test Blog (blog)"));
}

#[test]
fn hooks_and_summaries_run_after_download() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});
	fs::write(
		root.path().join("subscriptions.toml"),
		format!(
			r#"
[hooks]
post_download = "printf '\nHOOKED\n' >> {{file}}"

[summarize]
command = "printf 'A concise summary'"

[runtime]
per_host_delay_ms = 0
retry_base_ms = 1

[[feeds]]
url = "{feed_url}"
dir = "blog"
"#
		),
	)
	.unwrap();

	scrawl(root.path()).arg("sync").assert().success();
	let dir = root.path().join("blog");
	let files = article_files(&dir);
	assert_eq!(files.len(), 1);
	let markdown = fs::read_to_string(dir.join(&files[0])).unwrap();
	assert!(markdown.contains("HOOKED"));
	let daily = fs::read_dir(root.path().join("index"))
		.unwrap()
		.filter_map(Result::ok)
		.map(|entry| fs::read_to_string(entry.path()).unwrap())
		.collect::<Vec<_>>()
		.join("\n");
	assert!(daily.contains("A concise summary"));
}

#[test]
fn mark_failed_persists_ignored_entries_and_retries_then_skips() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	let post_mock = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(500).body("nope");
	});
	fs::write(
		root.path().join("subscriptions.toml"),
		format!(
			r#"
[runtime]
per_host_delay_ms = 0
retry_base_ms = 1
download_concurrency = 1

[[feeds]]
url = "{feed_url}"
dir = "blog"
"#
		),
	)
	.unwrap();

	scrawl(root.path())
		.args(["sync", "--mark-failed"])
		.assert()
		.code(1);
	assert_eq!(post_mock.calls(), 6);
	let state =
		fs::read_to_string(root.path().join("blog/.simplecrawl.json")).unwrap();
	assert!(state.contains("\"ignored\""));
	assert!(state.contains(&post_url));
	scrawl(root.path())
		.args(["sync", "--mark-failed"])
		.assert()
		.success();
	assert_eq!(post_mock.calls(), 6);
}

#[test]
fn dry_run_does_not_fetch_articles_or_write_state() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	server.mock(|when, then| {
		when.method(GET).path("/feed.xml");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.body(&rss);
	});
	let post_mock = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	write_config(root.path(), &feed_url);

	scrawl(root.path())
		.args(["sync", "--dry-run"])
		.assert()
		.success();
	assert_eq!(post_mock.calls(), 0);
	assert!(article_files(&root.path().join("blog")).is_empty());
	assert!(!root.path().join("blog/.simplecrawl.json").exists());
	assert!(!root.path().join("INDEX.md").exists());
}

#[test]
fn article_failures_force_feed_revalidation() {
	let server = MockServer::start();
	let root = TempDir::new().unwrap();
	let feed_url = server.url("/feed.xml");
	let post_url = server.url("/post");
	let rss = rss(&feed_url, &post_url);
	let feed_ok = server.mock(|when, then| {
		when.method(GET)
			.path("/feed.xml")
			.header_missing("if-none-match");
		then.status(200)
			.header("content-type", "application/rss+xml")
			.header("etag", "\"v1\"")
			.body(&rss);
	});
	let feed_not_modified = server.mock(|when, then| {
		when.method(GET)
			.path("/feed.xml")
			.header("if-none-match", "\"v1\"");
		then.status(304);
	});
	let mut post_fail = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(500).body("nope");
	});
	fs::write(
		root.path().join("subscriptions.toml"),
		format!(
			r#"
[runtime]
per_host_delay_ms = 0
retry_base_ms = 1
download_concurrency = 1

[[feeds]]
url = "{feed_url}"
dir = "blog"
"#
		),
	)
	.unwrap();

	scrawl(root.path()).arg("sync").assert().code(1);
	post_fail.delete();
	let post_ok = server.mock(|when, then| {
		when.method(GET).path("/post");
		then.status(200)
			.header("content-type", "text/html")
			.body(article());
	});
	server.mock(|when, then| {
		when.method(GET).path("/img.png");
		then.status(200)
			.header("content-type", "image/png")
			.body(b"\x89PNG\r\n\x1a\n");
	});

	scrawl(root.path()).arg("sync").assert().success();
	assert_eq!(post_ok.calls(), 1);
	assert_eq!(feed_not_modified.calls(), 0);
	assert!(feed_ok.calls() >= 2);
	assert_eq!(article_files(&root.path().join("blog")).len(), 1);
}

#[test]
fn help_prints_standard_help_then_embedded_user_guide() {
	let output = Command::cargo_bin("scrawl")
		.unwrap()
		.arg("help")
		.output()
		.unwrap();
	assert!(output.status.success());
	let text = String::from_utf8(output.stdout).unwrap();
	let usage = text.find("Usage: scrawl").expect("standard help");
	let guide = text
		.find("# scrawl user guide")
		.expect("embedded user guide");
	assert!(usage < guide, "the guide should follow standard help");
	assert!(text.contains("## What scrawl does"));

	let output = Command::cargo_bin("scrawl")
		.unwrap()
		.args(["help", "sync"])
		.output()
		.unwrap();
	assert!(output.status.success());
	let text = String::from_utf8(output.stdout).unwrap();
	assert!(text.contains("Usage: sync"));
	assert!(!text.contains("# scrawl user guide"));
}
