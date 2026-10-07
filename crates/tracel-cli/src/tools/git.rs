use anyhow::Context;
use gix::Repository;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;
use std::path::Path;

pub fn is_repo_dirty() -> anyhow::Result<bool> {
    let repo = gix::discover(".")?;
    Ok(repo.is_dirty()?)
}

/// Whether the repository has at least one commit.
pub fn has_commit() -> bool {
    gix::discover(".").is_ok_and(|repo| repo.head_commit().is_ok())
}

pub fn get_first_commit_hash() -> anyhow::Result<String> {
    let repo = gix::discover(".")?;

    let platform = repo
        .rev_walk([repo.head_id()?])
        .first_parent_only()
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::OldestFirst));
    let revs = platform.all()?;

    let last_hash = revs.last().context("The repository has no commit")?;
    let last_hash = last_hash?.id();

    Ok(last_hash.to_string())
}

pub fn is_repo_initialized() -> bool {
    gix::discover(".").is_ok()
}

pub fn init_repo(dir: &Path) -> anyhow::Result<Repository> {
    if is_repo_initialized() {
        anyhow::bail!("The git repository already exists.");
    }

    let repo = gix::init(dir)?;
    Ok(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_commit_is_the_root_of_head() {
        let output = std::process::Command::new("git")
            .args(["rev-list", "--max-parents=0", "--first-parent", "HEAD"])
            .output()
            .expect("git runs");
        let root = String::from_utf8_lossy(&output.stdout);

        let first = get_first_commit_hash().expect("the repository has commits");

        assert_eq!(first, root.trim());
    }
}
