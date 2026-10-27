use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct FrontMatter<'a> {
	pub title: &'a str,
	pub url: &'a str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub author: Option<&'a str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub published: Option<&'a str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub updated: Option<&'a str>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub feed: Option<&'a str>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<&'a str>,
}

pub struct ArticleMeta<'a> {
	pub title: &'a str,
	pub url: &'a str,
	pub author: Option<&'a str>,
	pub published: Option<&'a str>,
	pub updated: Option<&'a str>,
	pub feed: Option<&'a str>,
	pub tags: &'a [String],
}

pub fn render_article(meta: &ArticleMeta<'_>, body: &str) -> String {
	let front = FrontMatter {
		title: meta.title,
		url: meta.url,
		author: meta.author,
		published: meta.published,
		updated: meta.updated,
		feed: meta.feed,
		tags: meta.tags.iter().map(String::as_str).collect(),
	};
	let yaml =
		serde_yaml_ng::to_string(&front).expect("frontmatter is serializable");
	let mut out = String::new();
	out.push_str("---\n");
	out.push_str(&yaml);
	out.push_str("---\n\n");
	out.push_str("# ");
	out.push_str(&crate::util::one_line(meta.title));
	out.push_str("\n\nSource: <");
	out.push_str(meta.url);
	out.push_str(">\n\n");
	out.push_str(body.trim());
	out.push('\n');
	out
}

pub fn normalize_summary(value: &str) -> Option<String> {
	let line = crate::util::one_line(value);
	if line.is_empty() {
		None
	} else {
		Some(crate::util::truncate_chars(&line, 200))
	}
}
