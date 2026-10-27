use std::{path::PathBuf, process::ExitCode};

use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand};
use simplecrawl_core::{
	Config, Reporter, RootLock, SyncOptions,
	fetch::{list_sources, run_fetch},
	index,
	sync::{SyncError, run_sync},
};

#[derive(Debug, Parser)]
#[command(
	name = "scrawl",
	version,
	about = "Archive feeds as local Markdown",
	disable_help_subcommand = true
)]
struct Cli {
	/// Change to this directory before running.
	#[arg(short = 'C', default_value = ".", global = true)]
	directory: PathBuf,
	/// Print extra diagnostic output.
	#[arg(short, long, global = true, conflicts_with = "quiet")]
	verbose: bool,
	/// Suppress normal progress output.
	#[arg(short, long, global = true)]
	quiet: bool,
	#[command(subcommand)]
	command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
	/// Download new articles for all or selected feeds.
	Sync {
		/// Feed dirs, #tags, or ^exclusions.
		selectors: Vec<String>,
		/// Fetch feeds but do not download articles.
		#[arg(long)]
		dry_run: bool,
		/// Process at most N new articles per feed.
		#[arg(long)]
		limit: Option<usize>,
		/// Mark this run's article failures as ignored.
		#[arg(long)]
		mark_failed: bool,
	},
	/// List configured sources and their state.
	List {
		/// Emit JSON instead of a table.
		#[arg(long)]
		json: bool,
	},
	/// Download a single article into misc/.
	Fetch {
		/// HTTP(S) article URL.
		url: String,
	},
	/// Rebuild every daily index and INDEX.md from state.
	Index,
	/// Show standard help followed by the full user guide.
	Help {
		/// Show help for this subcommand instead.
		topic: Option<String>,
	},
}

const USER_GUIDE: &str = include_str!("../../../docs/user-guide.md");

#[derive(Debug)]
struct CliError {
	code: u8,
	error: anyhow::Error,
}

impl CliError {
	fn new(code: u8, error: impl Into<anyhow::Error>) -> Self {
		Self {
			code,
			error: error.into(),
		}
	}
}

#[tokio::main]
async fn main() -> ExitCode {
	match run().await {
		Ok(code) => code,
		Err(err) => {
			eprintln!("error: {}", err.error);
			ExitCode::from(err.code)
		}
	}
}

async fn run() -> Result<ExitCode, CliError> {
	let cli = Cli::parse();
	if let Command::Help { topic } = &cli.command {
		let mut command = Cli::command();
		if let Some(topic) = topic {
			let Some(subcommand) = command.find_subcommand_mut(topic) else {
				return Err(CliError::new(
					2,
					anyhow::anyhow!("unknown command `{topic}`"),
				));
			};
			subcommand
				.print_help()
				.context("failed to print help")
				.map_err(|error| CliError::new(1, error))?;
			println!();
		} else {
			command
				.print_help()
				.context("failed to print help")
				.map_err(|error| CliError::new(1, error))?;
			println!("\n\n{USER_GUIDE}");
		}
		return Ok(ExitCode::SUCCESS);
	}
	let root = cli.directory;
	if !root.is_dir() {
		return Err(CliError::new(
			2,
			anyhow::anyhow!("{} is not a directory", root.display()),
		));
	}
	let reporter = Reporter::new(cli.quiet, cli.verbose);
	match cli.command {
		Command::Sync {
			selectors,
			dry_run,
			limit,
			mark_failed,
		} => {
			if limit == Some(0) {
				return Err(CliError::new(
					2,
					anyhow::anyhow!("--limit must be at least 1"),
				));
			}
			let config = Config::load(&root)
				.map_err(|e| CliError::new(2, anyhow::Error::new(e)))?;
			let _lock = RootLock::acquire(&root)
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			let report = run_sync(
				&root,
				&config,
				SyncOptions {
					selectors,
					dry_run,
					limit,
					mark_failed,
					reporter,
				},
			)
			.await
			.map_err(|e| {
				let code = if matches!(e, SyncError::Select(_)) {
					2
				} else {
					1
				};
				CliError::new(code, anyhow::Error::new(e))
			})?;
			for error in &report.errors {
				eprintln!("error: {error}");
			}
			if !cli.quiet {
				println!(
					"sync: {} feed(s), {} new, {} failed, {} known",
					report.feeds, report.new, report.failed, report.skipped
				);
			}
			if report.failed > 0 || !report.errors.is_empty() {
				Ok(ExitCode::from(1))
			} else {
				Ok(ExitCode::SUCCESS)
			}
		}
		Command::List { json } => {
			let config = Config::load(&root)
				.map_err(|e| CliError::new(2, anyhow::Error::new(e)))?;
			let sources = list_sources(&root, &config)
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			if json {
				let text = serde_json::to_string_pretty(&sources)
					.context("failed to serialize source list")
					.map_err(|e| CliError::new(1, e))?;
				println!("{text}");
			} else {
				print_table(&sources);
			}
			Ok(ExitCode::SUCCESS)
		}
		Command::Fetch { url } => {
			let config = Config::load_optional(&root)
				.map_err(|e| CliError::new(2, anyhow::Error::new(e)))?;
			let _lock = RootLock::acquire(&root)
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			let outcome = run_fetch(&root, &config, &url, reporter)
				.await
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			if !cli.quiet {
				if outcome.skipped {
					println!("already fetched: {}", outcome.path.display());
				} else {
					println!("saved: {}", outcome.path.display());
				}
			}
			Ok(ExitCode::SUCCESS)
		}
		Command::Help { .. } => Ok(ExitCode::SUCCESS),
		Command::Index => {
			let config = Config::load_optional(&root)
				.map_err(|e| CliError::new(2, anyhow::Error::new(e)))?;
			let _lock = RootLock::acquire(&root)
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			index::rebuild_all(&root, &config)
				.map_err(|e| CliError::new(1, anyhow::Error::new(e)))?;
			if !cli.quiet {
				println!("index rebuilt");
			}
			Ok(ExitCode::SUCCESS)
		}
	}
}

fn print_table(sources: &[simplecrawl_core::ListEntry]) {
	let headers = [
		"DIR",
		"TITLE",
		"TAGS",
		"ARTICLES",
		"IGNORED",
		"LAST SYNC",
		"URL",
	];
	let rows = sources
		.iter()
		.map(|source| {
			[
				source.dir.clone(),
				source.title.clone().unwrap_or_default(),
				source.tags.join(","),
				source.articles.to_string(),
				source.ignored.to_string(),
				source.last_sync.clone().unwrap_or_default(),
				source.url.clone(),
			]
		})
		.collect::<Vec<_>>();
	let widths = headers
		.iter()
		.enumerate()
		.map(|(index, header)| {
			rows.iter()
				.map(|row| row[index].chars().count())
				.max()
				.unwrap_or(0)
				.max(header.chars().count())
		})
		.collect::<Vec<_>>();
	print_row(&headers.map(ToString::to_string), &widths);
	for row in rows {
		print_row(&row, &widths);
	}
}

fn print_row(row: &[String; 7], widths: &[usize]) {
	for (index, value) in row.iter().enumerate() {
		if index + 1 == row.len() {
			println!("{value}");
		} else {
			print!("{value:<width$}  ", width = widths[index]);
		}
	}
}
