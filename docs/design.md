# scrawl design constraints

Status: agreed. This document is the source of truth for scope and behavior.
Implementation details may change, but changing a constraint here requires an
explicit decision.

## 1. Goal and scope

`scrawl` is a small Unix CLI that:

1. Subscribes to RSS, Atom, and JSON feeds listed in a hand-written TOML file.
2. Downloads article pages incrementally.
3. Extracts the main article content and converts it to Markdown.
4. Archives everything under a normal filesystem directory that the user can
   inspect, edit, sync, or version-control.

Non-goals for v1:

- Windows support
- feed discovery from a website homepage
- OPML import/export
- built-in authentication flows or a cookie jar
- obeying `robots.txt`
- JavaScript rendering
- built-in summarization or LLM calls
- a database server or GUI

## 2. Platform and project shape

- Unix only: Linux and macOS. The hook runner uses `sh -c`.
- Rust edition 2024, current stable, MSRV 1.89. No MSRV promise beyond that.
- Workspace with two crates:
  - `simplecrawl-core`: configuration, feed parsing, HTTP, extraction, state, indexes.
  - `simplecrawl`: package for the CLI binary named `scrawl`.
- License: GPL-2.0-or-later.
- Documentation, CLI help, and error messages are English.

## 3. Root directory and reserved names

The archive root is the current directory, or the directory passed with
`-C <dir>`. `scrawl` never performs an upward search for a root.

Reserved names at the root:

```text
subscriptions.toml
INDEX.md
index/
misc/
templates/
.simplecrawl.lock
```

A feed `dir` is required, must be a single plain path segment, must stay inside
the root, and cannot be a reserved name (case-insensitive). It cannot start with
`#` or `^`, because those prefixes are selector syntax.

## 4. CLI surface

```text
scrawl sync [SELECTOR...] [--dry-run] [--limit N] [--mark-failed]
scrawl list [--json]
scrawl fetch <URL>
scrawl index
```

Global options: `-C <dir>`, `-v/--verbose`, `-q/--quiet`.

`scrawl help` prints the standard command help followed by the full embedded
user guide. `scrawl --help` remains the normal concise help.

There is no `add`, `remove`, `init`, or `--force`. Subscriptions are edited by
hand in `subscriptions.toml`.

Exit codes:

- `0`: success
- `1`: partial failure or runtime failure
- `2`: usage or configuration error

## 5. Configuration

`subscriptions.toml` is hand-written and read-only to the tool. Unknown fields
are rejected. The shape is:

```toml
[headers]
User-Agent = "Mozilla/5.0 ..."
Authorization = "Bearer ${BLOG_TOKEN}"

[hooks]
post_download = "some-formatter {file}"

[summarize]
command = "my-summarizer {file}"

[runtime]
download_concurrency = 8
hook_concurrency = 2
http_timeout_secs = 30
hook_timeout_secs = 30
summary_timeout_secs = 60
per_host_delay_ms = 200
retry_base_ms = 1000

[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"
tags = ["rust", "tech"]
after = "2024-01-01"

[feeds.headers]
Referer = "https://example.com/"

[feeds.hooks]
post_download = "other-formatter {file}"

[feeds.summarize]
command = "other-summarizer {file}"

[feeds.extract]
selector = "article.post-content"
```

Rules:

- `url` and `dir` are required. Duplicate dirs and duplicate feed URLs are
  rejected.
- `tags` belong to the subscription and are used only by `sync` selectors.
  They are not copied into article frontmatter.
- `after` is an optional per-feed inclusive `YYYY-MM-DD` cutoff. Entries
  published before it, and entries without a publication or update date, are
  skipped.
- `extract.selector` is an optional per-feed CSS selector. When present, the
  article body is restricted to the matching subtrees before readability
  scoring; invalid selectors are configuration errors and empty matches fail
  the article.
- Headers are global by default and per-feed override. Header names are compared
  case-insensitively.
- Header values support `${ENV_VAR}` expansion. `$$` is a literal `$`. A missing
  variable is a configuration error.
- Runtime concurrency and timeouts must be greater than zero.
- TLS extra trust anchors live in a separate global XDG config file, not in
  `subscriptions.toml`.

## 6. Feed handling

- Feed URLs must be direct HTTP(S) feed URLs. A website homepage is not
  autodiscovered.
- Supported formats are RSS 0.9/1.0/2.0, Atom, and JSON Feed via `feed-rs`.
- First sync processes all entries currently in the feed unless the feed sets
  `after`. `--limit N` processes at most the newest N new entries per feed.
