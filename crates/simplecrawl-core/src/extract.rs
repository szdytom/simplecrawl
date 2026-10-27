use std::{
	collections::{BTreeMap, BTreeSet},
	path::Path,
	sync::OnceLock,
};

use chrono::DateTime;
use dom_query::{Document, Matcher, NodeId};
use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;

use crate::{
	http::HttpClient,
	util::{atomic_write, hash_hex, one_line, short_hash},
};

#[derive(Debug)]
pub struct Extracted {
	pub title: String,
	pub byline: Option<String>,
	pub published: Option<i64>,
	pub updated: Option<i64>,
	pub markdown: String,
}

pub fn extract(
	html: &str,
	url: &str,
	selector: Option<&str>,
) -> Result<Extracted, ExtractError> {
	let doc = match selector {
		Some(selector) => scoped_document(html, selector)?,
		None => Document::from(html),
	};
	let preserved = preserve_markup(&doc);
	let mut readability = Readability::with_document(doc, Some(url), None)
		.map_err(|e| ExtractError::Readability(e.to_string()))?;
	readability.config = Config {
		text_mode: TextMode::Markdown,
		max_elements_to_parse: 100_000,
		..Config::default()
	};
	let article = readability
		.parse()
		.map_err(|e| ExtractError::Readability(e.to_string()))?;
	let title = one_line(&article.title);
	let body = article.text_content.trim().to_string();
	if body.is_empty() {
		return Err(ExtractError::Empty);
	}
	let body = strip_leading_title(&body, &title);
	let mut markdown = literal_delimiters(&body);
	for (token, replacement) in preserved {
		markdown = markdown.replace(&token, &replacement);
	}
	if markdown.trim().is_empty() {
		return Err(ExtractError::Empty);
	}
	let published = article.published_time.as_deref().and_then(parse_timestamp);
	let updated = article.modified_time.as_deref().and_then(parse_timestamp);
	Ok(Extracted {
		title,
		byline: article.byline.map(|value| one_line(&value)),
		published,
		updated,
		markdown,
	})
}

/// Restricts the document body to the elements matched by `selector`.
/// Nested matches are kept once, and everything outside the matches is removed.
fn scoped_document(
	html: &str,
	selector: &str,
) -> Result<Document, ExtractError> {
	let doc = Document::from(html);
	let matcher = Matcher::new(selector)
		.map_err(|_| ExtractError::InvalidSelector(selector.to_string()))?;
	let selection = doc.select_matcher(&matcher);
	if selection.is_empty() {
		return Err(ExtractError::SelectorMiss(selector.to_string()));
	}
	let mut kept = BTreeSet::<NodeId>::new();
	let mut scoped = String::new();
	for node in selection.nodes() {
		if node
			.ancestors(None)
			.iter()
			.any(|ancestor| kept.contains(&ancestor.id))
		{
			continue;
		}
		kept.insert(node.id);
		scoped.push_str(&node.inner_html());
	}
	doc.select_single("body").set_html(scoped);
	Ok(doc)
}

fn parse_timestamp(value: &str) -> Option<i64> {
	DateTime::parse_from_rfc3339(value)
		.ok()
		.map(|value| value.timestamp())
}

fn strip_leading_title(body: &str, title: &str) -> String {
	let mut lines = body.lines().peekable();
	let Some(first) = lines.peek() else {
		return body.to_string();
	};
	let first = first.trim();
	if let Some(heading) = first.strip_prefix("# ")
		&& normalize_heading(heading) == normalize_heading(title)
	{
		lines.next();
		let rest = lines.collect::<Vec<_>>().join("\n");
		return rest.trim_start_matches('\n').to_string();
	}
	body.to_string()
}

