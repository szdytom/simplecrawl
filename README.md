# scrawl

`scrawl` is a small Unix command-line tool that subscribes to RSS, Atom, and
JSON feeds, downloads each article, extracts its main content, and stores it as
local Markdown. It is meant for personal archiving: files are written once and
never overwritten, so edits and deletions you make by hand are respected.

See [`docs/user-guide.md`](docs/user-guide.md) for the detailed user guide and
[`docs/design.md`](docs/design.md) for the agreed design constraints.

## Features

- RSS 0.9/1.0/2.0, Atom, and JSON Feed through `feed-rs`
- Article extraction with `dom_smoothie` (Readability-style Markdown output)
- Incremental sync with ETag / Last-Modified validators
- Local image downloads with URL-hash deduplication
- YAML frontmatter, a `# Title` heading, and a Markdown `Source: <url>` line
- Daily indexes plus a global `INDEX.md`, optionally rendered from templates
- External `post_download` and `summarize` commands
- Tag and exclusion selectors for `sync`
- `fetch` for one-off articles in `misc/` without a subscription
- Atomic state writes and a root lock to prevent concurrent runs

## Install

```sh
cargo install --path crates/simplecrawl
```

The package is called `simplecrawl`; the installed binary is `scrawl`. After a
crates.io release, `cargo install simplecrawl` installs the same binary.

Requires a recent stable Rust toolchain (edition 2024). Linux and macOS are
supported. Windows is not supported; use WSL if needed.

## Quick start

Create an archive root and a `subscriptions.toml` by hand:

```toml
[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"
tags = ["tech", "rust"]
```

Then run:

```sh
scrawl -C /path/to/archive sync
```

The first sync downloads every entry currently in the feed. Later syncs only
download entries that are not already known.

## Layout

```text
archive/
  subscriptions.toml
  INDEX.md
  index/
    2026-10-06.md
  templates/                  # optional
    daily.md.j2
    index.md.j2
  example/
    .simplecrawl.json
    2026-10-06-some-post.md
    images/
      a1/
        a1b2c3d4e5f60718.png
  misc/
    .simplecrawl.json
    2026-10-06-page-title.md
```

Reserved names at the root: `subscriptions.toml`, `INDEX.md`, `index/`,
`templates/`, `misc/`, and `.simplecrawl.lock`. A feed's `dir` must be a single
plain path segment and cannot use a reserved name.

## Commands

```text
scrawl sync [SELECTOR...] [--dry-run] [--limit N] [--mark-failed]
scrawl list [--json]
scrawl fetch <URL>
scrawl index
```

Global options: `-C <dir>`, `-v/--verbose`, `-q/--quiet`.

### sync

- Defaults to every configured feed.
- `SELECTOR` matches a feed `dir` exactly, `#tag` matches configured tags, and
  `^value` excludes matching dirs or tags.
- Positive selectors form a union; exclusions are subtracted:
  `scrawl sync foo '#rust' '^old'` means `(foo OR #rust) - old`.
- Quote `#tag` in the shell so it is not treated as a comment.
- `--limit N` processes at most N new articles per selected feed.
- `--dry-run` fetches feeds to report new entries, but does not fetch articles,
  download images, run hooks, or write state/index files.
- `--mark-failed` adds this run's failed articles to the source's ignored list.
  The ignored articles are skipped on later syncs. The command still exits 1
  because the articles failed to download.

### list

Prints `DIR`, `TITLE`, `TAGS`, `ARTICLES`, `IGNORED`, `LAST SYNC`, and `URL`.
Use `--json` for machine-readable output.

### fetch

Downloads one article into `misc/` without requiring a subscription. It still
uses the global `[headers]`, `[hooks]`, `[summarize]`, and `[runtime]` settings
when `subscriptions.toml` exists, records the URL in `misc/.simplecrawl.json`,
and updates the indexes. A repeated URL is skipped.

### index

Rebuilds every daily index file and `INDEX.md` from state. Manual content
outside the generated markers is preserved.

## Configuration

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

[feeds.headers]
Referer = "https://example.com/"

[feeds.hooks]
post_download = "other-formatter {file}"

