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
pub(super) struct PendingLogin {
    pub authorization: DeviceCodeResponse,
    pub expires_at: SystemTime,
}

impl PendingLogin {
    pub fn new(authorization: DeviceCodeResponse, now: SystemTime) -> anyhow::Result<Self> {
        let expires_at = now
            .checked_add(authorization.expires_in())
            .context("Invalid device authorization expiry")?;
        Ok(Self {
            authorization,
            expires_at,
        })
    }
}

pub(super) struct PendingLoginStore {
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
            Err(error) => return Err(error).context("Could not read pending login"),
        };
        let pending = serde_json::from_slice(&bytes).context("Could not decode pending login")?;
        Ok(Some(pending))
    }

    pub fn save(&self, pending: &PendingLogin) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(pending)?;
        let directory = self
            .path
            .parent()
            .context("Missing pending login directory")?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(directory)
            .context("Could not create pending login directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .context("Could not protect pending login directory")?;
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
        result.context("Could not save pending login")
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("Could not remove pending login"),
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
                Err(error) => return Err(error).context("Could not create pending login file"),
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

    struct ScratchDirectory(PathBuf);

    impl ScratchDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tracel-pending-login-{}-{}",
                std::process::id(),
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn store(&self) -> PendingLoginStore {
            PendingLoginStore {
                path: self.0.join("sessions").join("localhost_9001.pending.json"),
            }
        }
    }

    impl Drop for ScratchDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn authorization(device_code: &str) -> DeviceCodeResponse {
        DeviceCodeResponse {
            device_code: device_code.to_string(),
            user_code: "BCDF-GHJK".to_string(),
            verification_uri: "http://localhost:9001/verify".to_string(),
            verification_uri_complete: "http://localhost:9001/verify?code=BCDF-GHJK".to_string(),
            expires_in: 600,
            interval: 5,
        }
    }

    #[test]
    fn pending_login_round_trip_replace_and_clear() {
        let directory = ScratchDirectory::new();
        let store = directory.store();
        assert!(store.load().unwrap().is_none());

        let now = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        for device_code in ["first", "second"] {
            let pending = PendingLogin::new(authorization(device_code), now).unwrap();
            store.save(&pending).unwrap();
            let loaded = store.load().unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&loaded).unwrap(),
                serde_json::to_value(&pending).unwrap()
            );
            assert_eq!(
                fs::read_dir(store.path.parent().unwrap()).unwrap().count(),
                1
            );
        }

        store.clear().unwrap();
        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pending_login_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = ScratchDirectory::new();
        let store = directory.store();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let pending = PendingLogin::new(authorization("first"), SystemTime::now()).unwrap();
        store.save(&pending).unwrap();
        assert_eq!(mode(&store.path), 0o600);
        assert_eq!(mode(store.path.parent().unwrap()), 0o700);

        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(
            store.path.parent().unwrap(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        store.save(&pending).unwrap();
        assert_eq!(mode(&store.path), 0o600);
        assert_eq!(mode(store.path.parent().unwrap()), 0o700);
    }

    #[test]
    fn expiry_uses_the_start_time_and_clamps_negative_lifetimes() {
        let now = UNIX_EPOCH + Duration::from_secs(1_900_000_000);
        for seconds in [-1, 0, 1, 600] {
            let mut authorization = authorization("device-code");
            authorization.expires_in = seconds;
            let pending = PendingLogin::new(authorization, now).unwrap();
            assert_eq!(
                pending.expires_at,
                now + Duration::from_secs(seconds.max(0) as u64)
            );
        }
    }

    #[test]
    fn pending_path_is_a_sibling_of_the_session() {
        for url in ["https://console.tracel.ai/api/", "http://localhost:9001/"] {
            let url = Url::parse(url).unwrap();
            let session = FileSessionStore::for_server(&url).unwrap();
            let pending = PendingLoginStore::for_server(&url).unwrap();
            assert_eq!(pending.path.parent(), session.path().parent());
            assert_eq!(pending.path, session.path().with_extension("pending.json"));
        }
    }
}