/// Protects semantic HTML that the Markdown serializer treats as plain text.
fn preserve_markup(document: &Document) -> Vec<(String, String)> {
	let text = document.root().text();
	let mut prefix = "SIMPLECRAWLPRESERVED".to_string();
	while text.contains(&prefix) {
		prefix.push('X');
	}
	let mut preserved = Vec::new();
	for node in document
		.select(".math.inline, .math.display, script[type^='math/tex']")
		.nodes()
	{
		if node.ancestors_it(None).any(|parent| parent.is("pre, code")) {
			continue;
		}
		let text = node.text();
		let mut latex = text.trim();
		for (open, close) in
			[(r"\(", r"\)"), (r"\[", r"\]"), ("$$", "$$"), ("$", "$")]
		{
			if let Some(inner) =
				latex.strip_prefix(open).and_then(|s| s.strip_suffix(close))
			{
				latex = inner;
				break;
			}
		}
		let in_table =
			node.ancestors_it(None).any(|parent| parent.is("td, th"));
		// TeX delimiters keep literal pipes from splitting Markdown table cells.
		let latex = if in_table {
			latex.replace(r"\|", r"\Vert ").replace('|', r"\vert ")
		} else {
			latex.to_string()
		};
		let display = node.has_class("display")
			|| node
				.attr("type")
				.is_some_and(|kind| kind.contains("mode=display"));
		let replacement = if display && !in_table {
			format!("\n\n\\[{latex}\\]\n\n")
		} else {
			format!("\\({latex}\\)")
		};
		let token = format!("{prefix}{}END", preserved.len());
		if node.is("script") {
			node.rename("span");
		}
		node.set_text(token.as_str());
		preserved.push((token, replacement));
	}
	// Empty headings otherwise produce an empty, invalid table separator cell.
	for node in document.select("th").nodes() {
		if node.inner_html().trim().is_empty() {
			let token = format!("{prefix}{}END", preserved.len());
			node.set_text(token.as_str());
			preserved.push((token, String::new()));
		}
	}
	preserved
}

/// Keeps `CommonMark` punctuation escapes from becoming Markview LaTeX delimiters.
fn literal_delimiters(markdown: &str) -> String {
	let arena = comrak::Arena::new();
	let root =
		comrak::parse_document(&arena, markdown, &comrak::Options::default());
	let mut lines = vec![0];
	lines.extend(markdown.match_indices('\n').map(|(offset, _)| offset + 1));
	let mut code = root
		.descendants()
		.filter_map(|node| {
			let data = node.data.borrow();
			matches!(
				data.value,
				comrak::nodes::NodeValue::Code(_)
					| comrak::nodes::NodeValue::CodeBlock(_)
			)
			.then(|| {
				let position = data.sourcepos;
				let start =
					lines[position.start.line - 1] + position.start.column - 1;
				let end = (lines[position.end.line - 1] + position.end.column)
					.min(markdown.len());
				start..end
			})
		})
		.peekable();
	let mut output = String::with_capacity(markdown.len());
	let mut offset = 0;
	while offset < markdown.len() {
		if let Some(range) = code.peek()
			&& offset == range.start
		{
			output.push_str(&markdown[range.clone()]);
			offset = range.end;
			code.next();
			continue;
		}
		let tail = &markdown[offset..];
		let mut chars = tail.chars();
		let c = chars.next().unwrap();
		if c == '\\'
			&& let Some(next) = chars.next()
		{
			match next {
				'(' => output.push_str("&#40;"),
				')' => output.push_str("&#41;"),
				'[' => output.push_str("&#91;"),
				']' => output.push_str("&#93;"),
				_ => {
					output.push(c);
					output.push(next);
				}
			}
			offset += c.len_utf8() + next.len_utf8();
		} else {
			output.push(c);
			offset += c.len_utf8();
		}
	}
	output
}

fn normalize_heading(value: &str) -> String {
	value
		.chars()
		.filter(|ch| ch.is_alphanumeric())
		.flat_map(char::to_lowercase)
		.collect()
}

pub async fn localize_images(
	http: &HttpClient,
	headers: &BTreeMap<String, String>,
	markdown: &str,
	source_dir: &Path,
	base_url: &str,
) -> (String, Vec<String>) {
	let regex = image_regex();
	let base = url::Url::parse(base_url).ok();
	let mut urls = Vec::new();
	for captures in regex.captures_iter(markdown) {
		let Some(url) =
			captures.name("angle").or_else(|| captures.name("plain"))
		else {
			continue;
		};
		let raw = url.as_str();
		if raw.starts_with("data:") {
			continue;
		}
		let resolved = base
			.as_ref()
			.and_then(|base| base.join(raw).ok())
			.map_or_else(|| raw.to_string(), |url| url.to_string());
		if !urls.iter().any(|(existing, _)| existing == &resolved) {
			urls.push((resolved, raw.to_string()));
		}
	}
	if urls.is_empty() {
		return (markdown.to_string(), Vec::new());
	}

	let futures = urls.into_iter().map(|(url, raw)| {
		let source_dir = source_dir.to_path_buf();
		async move {
			let local =
				match download_image(http, headers, &url, &source_dir).await {
					Ok(local) => local,
					Err(err) => return (raw, url, None, Some(err)),
				};
			(raw, url, Some(local), None)
		}
	});
	let results = futures::future::join_all(futures).await;
	let mut mapping = BTreeMap::new();
	let mut warnings = Vec::new();
	for (raw, url, local, error) in results {
		if let Some(local) = local {
			mapping.insert(raw, local.clone());
			mapping.insert(url, local);
		}
		if let Some(error) = error {
			warnings.push(error);
		}
	}
	(rewrite_images(markdown, &mapping), warnings)
}

