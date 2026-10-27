use std::{fs::OpenOptions, path::Path};

pub struct RootLock {
	file: std::fs::File,
}

impl RootLock {
	pub fn acquire(root: &Path) -> Result<Self, LockError> {
		std::fs::create_dir_all(root).map_err(LockError::Io)?;
		let path = root.join(".simplecrawl.lock");
		let file = OpenOptions::new()
			.create(true)
			.write(true)
			.truncate(false)
			.open(&path)
			.map_err(LockError::Io)?;
		match file.try_lock() {
			Ok(()) => {}
			Err(std::fs::TryLockError::WouldBlock) => {
				return Err(LockError::Busy(root.to_path_buf()));
			}
			Err(std::fs::TryLockError::Error(error)) => {
				return Err(LockError::Io(error));
			}
		}
		Ok(Self { file })
	}
}

impl Drop for RootLock {
	fn drop(&mut self) {
		let _ = self.file.unlock();
	}
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
	#[error("failed to open lock file: {0}")]
	Io(std::io::Error),
	#[error("another scrawl process is using {0}")]
	Busy(std::path::PathBuf),
}