- Later syncs process entries not already recorded as downloaded or ignored.
- Feed requests send `If-None-Match` and `If-Modified-Since` when state has
  validators. A `304 Not Modified` means no new entries.
- If any article fails, or `--limit` leaves entries unprocessed, the previous
  feed validators are restored so the next sync cannot skip those entries via a
  `304`.
- Entry identity for deduplication is chosen in this order:
  1. feed `id`/GUID, unless it looks like a generated UUID
  2. normalized article URL
  3. hash of title plus publication time
- Deduplication is per source directory.
- The frontmatter/Source URL is the raw feed link, not the post-redirect URL.
  Relative links in the page are resolved against the final response URL.

## 7. Networking

- HTTP client: `reqwest` with `tokio` and rustls.
- Global concurrency is capped by `download_concurrency` (default 8).
- The same host is limited to one in-flight request, with
  `per_host_delay_ms` (default 200 ms) between requests.
- Transient failures are retried up to five times with exponential backoff,
  honoring `Retry-After`. HTTP 4xx responses are not retried.
- Request timeout is `http_timeout_secs`.
- Default User-Agent is `scrawl/<version>`. Proxy environment variables are
  honored by `reqwest`.
- A global `$XDG_CONFIG_HOME/scrawl/config.toml` may set `tls_extra_roots` to
  a PEM bundle of additional trust anchors. The file is optional; relative
  paths resolve from its own directory.
- Custom headers are applied to feed, article, and image requests.
- `robots.txt` is not consulted.
- Response bodies are capped at 20 MiB.

## 8. Article extraction and rendering

- Article pages are fetched and parsed with `dom_smoothie` in Markdown mode.
- A per-feed `extract.selector` first replaces the document body with the
  matching subtree HTML, so content outside is never scored. Content inside the
  subtree still goes through normal readability pruning. Multiple matches are
  merged once per match; nested matches are included once.
- Feed-provided `content`/`summary` is **not** used as a fallback. If the article
  page cannot be fetched or no readable content is found, the article fails.
- The page title is only a fallback; the feed entry title has priority.
- If the extracted body starts with an H1 equal to the article title, that
  duplicate H1 is removed.
- Markview-style math and empty-table preservation is deferred. A
  `post_download` command can handle site-specific cleanup.
- Article titles: feed title, then extracted title, then URL.
- Dates: feed `published`, then feed `updated`, then page metadata, then the
  download time.
- Filenames and daily-index grouping use the local timezone. Frontmatter dates
  are RFC 3339 with a local offset.

Article file skeleton:

```markdown
---
title: Some Post
url: https://example.com/posts/some-post
author: Alice
published: 2026-10-06T12:34:56+08:00
updated: 2026-10-07T08:00:00+08:00
feed: https://example.com/feed.xml
tags:
- rust
---

# Some Post

Source: <https://example.com/posts/some-post>

Article body...
```

- Frontmatter fields present: `title`, `url`, `author`, `published`, `updated`,
  `feed`, `tags`. Optional fields are omitted when absent.
- `tags` in frontmatter come from feed entry categories only.
- `fetch` output omits `feed`.
- File names are `YYYY-MM-DD-<title-slug>.md`. Unicode letters/digits are
  preserved; whitespace and punctuation collapse to `-`; slugs are truncated to
  about 180 bytes; empty titles fall back to `untitled`.
- Name collisions append `-2`, `-3`, and so on.
- Article files are written once and never overwritten. Manually created,
  edited, renamed, or deleted files are left alone.

## 9. Images

- Images referenced by extracted Markdown are downloaded to
  `<source>/images/<hash-prefix>/<hash>.<ext>`.
- The hash is derived from the normalized image URL. Files are deduplicated by
  URL.
- Markdown image links are rewritten to the relative local path.
- Only `image/*` responses are saved. Individual images are capped at 20 MiB.
- If an image fails, or returns a non-image response, the original remote URL
  is kept and a warning is printed.
- `data:` URIs are left in place.

## 10. State and locking

Each source directory has one hidden `.simplecrawl.json`:

```text
version
feed_url
title
etag
last_modified
last_sync
entries: <entry-key> -> entry record
ignored: [entry records]
```

An entry record contains title, URL, author, published/updated values, tags,
source-relative Markdown path, local downloaded date/time, and optional summary.

Rules:

- State is written with a temporary file plus atomic rename. mmap is not used.
- State is written after each successfully written article.
- A root `.simplecrawl.lock` is held for every mutating command (`sync`,
  `fetch`, `index`). A second process exits immediately.
- If the configured feed URL differs from the URL recorded in state, that source
  stops with an error and requires human resolution.
- Article failures are not persisted. Their feed validators are preserved so
  they are retried on the next sync.