async fn download_image(
	http: &HttpClient,
	headers: &BTreeMap<String, String>,
	url: &str,
	source_dir: &Path,
) -> Result<String, String> {
	let parsed =
		url::Url::parse(url).map_err(|_| format!("invalid image URL {url}"))?;
	if !matches!(parsed.scheme(), "http" | "https") {
		return Err(format!("unsupported image URL {url}"));
	}
	let normalized = crate::util::normalize_url(url);
	let hash = hash_hex(&normalized);
	let stem = short_hash(&normalized, 16);
	if let Some(existing) = existing_image(source_dir, &hash[..2], &stem) {
		return Ok(existing);
	}
	let response = http
		.get(url, headers, Some("image/*"), None)
		.await
		.map_err(|err| format!("image {url}: {err}"))?;
	let content_type = response.content_type.as_deref().unwrap_or("");
	if !content_type.starts_with("image/") {
		return Err(format!(
			"image {url}: unexpected content type {content_type}"
		));
	}
	let extension = image_extension(parsed.as_str(), Some(content_type));
	let relative = format!("images/{}/{}.{}", &hash[..2], stem, extension);
	let target = source_dir.join(&relative);
	atomic_write(&target, &response.body)
		.map_err(|err| format!("image {url}: {err}"))?;
	Ok(relative)
}

fn image_extension(url: &str, content_type: Option<&str>) -> String {
	let from_type = content_type.map(|value| {
		value
			.split(';')
			.next()
			.unwrap_or(value)
			.trim()
			.to_ascii_lowercase()
	});
	let ext = match from_type.as_deref() {
		Some("image/jpeg") => Some("jpg"),
		Some("image/png") => Some("png"),
		Some("image/gif") => Some("gif"),
		Some("image/webp") => Some("webp"),
		Some("image/svg+xml") => Some("svg"),
		Some("image/avif") => Some("avif"),
		Some("image/bmp") => Some("bmp"),
		Some("image/x-icon" | "image/vnd.microsoft.icon") => Some("ico"),
		_ => None,
	};
	if let Some(ext) = ext {
		return ext.to_string();
	}
	url::Url::parse(url)
		.ok()
		.and_then(|url| {
			Path::new(url.path())
				.extension()
				.and_then(|ext| ext.to_str())
				.map(str::to_ascii_lowercase)
		})
		.filter(|ext| {
			(1..=5).contains(&ext.len())
				&& ext.chars().all(|ch| ch.is_ascii_alphanumeric())
		})
		.unwrap_or_else(|| "img".to_string())
}

fn rewrite_images(
	markdown: &str,
	mapping: &BTreeMap<String, String>,
) -> String {
	let regex = image_regex();
	let mut out = String::with_capacity(markdown.len());
	let mut last = 0;
	for captures in regex.captures_iter(markdown) {
		let Some(url) =
			captures.name("angle").or_else(|| captures.name("plain"))
		else {
			continue;
		};
		let Some(replacement) = mapping.get(url.as_str()) else {
			continue;
		};
		out.push_str(&markdown[last..url.start()]);
		out.push_str(replacement);
		last = url.end();
	}
	out.push_str(&markdown[last..]);
	out
}

fn image_regex() -> &'static Regex {
	static REGEX: OnceLock<Regex> = OnceLock::new();
	REGEX.get_or_init(|| {
		Regex::new(
			r#"!\[[^\]]*\]\((?:<(?P<angle>[^>]+)>|(?P<plain>[^)\s]+))(?:\s+"[^"]*")?\)"#,
		)
		.expect("image regex is valid")
	})
}

fn existing_image(
	source_dir: &Path,
	prefix: &str,
	stem: &str,
) -> Option<String> {
	let dir = source_dir.join("images").join(prefix);
	let entries = std::fs::read_dir(&dir).ok()?;
	for entry in entries.filter_map(Result::ok) {
		let path = entry.path();
		if path.file_stem().and_then(|value| value.to_str()) == Some(stem) {
			let file = path.file_name()?.to_string_lossy();
			return Some(format!("images/{prefix}/{file}"));
		}
	}
	None
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
	#[error("readability extraction failed: {0}")]
	Readability(String),
	#[error("invalid CSS selector `{0}`")]
	InvalidSelector(String),
	#[error("CSS selector `{0}` matched no elements")]
	SelectorMiss(String),
	#[error("no readable article content found")]
	Empty,
}

