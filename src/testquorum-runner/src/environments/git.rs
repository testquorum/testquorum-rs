use std::path::Path;

use git2::Oid;
use git2::Repository;
use git2::Sort;

/// Number of commits reachable from `from`, inclusive of `from` itself.
/// Matches the `git rev-list --count` semantics that TestQuorum's `height`
/// field is specified against.
pub(super) fn rev_count(repo: &Repository, from: Oid) -> Result<i64, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::NONE)?;
    walk.push(from)?;
    let mut count: i64 = 0;
    for step in walk {
        // Surface walker errors (missing object in a shallow clone) rather
        // than silently undercounting.
        step?;
        count += 1;
    }
    Ok(count)
}

/// Best-common-ancestor of two commits. Returned `Oid` is the merge base.
pub(super) fn merge_base(repo: &Repository, a: Oid, b: Oid) -> Result<Oid, git2::Error> {
    repo.merge_base(a, b)
}

/// Parses an SHA and returns the `Oid` if libgit2 can see the object in
/// the local repo. Used to verify environment-provided shas before height
/// counting.
pub(super) fn resolve_oid(repo: &Repository, sha: &str) -> Result<Oid, git2::Error> {
    let oid = Oid::from_str(sha)?;
    // `find_commit` errors if the object isn't present locally — exactly the
    // shallow-clone case we want to detect.
    let _ = repo.find_commit(oid)?;
    Ok(oid)
}

/// Open the repository the runner is invoked in. We always open from the
/// current working directory; CI checkouts and `cargo run` both land in the
/// repo root.
pub(super) fn open() -> Result<Repository, git2::Error> {
    Repository::discover(Path::new("."))
}

#[cfg(test)]
mod tests {
    use git2::IndexAddOption;
    use git2::Signature;
    use tempfile::TempDir;

    use super::*;

    fn signature() -> Signature<'static> {
        Signature::now("test", "test@example.com").unwrap()
    }

    fn write_file(repo: &Repository, name: &str, body: &str) {
        let path = repo.workdir().unwrap().join(name);
        std::fs::write(path, body).unwrap();
    }

    fn commit_all(repo: &Repository, message: &str) -> Oid {
        let mut index = repo.index().unwrap();
        index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = signature();
        let parents: Vec<git2::Commit> = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
            .unwrap()
    }

    #[test]
    fn rev_count_and_merge_base_on_branching_history() {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        write_file(&repo, "a", "1");
        let c1 = commit_all(&repo, "c1");
        write_file(&repo, "a", "2");
        let c2 = commit_all(&repo, "c2");

        // Branch off at c2 → side.
        let c2_commit = repo.find_commit(c2).unwrap();
        repo.branch("side", &c2_commit, false).unwrap();

        // Advance main with one more commit.
        write_file(&repo, "a", "3");
        let c3 = commit_all(&repo, "c3");

        // Switch to side and add a divergent commit.
        repo.set_head("refs/heads/side").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .unwrap();
        write_file(&repo, "b", "x");
        let s1 = commit_all(&repo, "s1");

        assert_eq!(rev_count(&repo, c1).unwrap(), 1);
        assert_eq!(rev_count(&repo, c3).unwrap(), 3);
        assert_eq!(rev_count(&repo, s1).unwrap(), 3);
        assert_eq!(merge_base(&repo, c3, s1).unwrap(), c2);
    }

    #[test]
    fn resolve_oid_rejects_unknown_sha() {
        let dir = TempDir::new().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // A well-formed but absent sha.
        let err = resolve_oid(&repo, "0123456789abcdef0123456789abcdef01234567").unwrap_err();
        assert_eq!(err.class(), git2::ErrorClass::Odb);
    }
}
