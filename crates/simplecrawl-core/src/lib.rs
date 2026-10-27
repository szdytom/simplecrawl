//! Core library for `scrawl`, a small feed-to-Markdown archiver.

pub mod config;
pub mod extract;
pub mod feed;
pub mod fetch;
pub mod hook;
pub mod http;
pub mod index;
pub mod lock;
pub mod render;
pub mod reporter;
pub mod select;
pub mod state;
pub mod sync;
pub mod util;

pub use config::{Config, FeedConfig, Runtime};
pub use fetch::{FetchOutcome, ListEntry};
pub use lock::RootLock;
pub use reporter::Reporter;
pub use sync::{SyncOptions, SyncReport};