#[cfg(test)]
mod tests {
	use super::*;

	const MATH_TABLE_HTML: &str = r#"<!doctype html>
<html><head><title>Cabo 胜率</title></head><body><article>
<h1>Cabo 胜率</h1>
<p>本问题考察 <span class="math inline">\(n=2\)</span> 的特殊情形：我方先行动，初始查看的两张牌为 <span class="math inline">\((k_1,k_2)\)</span>。问：若此时立即宣告 Cabo，本轮胜率是多少？普通说明 (delta) 与数学公式应保留不同的语义。</p>
<p>对手不知道我方初始看牌，在信息集中选择最优行动。基线用于分离对手回合的影响，并验证模拟器的结果。模拟器基线与精确值的最大偏差在统计噪声内，误差为 <span class="math inline">\(|\Delta|_{\max}\approx0.003\)</span>。</p>
<div class="math display">\[W(k_1,k_2)=\Pr\!\big(\Sigma_{\mathrm{us}}\le\Sigma_{\mathrm{th}}\big)\]</div>
<p>以下给出代表性胜率表，空列用于分隔两组结果，表头、行数和数值都必须保留。正文的显示公式与表格中的行内公式均不需要执行 JavaScript。</p>
<table><thead><tr>
<th><span class="math inline">\((k_1,k_2)\)</span></th><th><span class="math inline">\(W\)</span></th><th></th><th><span class="math inline">\((k_1,k_2)\)</span></th><th><span class="math inline">\(W\)</span></th>
</tr></thead><tbody>
<tr><td><span class="math inline">\((0,0)\)</span></td><td>0.840</td><td></td><td><span class="math inline">\((6,6)\)</span></td><td>0.372</td></tr>
<tr><td><span class="math inline">\((0,3)\)</span></td><td>0.744</td><td></td><td><span class="math inline">\((7,7)\)</span></td><td>0.295</td></tr>
</tbody></table>
<table><thead><tr><th></th><th> </th></tr></thead><tbody>
<tr><td><span class="math inline">\(a|b\)</span></td><td><span class="math inline">\(\|x\|\)</span></td></tr>
</tbody></table>
<p>另一种静态 MathJax 标记：<script type="math/tex">x < y</script>。内联代码应保持原样：<code>\(not math\)</code>。</p>
<script type="math/tex; mode=display">x^2 + y^2 = z^2</script>
</article></body></html>"#;

	#[test]
	fn preserves_math_and_empty_table_headers() {
		let extracted =
			extract(MATH_TABLE_HTML, "https://example.org/cabo", None).unwrap();
		let markdown = extracted.markdown;
		assert!(!markdown.contains("SIMPLECRAWLPRESERVED"));
		assert!(markdown.contains(r"\(n=2\)"));
		assert!(markdown.contains(r"\((k_1,k_2)\)"));
		assert!(markdown.contains(r"\(|\Delta|_{\max}\approx0.003\)"));
		assert!(markdown.contains(r"\[W(k_1,k_2)"));
		assert!(markdown.contains(r"\(x < y\)"));
		assert!(markdown.contains(r"\[x^2 + y^2 = z^2\]"));
		assert!(markdown.contains("&#40;delta&#41;"));
		assert!(markdown.contains(r"`\(not math\)`"));
		assert!(markdown.contains(r"\vert b"));
		assert!(markdown.contains(r"\Vert x\Vert"));
		let table_rows: Vec<_> = markdown
			.lines()
			.filter(|line| line.starts_with('|'))
			.collect();
		assert!(table_rows.iter().any(|line| line.matches('|').count() == 6));
		assert!(
			table_rows
				.iter()
				.filter(|line| line.matches('|').count() == 3)
				.count() >= 2
		);
	}

	#[test]
	fn delimiter_conversion_skips_code_and_keeps_escaped_backslashes() {
		let markdown = "中文 \\(prose\\) \\[brackets\\] `\\(inline\\)`\n\n| Code |\n| --- |\n| `\\[table\\]` |\n\n- Example:\n\n  ```rust\n  \\(fenced\\)\n  ```\n\n\\\\(backslash)";
		let converted = literal_delimiters(markdown);
		assert!(converted.contains("&#40;prose&#41; &#91;brackets&#93;"));
		for code in [
			r"`\(inline\)`",
			r"`\[table\]`",
			r"\(fenced\)",
			r"\\(backslash)",
		] {
			assert!(converted.contains(code), "lost {code:?}: {converted}");
		}
	}
}
