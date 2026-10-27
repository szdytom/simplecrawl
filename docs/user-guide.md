# scrawl user guide

This guide describes how to install, configure, and use `scrawl` in daily
practice. The agreed behavioral constraints are recorded separately in
[`design.md`](design.md).

## Contents

1. [What scrawl does](#what-scrawl-does)
2. [Installation](#installation)
3. [Creating an archive](#creating-an-archive)
4. [Daily workflow](#daily-workflow)
5. [Command reference](#command-reference)
6. [Configuration reference](#configuration-reference)
7. [Selectors](#selectors)
8. [Article files](#article-files)
9. [Images](#images)
10. [State, locking, and failures](#state-locking-and-failures)
11. [Hooks](#hooks)
12. [Summaries](#summaries)
13. [Indexes and templates](#indexes-and-templates)
14. [Single-article fetch](#single-article-fetch)
15. [Backup and version control](#backup-and-version-control)
16. [Troubleshooting](#troubleshooting)
17. [Security notes](#security-notes)
18. [Limitations and non-goals](#limitations-and-non-goals)

## What scrawl does

`scrawl` reads a hand-written subscription file, fetches each feed, downloads
new article pages, extracts the main content, and saves the result as Markdown
under a normal directory tree.

It is designed around a few strong rules:

- Downloads are incremental.
- Article files are written once and never overwritten.
- Every source has its own folder and state file.
- The archive is plain files; there is no database.
- Global and daily Markdown indexes are generated from state.
- External commands can format articles or produce summaries.

A source is identified by a directory name such as `example` or `my-blog`.
Each source has one feed and one `.simplecrawl.json`. One-off pages fetched
without a subscription go into `misc/`.

## Installation

### From this repository

```sh
cargo install --path crates/simplecrawl
```

The package is called `simplecrawl`; the installed binary is `scrawl`. After a
crates.io release, install it with `cargo install simplecrawl`.

### Requirements

- Recent stable Rust, at least 1.89
- Linux or macOS
- `sh`, used by the hook and summary commands

Windows is not supported. WSL can be used, but paths and hooks then follow the
Linux environment inside WSL.

Verify the installation:

```sh
scrawl --version
scrawl --help
```

## Creating an archive

Create a directory and a subscription file:

```sh
mkdir -p ~/blogs
cd ~/blogs
```

Create `subscriptions.toml`:

```toml
[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"
tags = ["tech", "example"]
```

Then run the first sync:

```sh
scrawl sync
```

Because the current directory is the archive root, no `-C` is needed. The
first sync downloads every entry currently present in the feed. Depending on
the feed size, this can take a while.

After it finishes, the directory looks roughly like this:

```text
blogs/
  subscriptions.toml
  INDEX.md
  index/
    2026-10-06.md
  example/
    .simplecrawl.json
    2026-10-06-first-post.md
    images/
      a1/
        a1b2c3d4e5f60718.png
```

If you prefer to keep the archive elsewhere:

```sh
scrawl -C ~/blogs sync
scrawl -C ~/blogs list
```

`-C` must point to an existing directory. `scrawl` does not search upward for
an archive root.

## Daily workflow

A typical day:

```sh
scrawl -C ~/blogs sync
```

`sync` does the following:

1. Reads `subscriptions.toml`.
2. Fetches each selected feed, unless the feed is unchanged.
3. Filters out entries already downloaded or explicitly ignored.
4. Fetches each new article page.
5. Extracts the article and downloads its images.
6. Writes the Markdown file and updates state.
7. Runs `post_download` and `summarize` if configured.
8. Rebuilds today's daily index and `INDEX.md`.

Useful variants:

```sh
# Sync only one source
scrawl sync example

# Sync all sources tagged "rust"
scrawl sync '#rust'

# Sync all sources tagged "rust", except the source or tag "old"
scrawl sync '#rust' '^old'

# Look at what would be downloaded, without fetching article pages
scrawl sync --dry-run

# Download at most 10 new articles from each selected feed
scrawl sync --limit 10
```

Quotes around `#rust` are important in most shells because an unquoted `#`
starts a comment.

## Command reference

### `scrawl sync`

```text
scrawl sync [SELECTOR...] [--dry-run] [--limit N] [--mark-failed]
```

Downloads new articles.

- No selectors: process every configured feed.
- `SELECTOR`: a feed `dir`, `#tag`, or `^exclusion`; see
  [Selectors](#selectors).
- `--dry-run`: fetch feeds and report planned downloads, but do not fetch
  article pages or images. Does not write article, state, or index files.
- `--limit N`: process at most N new articles per feed, newest first. N must be
  at least 1.
- `--mark-failed`: add this run's failed articles to the source's `ignored`
  list. The command still exits 1 because the articles failed.

Progress is written to stderr. The final summary is written to stdout unless
`--quiet` is used.

Example output:

```text
example: 2 new article(s)
example: A First Post -> 2026-10-06-a-first-post.md
sync: 1 feed(s), 2 new, 0 failed, 0 known
```

### `scrawl list`

```text
scrawl list [--json]
```

Lists configured sources, including `misc` if it has state.

Columns:

| Column | Meaning |
| --- | --- |
| `DIR` | Source directory name |
| `TITLE` | Feed title recorded in state |
| `TAGS` | Subscription tags |
| `ARTICLES` | Number of successful state entries |
| `IGNORED` | Number of ignored entries |
| `LAST SYNC` | Last successful feed sync time |
| `URL` | Feed URL |

`--json` emits an array of objects with the same information. Example:

```sh
scrawl list --json | jq '.[] | {dir, articles, ignored}'
```

### `scrawl fetch`

```text
scrawl fetch <URL>
```

Downloads one article into `misc/` without a subscription. See
[Single-article fetch](#single-article-fetch).

### `scrawl index`

```text
scrawl index
```

Rebuilds every daily index file and `INDEX.md` from state. This is useful after
editing state, filling in a summary for an older day, or changing a template.

### Global options

```text
-C <DIR>       Archive root; defaults to the current directory
-v, --verbose  Print extra diagnostics
-q, --quiet    Suppress normal progress and summaries
```

`--verbose` and `--quiet` cannot be used together.

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Partial failure: some article or feed failed, or a runtime error occurred |
| `2` | Usage or configuration error |

Examples of exit 2: missing `subscriptions.toml`, malformed TOML, invalid feed
URL, reserved `dir`, invalid selector, `--limit 0`.

## Configuration reference

The configuration file is always `subscriptions.toml` in the archive root. It
is read by the tool and is never rewritten. Edit it by hand.

A complete example:

```toml
[headers]
User-Agent = "Mozilla/5.0 (compatible; scrawl)"
Authorization = "Bearer ${BLOG_TOKEN}"
Accept-Language = "en"

[hooks]
post_download = "my-formatter {file}"

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
tags = ["tech", "example"]
after = "2024-01-01"

[feeds.headers]
Referer = "https://example.com/"

[feeds.hooks]
post_download = "other-formatter {file}"

[feeds.summarize]
command = "other-summarizer {file}"
```

### `[[feeds]]`

| Field | Required | Meaning |
| --- | --- | --- |
| `url` | yes | Direct HTTP(S) RSS, Atom, or JSON Feed URL |
| `dir` | yes | Single path segment used as the source folder |
| `tags` | no | Subscription tags used by `sync` selectors |
| `after` | no | Only sync entries published on or after this `YYYY-MM-DD` date |
| `headers` | no | Per-feed header overrides |
| `hooks` | no | Per-feed hook overrides |
| `summarize` | no | Per-feed summary override |

Rules:

- `dir` cannot be empty, absolute, contain `/` or `\`, start with `.`, or start
  with `#` or `^`.
- `dir` cannot be one of the reserved names: `index`, `misc`, `templates`,
  `INDEX.md`, or `subscriptions.toml` (case-insensitive).
- Duplicate `dir` values are rejected.
- Duplicate feed `url` values are rejected.
- `after` must be a date in `YYYY-MM-DD` form. Both quoted strings and native
  TOML dates are accepted.
- A feed `dir` is created on first sync if it does not exist.

### Per-feed `after` cutoff

`after` limits a source to entries published on or after the given date. The
date is inclusive: an entry published on the date itself is synced. Entries
without a publication or update date are skipped.

```toml
[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"
after = "2024-01-01"
```

### `[headers]` and `[feeds.headers]`

Headers are sent with feed, article, and image requests.

```toml
[headers]
User-Agent = "Mozilla/5.0"
Authorization = "Bearer ${BLOG_TOKEN}"

[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"

[feeds.headers]
Referer = "https://example.com/"
```

- Header names are compared case-insensitively.
- Per-feed headers override global headers with the same name.
- Values support `${ENV_VAR}` expansion.
- `$$` produces a literal `$`.
- A missing environment variable is a configuration error.

Environment expansion:

```toml
[headers]
Authorization = "Bearer ${BLOG_TOKEN}"
Cookie = "session=${SESSION_COOKIE}; theme=dark"
```

```sh
export BLOG_TOKEN="..."
export SESSION_COOKIE="..."
scrawl sync
```

### `[hooks]` and `[feeds.hooks]`

```toml
[hooks]
post_download = "my-formatter {file}"
```

`post_download` runs after a new article and its images have been written.
Per-feed hooks override the global hook. See [Hooks](#hooks).

### `[summarize]` and `[feeds.summarize]`

```toml
[summarize]
command = "my-summarizer {file}"
```

`summarize` runs after `post_download` and its stdout becomes the article
summary in the indexes. Per-feed commands override the global command. See
[Summaries](#summaries).

### `[runtime]`

| Field | Default | Meaning |
| --- | --- | --- |
| `download_concurrency` | `8` | Maximum concurrent HTTP requests |
| `hook_concurrency` | `2` | Maximum concurrent hook/summary processes |
| `http_timeout_secs` | `30` | Timeout per HTTP attempt |
| `hook_timeout_secs` | `30` | Timeout for `post_download` |
| `summary_timeout_secs` | `60` | Timeout for `summarize` |
| `per_host_delay_ms` | `200` | Delay between requests to the same host |
| `retry_base_ms` | `1000` | Base delay for exponential retry backoff |

Concurrency and timeouts must be greater than zero. `per_host_delay_ms` and
`retry_base_ms` may be zero, which is mainly useful in tests.

### Global configuration

A small global configuration file can be used for settings that should apply
to every archive. It is not part of `subscriptions.toml` and is never
rewritten by the tool. Its location follows the XDG base directory rules:

```text
$XDG_CONFIG_HOME/scrawl/config.toml
```

If `XDG_CONFIG_HOME` is unset, empty, or relative, the fallback is:

```text
$HOME/.config/scrawl/config.toml
```

The file is optional. Supported keys:

```toml
# Extra PEM certificates appended to the built-in trust store.
tls_extra_roots = "certs/extra-roots.pem"
```

Relative paths are resolved from the directory containing `config.toml`, not
from the archive root or the current working directory. The file may contain
one or more PEM certificates.

This is an escape hatch for servers that send an incomplete TLS chain or use a
private CA. For an incomplete public chain, add the missing intermediate
certificate itself, not just its root. For example, a server whose leaf
certificate is issued by Let's Encrypt `YR1` but does not send `YR1` can use:

```sh
curl -fsS http://yr1.i.lencr.org/ -o yr1.der
openssl x509 -inform DER -in yr1.der -out \
  "$XDG_CONFIG_HOME/scrawl/certs/extra-roots.pem"
```

Then set:

```toml
tls_extra_roots = "certs/extra-roots.pem"
```

### Environment expansion in headers

```toml
[headers]
Authorization = "Bearer ${BLOG_TOKEN}"
```

The value is expanded when `subscriptions.toml` is loaded. If `BLOG_TOKEN` is
not set, commands that load the configuration fail with exit 2. `$$` escapes a
literal dollar sign.

## Selectors

`sync` accepts zero or more selectors.

| Form | Matches |
| --- | --- |
| `foo` | Feed whose `dir` is exactly `foo` |
| `#tag` | Feed whose `tags` contain `tag` |
| `^foo` | Excludes dir `foo` or tag `foo` |
| `^#tag` | Excludes tag `tag` |

Semantics:

- All positive selectors form a union `A`.
- All exclusions form a union `B`.
- The result is `A \ B`, i.e. `A` minus `B`.
- If there are no positive selectors, start from all feeds.
- If a non-empty selector set matches no feeds, the command exits 2.

Examples:

```sh
# One source
scrawl sync example

# Everything tagged rust
scrawl sync '#rust'

# Everything tagged rust, except the old source or old tag
scrawl sync '#rust' '^old'

# Several positives: source alpha OR tag rust, minus beta
scrawl sync alpha '#rust' '^beta'

# Everything except the source "archive" and the tag "spam"
scrawl sync '^archive' '^#spam'
```

Remember to quote `#tag`; otherwise the shell treats it as a comment.

## Article files

Each article is a single Markdown file directly inside its source directory:

```text
example/2026-10-06-a-first-post.md
```

File names use `YYYY-MM-DD-<slug>.md`:

- The date is the article publication date in the local timezone.
- Date fallback order: feed `published`, feed `updated`, page metadata, download
  date.
- Unicode letters and digits are preserved.
- Whitespace and punctuation collapse to `-`.
- The slug is truncated to about 180 bytes.
- An empty title becomes `untitled`.
- Name collisions get `-2`, `-3`, and so on.

Articles are never overwritten. If a file already exists, `scrawl` chooses a new
name instead. If you edit, rename, or delete an article, `scrawl` respects that
decision:

- It will not change the file.
- It will not re-download the entry.
- It will not update the index path.
- If you deleted the file, the index can keep a dead link.

### File format

```markdown
---
title: A First Post
url: https://example.com/posts/a-first-post
author: Alice
published: 2026-10-06T12:34:56+08:00
updated: 2026-10-07T08:00:00+08:00
feed: https://example.com/feed.xml
tags:
- rust
- example
---

# A First Post

Source: <https://example.com/posts/a-first-post>

This is the article body.
```

Frontmatter fields:

| Field | Meaning |
| --- | --- |
| `title` | Article title |
| `url` | Feed entry URL, as written in the feed |
| `author` | Feed author, or extracted page byline |
| `published` | Publication time, RFC 3339 with local offset |
| `updated` | Update time, if known |
| `feed` | Feed URL; omitted for `fetch` |
| `tags` | Feed entry categories |

Title priority:

1. Feed entry title
2. Extracted page title
3. Article URL

The article body comes from the article page, not from the feed's
`content`/`summary` fields. If the page cannot be fetched or extracted, the
article fails. There is no feed-content fallback.

If the extracted body starts with an H1 whose text matches the article title,
that duplicate heading is removed before the body is written.

### Manual files

The tool never scans source directories for Markdown files. A file you create by
hand is not part of state and is not included in the generated indexes. It is
also not overwritten, even if a new article would otherwise use the same name;
the new article gets a suffixed name instead.

## Images

Images referenced by extracted Markdown are downloaded into the source folder:

```text
example/images/a1/a1b2c3d4e5f60718.png
```

Behavior:

- Images are deduplicated by normalized URL.
- The local path is used as a relative Markdown image link.
- Only responses with an `image/*` content type are saved.
- Each response is capped at 20 MiB.
- `data:` image URIs are left unchanged.
- If an image download fails, or returns a non-image response, the original
  remote URL is kept and a warning is printed.
- A failed image does not fail the article.

Images are also subject to the global and per-host HTTP limits.

## State, locking, and failures

Each source folder contains a hidden `.simplecrawl.json`:

```text
example/
  .simplecrawl.json
  .md files
  images/
```

The state file records:

- state format version
- feed URL and feed title
- feed validators (`ETag`, `Last-Modified`)
- last sync time
- downloaded entries, including title, URL, path, dates, tags, and summary
- ignored entries

State is written with a temporary file and an atomic rename. It is written after
each successful article.

### Locking

Every mutating command (`sync`, `fetch`, `index`) takes an exclusive lock on:

```text
archive/.simplecrawl.lock
```

If another process already holds the lock, the command exits immediately with an
error. The lock is released automatically when the process exits. A leftover
lock file by itself does not mean the archive is locked; the file may stay on
disk safely.

### Feed URL changes

If `subscriptions.toml` points a `dir` at a different feed URL than the one
recorded in `.simplecrawl.json`, that source stops with an error. Resolve it by
one of:

1. Restoring the original URL.
2. Removing the state file and accepting a fresh archive in that directory.
3. Using a different `dir` for the new feed.

### Article failures

Article failures are not persisted by default. The next `sync` retries them.
When an article fails, the previous feed validators are restored so the next sync
cannot skip the failed entries with a `304 Not Modified`.

`--mark-failed` records the current run's failures in `ignored`:

```sh
scrawl sync --mark-failed
```

Those entries are skipped on later syncs. The command still exits 1 for this
run. To unignore an entry, edit `.simplecrawl.json` and remove it from the
`ignored` array.

### Feed-level failures

If a feed cannot be fetched or parsed, that source fails but other selected
sources continue. The command exits 1 and prints the error.

## Hooks

Hooks are external commands configured in `subscriptions.toml`.

```toml
[hooks]
post_download = "my-formatter {file}"
```

`post_download` runs after the article file and images have been written. It
runs before the summary command.

### Command contract

- The command is executed with `sh -c`.
- The working directory is the archive root.
- Placeholders are replaced with automatically shell-escaped values.
- The same values are available as environment variables.
- The command's stdout is ignored for `post_download`.
- A non-zero exit, timeout, or spawn error produces a warning. The article is
  kept and sync continues.

Do not add quotes around placeholders; they are already shell-escaped.

```toml
# Correct
post_download = "my-formatter {file}"

# Incorrect: results in double quoting
post_download = "my-formatter \"{file}\""
```

### Placeholders

| Placeholder | Value |
| --- | --- |
| `{file}` | Absolute path to the Markdown file |
| `{root}` | Absolute archive root |
| `{source}` | Source directory name; `misc` for `fetch` |
| `{url}` | Article URL |
| `{title}` | Article title |

### Environment variables

| Variable | Value |
| --- | --- |
| `SIMPLECRAWL_FILE` | Article file path |
| `SIMPLECRAWL_ROOT` | Archive root |
| `SIMPLECRAWL_SOURCE` | Source directory name |
| `SIMPLECRAWL_URL` | Article URL |
| `SIMPLECRAWL_TITLE` | Article title |

### Examples

Append a generated footer:

```toml
[hooks]
post_download = "printf '\n---\nArchived by scrawl\n' >> {file}"
```

Run a site-specific cleanup formatter:

```toml
[hooks]
post_download = "markview-webfmt {file}"
```

Run a script that reads environment variables:

```toml
[hooks]
post_download = "~/bin/post-process.sh"
```

```sh
#!/bin/sh
set -eu
printf '\nProcessed from %s\n' "$SIMPLECRAWL_URL" >> "$SIMPLECRAWL_FILE"
```

### Concurrency and timeouts

At most `hook_concurrency` external commands run at once. `post_download` uses
`hook_timeout_secs`; `summarize` uses `summary_timeout_secs`.

## Summaries

A summary command reads an article file and prints a short summary:

```toml
[summarize]
command = "my-summarizer {file}"
```

The command contract is the same as for hooks: `sh -c`, archive root as working
directory, escaped placeholders, environment variables, and a timeout.

The output is:

- whitespace-collapsed to one line
- truncated to about 200 characters
- stored in `.simplecrawl.json`
- used by the daily index

Summaries are never invalidated. If you change the summarize command, already
generated summaries remain in place.

New summaries are generated after `post_download` completes. If the command
fails or produces no output, the article is still archived but has no summary.
Missing summaries are retried on later feed syncs. `index` does not run summary
commands; use `sync` when you want missing summaries filled.

Example with a simple shell command:

```toml
[summarize]
command = "printf 'Summary generated at %s' \"$(date -u +%FT%TZ)\""
```

A real summarizer will usually read `{file}` and print a sentence.

## Indexes and templates

Two kinds of indexes are generated.

### Daily index

```text
index/YYYY-MM-DD.md
```

The date is the local date on which the article was downloaded, not the article
publication date. Multiple syncs on the same day merge into one file.

Entries are sorted by publication date, newest first. Each line looks like:

```markdown
- [A First Post](<../example/2026-10-06-a-first-post.md>) · example · 2026-10-06 — A short summary
```

The source and summary parts are omitted or simplified when unavailable.

### Global index

```text
INDEX.md
```

Sections are ordered by `[[feeds]]` order in `subscriptions.toml`, with `misc`
last. Each section contains that source's articles, newest first.

Section headings use the feed title plus the source directory:

```markdown
## Example Blog (example)
```

If the feed title is unavailable or identical to the directory, the directory is
used.

### Generated markers

Generated regions are wrapped in comments:

```markdown
<!-- simplecrawl:begin -->
...
<!-- simplecrawl:end -->
```

Only the region between those markers is rewritten. You can write notes before
or after the block:

```markdown
# Index

My own notes about this archive.

<!-- simplecrawl:begin -->
...
<!-- simplecrawl:end -->

More notes.
```

`sync` rebuilds today's daily index and `INDEX.md`. `scrawl index` rebuilds all
daily indexes and `INDEX.md`.

### Templates

Override the default formatting by creating:

```text
templates/daily.md.j2
templates/index.md.j2
```

Templates use `minijinja` syntax. Auto-escaping is disabled because the output is
Markdown.

Daily template context:

```text
date: string
entries: [
  {
    title: string,
    href: string,             # relative to index/
    source: string,
    published_date: string,
    summary: string | null
  }
]
```

Global template context:

```text
groups: [
  {
    dir: string,
    title: string,
    entries: [
      {
        title: string,
        href: string,          # relative to the archive root
        published_date: string
      }
    ]
  }
]
```

Example daily template:

```jinja
# {{ date }}

{% for entry in entries -%}
- [{{ entry.title }}](<{{ entry.href }}>) · {{ entry.source }}
  {%- if entry.summary %} — {{ entry.summary }}{% endif %}
{% endfor -%}
```

Example global template:

```jinja
# My Archive

{% for group in groups -%}
## {{ group.title }}

{% for entry in group.entries -%}
- {{ entry.published_date }} [{{ entry.title }}](<{{ entry.href }}>)
{% endfor %}

{% endfor -%}
```

Template errors cause the index command to fail with exit 1. Article and state
files are not affected.

## Single-article fetch

`fetch` archives one page without adding a subscription:

```sh
scrawl -C ~/blogs fetch https://example.com/an-interesting-post
```

Behavior:

- Writes into `misc/`.
- Uses the same extraction and article format as `sync`.
- Uses global headers, hooks, summarize, and runtime settings if
  `subscriptions.toml` exists.
- Maintains `misc/.simplecrawl.json`.
- Skips a URL that was already fetched.
- Adds the article to the daily index and `INDEX.md` under `misc`.
- Renders `misc` last in `INDEX.md`.
- Omits the `feed` frontmatter field because there is no feed.
- Uses the page title, page author, and page publication time when available.

`fetch` does not require `subscriptions.toml`, but the archive root must exist.

Repeated fetch:

```text
already fetched: /home/user/blogs/misc/2026-10-06-an-interesting-post.md
```

To re-fetch the same URL, remove its entry from `misc/.simplecrawl.json` and
delete or rename the existing Markdown file if desired.

## Backup and version control

The archive is a normal directory. A safe backup includes:

- all Markdown files
- all `images/` directories
- `subscriptions.toml`
- `INDEX.md`
- `index/`
- `templates/`
- every `.simplecrawl.json`

Include `.simplecrawl.json` if you want to preserve incremental-sync state. If
you restore Markdown files without state, `scrawl` treats all feed entries as new
and will create suffixed duplicates instead of overwriting existing files.

You do not need to keep:

- `.simplecrawl.lock`

The lock file is harmless and its presence does not mean the archive is locked.

### Git example

```sh
cd ~/blogs
git init
printf '.simplecrawl.lock\n' >> .gitignore
git add .
git commit -m "chore: initial blog archive"
```

Track state files if you sync the archive between machines and want to avoid
re-downloading everything.

If you do not want hidden JSON files in the repository, exclude them, but expect
a fresh download pass and possible filename suffixes on existing articles.

## Troubleshooting

### `sync` says `0 new` but I expected articles

Possible causes:

- The feed returned `304 Not Modified` and state is current.
- Entries are already recorded in state.
- Entries are in `ignored`.
- Selectors did not include the feed.
- The entries have no article URL and therefore cannot be downloaded.

Check state and selectors:

```sh
scrawl list
scrawl sync --dry-run
scrawl list --json | jq
```

If you want to force a feed refetch, remove its `etag` and `last_modified` from
`.simplecrawl.json`. There is intentionally no `--force`.

### Feed URL mismatch

```text
feed dir `example` already belongs to https://old.example/feed.xml, not https://new.example/feed.xml
```

Restore the old URL, remove `example/.simplecrawl.json`, or use a different
`dir`.

### Lock busy

```text
another scrawl process is using /path/to/archive
```

Another `sync`, `fetch`, or `index` is running. Wait for it to finish. The lock
is released when its process exits. Do not delete the lock file while a process
is running.

### Article fails but the feed is fine

Common reasons:

- The article URL returns 403/404/500.
- The site requires cookies or a different User-Agent.
- The page is JavaScript-rendered and contains no readable static content.
- The response is not HTML.
- The page has no content that `dom_smoothie` can extract.
- The article body exceeds the 20 MiB response limit.

Try adding headers:

```toml
[[feeds]]
url = "https://example.com/feed.xml"
dir = "example"

[feeds.headers]
User-Agent = "Mozilla/5.0 ..."
Referer = "https://example.com/"
```

`scrawl` does not use the feed's embedded content as a fallback.

If the failure is permanent, ignore it:

```sh
scrawl sync --mark-failed
```

### Images stay remote

Possible reasons:

- The image URL is not HTTP(S).
- The response is not `image/*`.
- The image exceeds 20 MiB.
- The image request failed.

The article is still written; the original URL is kept. Run with `--verbose` to
see more detail. Image failures do not cause exit 1.

### Index is missing an article

Check:

1. Is the article in `.simplecrawl.json`?
2. Is the file path in state correct?
3. Does the daily index date match the download date, not the publication date?
4. Was the daily index manually edited outside the markers?

Manual Markdown files are not scanned into indexes. Only state entries are used.
Run:

```sh
scrawl index
```

to rebuild all indexes from state.

### Index has a dead link

If you deleted or renamed an article by hand, state still points at the old path.
This is deliberate: the tool respects manual deletions and does not re-download.
Edit the state entry's `path`, remove the entry from state, or edit the generated
index.

### A summary disappeared or changed

Summaries are cached in state forever. Changing the summarize command does not
rewrite old summaries. To regenerate one, remove its `summary` field from
`.simplecrawl.json` and run `sync` for that source.

### Timestamps or file dates look wrong

Article file dates and daily-index grouping use the local timezone. Set `TZ` if
you need deterministic output:

```sh
TZ=UTC scrawl sync
```

Feed dates are stored as RFC 3339 in frontmatter. Existing files are not renamed
when the timezone changes.

### `#tag` selector does nothing

The shell removed the token as a comment. Quote it:

```sh
scrawl sync '#rust'
```

### `scrawl fetch` says `already fetched`

The URL is already present in `misc/.simplecrawl.json`. Remove that entry to
fetch it again. Existing files are never overwritten.

## Security notes

- `subscriptions.toml` can contain secrets in headers. Prefer `${ENV_VAR}`
  expansion and keep the secret in the environment, not in the file.
- Header values are sent to the feed, article pages, and image hosts. Only add
  credentials when you trust every host involved.
- Hooks and summary commands execute arbitrary shell commands from your
  configuration. Treat `subscriptions.toml` as trusted code.
- Remote images are fetched automatically. This contacts image hosts and can
  reveal your IP address and download activity.
- The tool does not obey `robots.txt`, does not maintain a cookie jar, and does
  not run JavaScript.
- Downloaded HTML is converted to Markdown and saved; no page scripts are
  executed by `scrawl`.

## Limitations and non-goals

- Unix only; no Windows support.
- No feed autodiscovery from a homepage.
- No OPML import/export.
- No built-in login flow, cookie jar, or OAuth.
- No `robots.txt` handling.
- No JavaScript rendering.
- No feed-content fallback.
- No `add`, `remove`, `init`, or `--force`.
- No built-in LLM or summarizer.
- No Markview-style math/empty-table preservation yet; use a `post_download`
  hook for site-specific cleanup.
- Feed fetching is incremental, but a feed that only exposes the latest N entries
  cannot be used to recover entries that have already disappeared from the feed.

If a site moves or renumbers entries, the entry identity may change and `scrawl`
may download the article again under a new filename. This is expected because
the tool never overwrites existing files.