- `sync --mark-failed` writes the current run's article failures into `ignored`.
  The run still exits 1. Unignoring means editing the state file.
- Manual Markdown files are never read as input, never overwritten, and never
  deleted.

## 11. Hooks and summaries

- `post_download` runs after the article and images are written.
- `summarize` runs after `post_download`.
- Both use `sh -c` with the archive root as working directory.
- Placeholders are shell-escaped automatically:
  `{file}`, `{root}`, `{source}`, `{url}`, `{title}`.
- The same values are injected as `SIMPLECRAWL_FILE`, `SIMPLECRAWL_ROOT`,
  `SIMPLECRAWL_SOURCE`, `SIMPLECRAWL_URL`, and `SIMPLECRAWL_TITLE`.
- Hook and summary commands are limited by `hook_concurrency`.
- Hook failures are warnings; the article is kept.
- Summary output is whitespace-collapsed to one line and truncated to about
  200 characters.
- Summaries are cached in state and never expire.
- Missing summaries are retried on later feed syncs.
- If the summarize command itself changes, old summaries remain.

## 12. Indexes and templates

- State is the only source of truth for indexes. Manual Markdown files are not
  scanned into indexes.
- Daily index: `index/YYYY-MM-DD.md`, grouped by download date, flat timeline
  sorted by publication date, newest first.
  Line shape: `- [title](<path>) · source · published-date — summary`.
- Global index: `INDEX.md`, source sections in `subscriptions.toml` order,
  `misc` last. Each source lists its articles newest first.
- Every sync rebuilds the current day's daily index and `INDEX.md`.
- `scrawl index` rebuilds every daily index and `INDEX.md`.
- Generated content is wrapped in:
  `<!-- simplecrawl:begin -->` and `<!-- simplecrawl:end -->`.
  Only that block is rewritten; user content outside it is preserved.
- Optional templates live at `templates/daily.md.j2` and
  `templates/index.md.j2`, rendered with `minijinja`.
- Daily template context: `date`, `entries[title, href, source,
  published_date, summary]`.
- Global template context: `groups[dir, title, entries[title, href,
  published_date]]`.
- Index links use paths relative to the index file and angle brackets.
- If a state entry points at a missing Markdown file, the index keeps the link.
  Dead links are allowed; the tool never re-downloads a file the user deleted.

## 13. Selectors

- `sync` with no selectors processes all feeds.
- Plain `foo` matches a feed `dir` exactly.
- `#tag` matches a configured subscription tag.
- `^value` excludes matches by dir or tag; `^#tag` excludes by tag only.
- Positive selectors form a union. Exclusions form a union and are subtracted:
  `A \ B`.
- Only exclusions means "all feeds minus exclusions".
- If no feed matches a non-empty selector set, the command exits 2.
- `#tag` normally needs shell quoting.

## 14. Fetch (`misc`)

- `scrawl fetch <url>` requires no subscription and writes to `misc/`.
- It reuses global headers, hooks, summarize, and runtime settings when
  `subscriptions.toml` exists.
- `misc/` has its own `.simplecrawl.json`.
- Repeated URLs are skipped using the same normalized-URL key.
- Fetched articles still enter the daily index and global `INDEX.md` under
  `misc`.

## 15. Dry-run and limiting

- `sync --dry-run` fetches feeds to determine new entries, but does not fetch
  article pages or images, does not run hooks or summaries, and does not write
  article, state, or index files.
- `--limit N` is per feed, applies after known/ignored entries are filtered out,
  and takes the newest N by publication time.
- `--limit 0` is rejected.

## 16. Testing, CI, and documentation

- Integration tests use `httpmock` and `tempfile`; the main flows are exercised
  through the real `scrawl` binary.
- Covered flows include: feed sync, images, frontmatter, indexes, manual index
  content, templates, fetch deduplication, hooks, summaries, dry-run,
  `--mark-failed`, retries, and validator restoration after failures.
- Small unit tests cover selectors, slugs, quoting, and template substitution.
- Required checks: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- GitHub Actions runs the checks on Ubuntu only.
- Deliverables include README, CHANGELOG, LICENSE, and this design document.

## 17. Explicitly rejected alternatives

- SQLite or a single global state JSON instead of per-source JSON
- mmap for state writes
- `add`/`remove`/`init` commands; configuration is hand-edited
- overwriting or updating previously downloaded article files
- using feed content as an article fallback
- `--force` re-downloading
- automatic feed discovery
- Windows support
- robots.txt parsing
- built-in LLM summarization
- source selectors by feed URL
- rebuilding all historical daily indexes on every sync
- deleting or re-downloading files that the user removed