[feeds.summarize]
command = "other-summarizer {file}"
```

- `url` and `dir` are required. `dir` must be a single path segment.
- `tags` are used only by `sync` selectors. Article frontmatter tags come from
  the feed's own categories.
- Custom headers are applied to feed, article, and image requests. Per-feed
  headers override global headers with the same name.
- Header values support `${ENV_VAR}` expansion; `$$` produces a literal `$`.
  A missing environment variable is a configuration error.
- `robots.txt` is not consulted. The default User-Agent is
  `scrawl/<version>`. Standard proxy environment variables are honored by
  `reqwest`.
- Transient failures are retried up to five times with exponential backoff.
  `Retry-After` is honored. HTTP 4xx responses are not retried.

A global file at `$XDG_CONFIG_HOME/scrawl/config.toml` (fallback
`~/.config/scrawl/config.toml`) can add extra TLS trust anchors:

```toml
tls_extra_roots = "certs/extra-roots.pem"
```

Relative paths are resolved from that config file's directory. This is useful
for servers with an incomplete certificate chain; include the missing
intermediate certificate in the PEM file. See the
[user guide](docs/user-guide.md#global-configuration) for details.

## Article files

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
- tech
---

# Some Post

Source: <https://example.com/posts/some-post>

Article body...
```

- The title comes from the feed entry first, then the extracted page title, then
  the article URL.
- The date comes from `published`, then `updated`, then the page metadata, then
  the download date. Filenames and `downloaded_on` use the local timezone;
  frontmatter dates use RFC 3339 with a local offset.
- If the extracted body starts with an H1 matching the article title, that
  duplicate heading is removed.
- Files are named `YYYY-MM-DD-<title-slug>.md`. Unicode letters and digits are
  preserved. Collisions get `-2`, `-3`, and so on.
- Files are never overwritten. A manually created, edited, renamed, or deleted
  file is left alone. If a state entry points at a missing file, the index keeps
  the link, so a dead link is possible.

## Images

Images in the extracted Markdown are downloaded to
`<source>/images/<hash-prefix>/<hash>.<ext>`. The Markdown link is rewritten to
the relative path. Images are deduplicated by URL. Data URIs are left in place.
If an image download fails, or the response is not an image, the original remote
URL is kept and a warning is printed.

## Hooks and summaries

`post_download` runs after an article and its images are written. `summarize`
runs after that. Both use `sh -c` with the archive root as the working directory.

Placeholders are shell-escaped automatically:

- `{file}` - absolute path to the Markdown file
- `{root}` - absolute archive root
- `{source}` - source directory name (`misc` for `fetch`)
- `{url}` - article URL
- `{title}` - article title

The same values are available as `SIMPLECRAWL_FILE`, `SIMPLECRAWL_ROOT`,
`SIMPLECRAWL_SOURCE`, `SIMPLECRAWL_URL`, and `SIMPLECRAWL_TITLE`.

Hook failures are warnings; the article is kept. A summary is the first
whitespace-collapsed line of the command's stdout, truncated to about 200
characters. Summaries are cached in `.simplecrawl.json` and never expire. If a
summary is missing, later feed syncs retry it. External commands run with at most
`hook_concurrency` concurrent processes.

## Indexes

After a sync, `scrawl` rebuilds the current day's file in `index/` and
`INDEX.md`. The daily file is a flat timeline sorted by publication date, newest
first. `INDEX.md` groups articles by feed in `subscriptions.toml` order, with
`misc` last; articles within each group are newest first.

Generated sections are wrapped in:

```markdown
<!-- simplecrawl:begin -->
...
<!-- simplecrawl:end -->
```

Only that section is rewritten. Content outside it is yours. `scrawl index`
rebuilds every daily file and the global index.

To customize formatting, place `templates/daily.md.j2` or
`templates/index.md.j2` in the root. Templates use `minijinja`.

Daily context:

- `date`: `YYYY-MM-DD`
- `entries`: objects with `title`, `href`, `source`, `published_date`, and
  `summary` (possibly null)

Global context:

- `groups`: objects with `dir`, `title`, and `entries`; entries have `title`,
  `href`, and `published_date`

## State and failure handling

Each source has a `.simplecrawl.json` containing feed validators, the feed
title, last sync time, downloaded entries, and ignored entries. State is written
atomically after each article. The whole mutating command holds
`.simplecrawl.lock`; if another process holds it, the command exits immediately.

If a feed URL in configuration no longer matches the URL recorded in state, the
sync stops with an error. Change the URL back, remove the state file, or use a
different `dir`.

Article failures are not persisted by default, so the next sync retries them.
Use `sync --mark-failed` to add this run's failures to the ignored list. To
unignore an entry, edit the state file and remove it from `ignored`.

Exit codes:

- `0`: success
- `1`: partial failure (some articles or feeds failed)
- `2`: usage or configuration error

## Limitations

- Feed discovery from a website homepage is not implemented; pass the feed URL.
- Feed-provided content is not used as a fallback. If the article page cannot
  be fetched or extracted, the article fails.
- JavaScript-rendered pages are not supported.
- Markview-style math and empty-table preservation is not built in yet. Use a
  `post_download` command for site-specific cleanup.

## License

GPL-2.0-or-later.
