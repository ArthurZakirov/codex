//! One audio owner across the user's portable Voice processes.
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

static LOCAL_OWNER: Mutex<Weak<File>> = Mutex::new(Weak::new());

pub(super) fn acquire() -> io::Result<Arc<File>> {
    let mut owner = LOCAL_OWNER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lease) = owner.upgrade() {
        return Ok(lease);
    }
    let home = dirs::home_dir().ok_or_else(|| io::Error::other("Home directory unavailable"))?;
    let directory = home.join(".codex");
    std::fs::create_dir_all(&directory)?;
    // Use the user's home, not a session-specific CODEX_HOME.
    let lease = Arc::new(lock(&directory.join("voice-audio.lock"))?);
    *owner = Arc::downgrade(&lease);
    Ok(lease)
}

fn lock(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Voice audio is already in use in another Codex tab.",
        )),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_process_can_only_take_a_released_lease() {
        const PATH: &str = "CODEX_TEST_VOICE_LOCK_PATH";
        const BUSY: &str = "CODEX_TEST_VOICE_LOCK_BUSY";
        if let Some(path) = std::env::var_os(PATH) {
            let result = lock(Path::new(&path));
            if std::env::var_os(BUSY).is_some() {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
            } else {
                assert!(result.is_ok());
            }
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("voice.lock");
        let probe = |busy: bool| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("--exact")
                .arg(
                    concat!(
                        module_path!(),
                        "::another_process_can_only_take_a_released_lease"
                    )
                    .split_once("::")
                    .unwrap()
                    .1,
                )
                .env(PATH, &path);
            if busy {
                child.env(BUSY, "1");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        };

        let lease = lock(&path).unwrap();
        probe(true);
        drop(lease);
        probe(false);
    }
}
