use super::*;
use git2::{BranchType, Oid, Signature, Time};

/// Write `files` into the working tree, stage them, and commit on `HEAD`
/// at `seconds` since the epoch, so recency ordering is deterministic.
fn commit_at(dir: &Path, repo: &Repository, files: &[(&str, &str)], msg: &str, seconds: i64) {
    for (path, text) in files {
        fs::write(dir.join(path), text).unwrap();
    }
    stage_all(repo);
    let mut index = repo.index().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(seconds, 0)).unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .unwrap();
}

fn checkout_new_branch(repo: &Repository, name: &str) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
    repo.set_head(&format!("refs/heads/{name}")).unwrap();
}

#[test]
fn local_branches_list_the_checked_out_branch_first_then_by_recency() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "a\n")], "first", 100);
    let main = raw.head().unwrap().shorthand().unwrap().to_string();
    checkout_new_branch(&raw, "older");
    commit_at(dir.path(), &raw, &[("a.txt", "b\n")], "older work", 200);
    raw.set_head(&format!("refs/heads/{main}")).unwrap();
    checkout_new_branch(&raw, "newer");
    commit_at(dir.path(), &raw, &[("a.txt", "c\n")], "newer work", 300);
    raw.set_head(&format!("refs/heads/{main}")).unwrap();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let branches = repo.local_branches().unwrap();

    let names: Vec<_> = branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, [main.as_str(), "newer", "older"]);
    assert!(branches[0].head);
    assert!(!branches[1].head);
    assert_eq!(branches[1].summary, "newer work");
    assert_eq!(branches[1].time, 300);
}

#[test]
fn local_branches_leave_out_remote_tracking_branches() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "a\n")], "first", 100);
    let head = raw.head().unwrap().target().unwrap();
    raw.reference("refs/remotes/origin/feature", head, false, "test")
        .unwrap();
    assert!(
        raw.find_branch("origin/feature", BranchType::Remote)
            .is_ok()
    );

    let repo = Repo::discover_at(dir.path()).unwrap();
    let branches = repo.local_branches().unwrap();

    assert_eq!(branches.len(), 1);
}

#[test]
fn local_branches_of_an_unborn_repo_are_empty() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    let repo = Repo::discover_at(dir.path()).unwrap();

    assert_eq!(repo.local_branches().unwrap(), Vec::new());
}

#[test]
fn commits_walk_a_branch_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "a\n")], "first", 100);
    commit_at(
        dir.path(),
        &raw,
        &[("a.txt", "b\n")],
        "second\n\nWith a body.\n",
        200,
    );
    let branch = raw.head().unwrap().shorthand().unwrap().to_string();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let commits = repo.commits(&branch, 10).unwrap();

    let summaries: Vec<_> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["second", "first"]);
    let newest = &commits[0];
    assert_eq!(newest.id.len(), 40);
    assert_eq!(newest.short_id, newest.id[..7]);
    assert_eq!(newest.message, "second\n\nWith a body.\n");
    assert_eq!(newest.author, "Ada Lovelace");
    assert_eq!(newest.email, "ada@example.com");
    assert_eq!(newest.time, 200);
}

#[test]
fn commits_stop_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    for n in 0..5 {
        commit_at(
            dir.path(),
            &raw,
            &[("a.txt", &format!("{n}\n"))],
            &format!("c{n}"),
            100 + n,
        );
    }
    let branch = raw.head().unwrap().shorthand().unwrap().to_string();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let commits = repo.commits(&branch, 2).unwrap();

    let summaries: Vec<_> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["c4", "c3"]);
}

/// Commit `HEAD`'s tree on `parents` at `seconds`, moving no ref.
fn commit_on(repo: &Repository, parents: &[Oid], msg: &str, seconds: i64) -> Oid {
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(seconds, 0)).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|id| repo.find_commit(*id).unwrap())
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    repo.commit(None, &sig, &sig, msg, &tree, &parents).unwrap()
}

/// A branch `merged` whose two lines of work interleave in time:
/// `base` 100, `side 1` 200, `main 2` 300, `side 3` 400, `main 4` 500,
/// and the merge 600.
fn interleaved_merge(dir: &Path) -> Repository {
    let raw = init_repo(dir);
    commit_at(dir, &raw, &[("a.txt", "a\n")], "base", 100);
    let base = raw.head().unwrap().target().unwrap();
    let side1 = commit_on(&raw, &[base], "side 1", 200);
    let main2 = commit_on(&raw, &[base], "main 2", 300);
    let side3 = commit_on(&raw, &[side1], "side 3", 400);
    let main4 = commit_on(&raw, &[main2], "main 4", 500);
    let merge = commit_on(&raw, &[main4, side3], "merge", 600);
    raw.reference("refs/heads/merged", merge, false, "test")
        .unwrap();
    raw
}

#[test]
fn commits_list_merged_history_newest_first_each_once() {
    let dir = tempfile::tempdir().unwrap();
    interleaved_merge(dir.path());

    let repo = Repo::discover_at(dir.path()).unwrap();
    let commits = repo.commits("merged", 10).unwrap();

    let summaries: Vec<_> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(
        summaries,
        ["merge", "main 4", "side 3", "main 2", "side 1", "base"]
    );
}

#[test]
fn commits_stop_at_the_limit_across_merges() {
    let dir = tempfile::tempdir().unwrap();
    interleaved_merge(dir.path());

    let repo = Repo::discover_at(dir.path()).unwrap();
    let commits = repo.commits("merged", 3).unwrap();

    let summaries: Vec<_> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["merge", "main 4", "side 3"]);
}

#[test]
fn commits_of_an_unknown_branch_are_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "a\n")], "first", 100);

    let repo = Repo::discover_at(dir.path()).unwrap();

    assert!(repo.commits("nope", 10).is_err());
}

#[test]
fn commit_patch_diffs_against_the_first_parent_with_full_blob_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "one\ntwo\n")], "first", 100);
    commit_at(
        dir.path(),
        &raw,
        &[("a.txt", "one\nTWO\n"), ("b.txt", "new\n")],
        "second",
        200,
    );
    let id = raw.head().unwrap().target().unwrap().to_string();
    // Uncommitted edits never leak into a commit's patch.
    fs::write(dir.path().join("a.txt"), "dirty\n").unwrap();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("diff --git a/a.txt b/a.txt"), "{patch}");
    assert!(patch.contains("-two\n+TWO\n"), "{patch}");
    assert!(patch.contains("diff --git a/b.txt b/b.txt"), "{patch}");
    assert!(patch.contains("new file mode"), "{patch}");
    assert!(!patch.contains("dirty"), "{patch}");
    let before = raw
        .revparse_single("HEAD~1:a.txt")
        .unwrap()
        .id()
        .to_string();
    let after = raw.revparse_single("HEAD:a.txt").unwrap().id().to_string();
    assert!(
        patch.contains(&format!("index {before}..{after}")),
        "{patch}"
    );
}

#[test]
fn commit_patch_of_a_root_commit_adds_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "one\n")], "first", 100);
    let id = raw.head().unwrap().target().unwrap().to_string();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("new file mode"), "{patch}");
    assert!(patch.contains("+one\n"), "{patch}");
}

#[test]
fn commit_patch_of_an_unknown_id_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "one\n")], "first", 100);

    let repo = Repo::discover_at(dir.path()).unwrap();

    assert!(
        repo.commit_patch("0123456789012345678901234567890123456789")
            .is_err()
    );
}
