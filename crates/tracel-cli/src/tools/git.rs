use gix::Repository;
use std::path::Path;

pub fn is_repo_initialized() -> bool {
    gix::discover(".").is_ok()
}

pub fn init_repo(dir: &Path) -> anyhow::Result<Repository> {
    if is_repo_initialized() {
        return Err(anyhow::anyhow!("Repository already initialized."));
    }

    let repo = gix::init(dir)?;
    Ok(repo)
}
