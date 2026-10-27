#[derive(Debug, Clone, Copy)]
pub struct Reporter {
	pub quiet: bool,
	pub verbose: bool,
}

impl Reporter {
	pub const fn new(quiet: bool, verbose: bool) -> Self {
		Self { quiet, verbose }
	}

	pub fn info(&self, message: impl AsRef<str>) {
		if !self.quiet {
			eprintln!("{}", message.as_ref());
		}
	}

	pub fn warn(&self, message: impl AsRef<str>) {
		eprintln!("warning: {}", message.as_ref());
	}

	pub fn detail(&self, message: impl AsRef<str>) {
		if self.verbose {
			eprintln!("{}", message.as_ref());
		}
	}
}
