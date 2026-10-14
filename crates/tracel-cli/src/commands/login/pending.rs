use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tracel_client::console::FileSessionStore;
use tracel_client::console::auth::DeviceCodeResponse;
use url::Url;

static TEMPORARY_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Serialize, Deserialize)]
pub struct PendingLogin {
    pub authorization: DeviceCodeResponse,
    pub expires_at: SystemTime,
}

impl PendingLogin {
    pub fn new(authorization: DeviceCodeResponse, now: SystemTime) -> anyhow::Result<Self> {
        let expires_at = now
            .checked_add(authorization.expires_in())
            .context("The login's expiry is out of range")?;
        Ok(Self {
            authorization,
            expires_at,
        })
    }
}

pub struct PendingLoginStore {
    path: PathBuf,
}

impl PendingLoginStore {
    pub fn for_server(base_url: &Url) -> anyhow::Result<Self> {
        let session = FileSessionStore::for_server(base_url)?;
        Ok(Self {
            path: session.path().with_extension("pending.json"),
        })
    }

    pub fn load(&self) -> anyhow::Result<Option<PendingLogin>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Can't read the pending login"),
        };
        let pending = serde_json::from_slice(&bytes).context("Can't read the pending login")?;
        Ok(Some(pending))
    }

    pub fn save(&self, pending: &PendingLogin) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(pending)?;
        let directory = self
            .path
            .parent()
            .context("The pending login has no directory")?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(directory)
            .context("Can't create the pending login's directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .context("Can't protect the pending login's directory")?;
        }

        let (temporary, mut file) = self.create_temporary()?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.context("Can't save the pending login")
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Can't remove the pending login"),
        }
    }

    fn create_temporary(&self) -> anyhow::Result<(PathBuf, File)> {
        loop {
            let mut name = self.path.file_name().unwrap_or_default().to_os_string();
            name.push(format!(
                ".{}.{}.tmp",
                std::process::id(),
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let temporary = self.path.with_file_name(name);
            match create_owner_only(&temporary) {
                Ok(file) => return Ok((temporary, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("Can't create the pending login file"),
            }
        }
    }
}

fn create_owner_only(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn store(directory: &tempfile::TempDir) -> PendingLoginStore {
        PendingLoginStore {
            path: directory
                .path()
                .join("sessions")
                .join("localhost_9001.pending.json"),
        }
    }

    fn pending(device_code: &str) -> PendingLogin {
        let authorization = DeviceCodeResponse {
            device_code: device_code.to_string(),
            user_code: "BCDF-GHJK".to_string(),
            verification_uri: "http://localhost:9001/verify".to_string(),
            verification_uri_complete: "http://localhost:9001/verify?code=BCDF-GHJK".to_string(),
            expires_in: 600,
            interval: 5,
        };
        PendingLogin::new(
            authorization,
            UNIX_EPOCH + Duration::from_secs(1_900_000_000),
        )
        .unwrap()
    }

    #[test]
    fn given_no_pending_login_when_loading_it_then_there_is_none() {
        let directory = tempfile::tempdir().unwrap();

        let loaded = store(&directory).load().unwrap();

        assert!(loaded.is_none());
    }

    #[test]
    fn given_a_saved_pending_login_when_loading_it_then_it_is_the_one_saved() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store.save(&pending("first")).unwrap();

        let loaded = store.load().unwrap().unwrap();

        assert_eq!(loaded.authorization.device_code, "first");
        assert_eq!(loaded.expires_at, pending("first").expires_at);
    }

    #[test]
    fn given_a_saved_pending_login_when_saving_another_then_it_replaces_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store.save(&pending("first")).unwrap();

        store.save(&pending("second")).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.authorization.device_code, "second");
    }

    #[test]
    fn given_a_saved_pending_login_when_saving_another_then_no_temporary_file_is_left() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store.save(&pending("first")).unwrap();

        store.save(&pending("second")).unwrap();

        let files = fs::read_dir(store.path.parent().unwrap()).unwrap().count();
        assert_eq!(files, 1);
    }

    #[test]
    fn given_a_saved_pending_login_when_clearing_it_twice_then_there_is_none() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store.save(&pending("first")).unwrap();

        store.clear().unwrap();
        store.clear().unwrap();

        assert!(store.load().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn given_a_pending_login_others_could_read_when_saving_it_then_only_its_owner_can() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let sessions = store.path.parent().unwrap().to_path_buf();
        store.save(&pending("first")).unwrap();
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o755)).unwrap();

        store.save(&pending("first")).unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.path), 0o600);
        assert_eq!(mode(&sessions), 0o700);
    }
}
