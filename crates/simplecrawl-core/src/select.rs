use crate::config::{Config, FeedConfig};

pub enum Selector<'a> {
	Dir(&'a str),
	Tag(&'a str),
}

pub fn parse_selector(value: &str) -> Selector<'_> {
	if let Some(tag) = value.strip_prefix('#') {
		Selector::Tag(tag)
	} else {
		Selector::Dir(value)
	}
}

pub fn feed_matches(feed: &FeedConfig, selector: &Selector<'_>) -> bool {
	match selector {
		Selector::Dir(dir) => feed.dir == *dir,
		Selector::Tag(tag) => feed.tags.iter().any(|value| value == tag),
	}
}

pub fn feed_matches_exclusion(feed: &FeedConfig, value: &str) -> bool {
	let value = value.strip_prefix('^').unwrap_or(value);
	if let Some(tag) = value.strip_prefix('#') {
		feed.tags.iter().any(|candidate| candidate == tag)
	} else {
		feed.dir == value
			|| feed.tags.iter().any(|candidate| candidate == value)
	}
}

pub fn select<'a>(
	config: &'a Config,
	selectors: &[String],
) -> Result<Vec<&'a FeedConfig>, SelectError> {
	let mut positives = Vec::new();
	let mut exclusions = Vec::new();
	for value in selectors {
		if let Some(value) = value.strip_prefix('^') {
			exclusions.push(value);
		} else if !value.is_empty() {
			positives.push(value);
		}
	}
	let mut selected = Vec::new();
	for feed in &config.feeds {
		let included = if positives.is_empty() {
			true
		} else {
			positives
				.iter()
				.any(|value| feed_matches(feed, &parse_selector(value)))
		};
		if !included {
			continue;
		}
		if exclusions
			.iter()
			.any(|value| feed_matches_exclusion(feed, value))
		{
			continue;
		}
		selected.push(feed);
	}
	if selected.is_empty() && !selectors.is_empty() {
		return Err(SelectError::NoMatch);
	}
	Ok(selected)
}

#[derive(Debug, thiserror::Error)]
pub enum SelectError {
	#[error("no feeds matched the selector")]
	NoMatch,
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeMap;

	use crate::config::{Config, FeedConfig, Hooks, Summarize};

	fn feed(dir: &str, tags: &[&str]) -> FeedConfig {
		FeedConfig {
			url: format!("https://example.com/{dir}.xml"),
			dir: dir.to_string(),
			tags: tags.iter().map(ToString::to_string).collect(),
			after: None,
			headers: BTreeMap::default(),
			hooks: Hooks::default(),
			summarize: Summarize::default(),
		}
	}

	fn config() -> Config {
		Config {
			feeds: vec![
				feed("alpha", &["rust", "tech"]),
				feed("beta", &["rust"]),
				feed("gamma", &["life"]),
			],
			..Config::default()
		}
	}

	fn dirs(values: Vec<&FeedConfig>) -> Vec<&str> {
		values.into_iter().map(|feed| feed.dir.as_str()).collect()
	}

	#[test]
	fn positives_are_a_union_and_exclusions_are_subtracted() {
		let config = config();
		let selected =
			select(&config, &["alpha".into(), "#rust".into(), "^beta".into()])
				.unwrap();
		assert_eq!(dirs(selected), vec!["alpha"]);
	}

	#[test]
	fn only_exclusions_start_from_all_feeds() {
		let config = config();
		let selected = select(&config, &["^#rust".into()]).unwrap();
		assert_eq!(dirs(selected), vec!["gamma"]);
	}

	#[test]
	fn unknown_selector_is_an_error() {
		let config = config();
		assert!(select(&config, &["missing".into()]).is_err());
	}
}
