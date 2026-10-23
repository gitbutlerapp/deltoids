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

#[test]
fn tree_patch_shows_the_net_change_across_commits() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("keep.txt", "one\n")], "base", 100);
    let first = commit_id(&raw);
    commit_at(
        dir.path(),
        &raw,
        &[("keep.txt", "one\nflip\n"), ("new.txt", "draft\n")],
        "second",
        200,
    );
    commit_at(
        dir.path(),
        &raw,
        &[("keep.txt", "one\n"), ("new.txt", "final\n")],
        "third",
        300,
    );
    let tip = commit_id(&raw);

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.tree_patch(&first, &tip).unwrap();

    assert!(!patch.contains("keep.txt"), "{patch}");
    assert!(patch.contains("new file mode"), "{patch}");
    assert!(patch.contains("+final\n"), "{patch}");
    assert!(!patch.contains("draft"), "{patch}");
}

#[test]
fn tree_patch_takes_any_revision() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "one\n")], "first", 100);
    commit_at(dir.path(), &raw, &[("a.txt", "two\n")], "second", 200);
    let tip = commit_id(&raw);

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.tree_patch(&format!("{tip}^"), &tip).unwrap();

    assert!(patch.contains("-one\n+two\n"), "{patch}");
    assert!(repo.tree_patch("nope", &tip).is_err());
}

fn commit_id(repo: &Repository) -> String {
    repo.head().unwrap().target().unwrap().to_string()
}

#[test]
fn commits_after_stop_at_the_base_even_past_a_merge() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    commit_at(dir.path(), &raw, &[("a.txt", "a\n")], "old", 100);
    commit_at(dir.path(), &raw, &[("a.txt", "b\n")], "base", 200);
    let base = commit_id(&raw);
    let main = raw.head().unwrap().shorthand().unwrap().to_string();
    checkout_new_branch(&raw, "feature");
    commit_at(dir.path(), &raw, &[("f.txt", "f\n")], "feature work", 300);
    // A merge whose second parent is an ancestor of the base.
    let old = raw
        .revparse_single("HEAD~2")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let head = raw.head().unwrap().peel_to_commit().unwrap();
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(400, 0)).unwrap();
    raw.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "merge old",
        &head.tree().unwrap(),
        &[&head, &old],
    )
    .unwrap();
    raw.set_head(&format!("refs/heads/{main}")).unwrap();

    let repo = Repo::discover_at(dir.path()).unwrap();
    let summaries =
        |commits: Vec<Commit>| -> Vec<String> { commits.into_iter().map(|c| c.summary).collect() };

    assert_eq!(
        summaries(repo.commits_after(&base, "feature", 10).unwrap()),
        ["merge old", "feature work"]
    );
    assert!(repo.commits_after(&base, &base, 10).unwrap().is_empty());
    assert_eq!(
        repo.commits_after("0123456789012345678901234567890123456789", "feature", 10)
            .unwrap()
            .len(),
        4,
        "an unknown base hides nothing"
    );
}

/// A tree of `files`, each a path at the root and its text.
fn tree_of(repo: &Repository, files: &[(&str, &str)]) -> Oid {
    let mut builder = repo.treebuilder(None).unwrap();
    for (path, text) in files {
        let blob = repo.blob(text.as_bytes()).unwrap();
        builder.insert(path, blob, 0o100644).unwrap();
    }
    builder.write().unwrap()
}

/// The tree GitButler writes for a conflicted commit: `top` at the root,
/// `resolved` as `.auto-resolution`, one side, and the conflict list.
fn conflicted_tree(repo: &Repository, top: &[(&str, &str)], resolved: &[(&str, &str)]) -> Oid {
    let base = repo.find_tree(tree_of(repo, top)).unwrap();
    let mut builder = repo.treebuilder(Some(&base)).unwrap();
    let auto = tree_of(repo, resolved);
    let side = tree_of(repo, &[("a.txt", "theirs\n")]);
    let list = repo.blob(b"ourEntries = [\"a.txt\"]\n").unwrap();
    builder.insert(".auto-resolution", auto, 0o040000).unwrap();
    builder.insert(".conflict-side-0", side, 0o040000).unwrap();
    builder.insert(".conflict-files", list, 0o100644).unwrap();
    builder.write().unwrap()
}

/// Write a commit of `tree` on `parent`, with `header` (a `name value`
/// line) after the committer when given, and return its id.
fn commit_tree(
    repo: &Repository,
    tree: Oid,
    parent: Option<Oid>,
    message: &str,
    header: Option<&str>,
) -> String {
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(100, 0)).unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let parent = parent.map(|id| repo.find_commit(id).unwrap());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let buffer = repo
        .commit_create_buffer(&sig, &sig, message, &tree, &parents)
        .unwrap();
    let mut text = buffer.as_str().unwrap().to_string();
    if let Some(header) = header {
        let end = text.find("\n\n").unwrap();
        text.insert_str(end, &format!("\n{header}"));
    }
    repo.odb()
        .unwrap()
        .write(git2::ObjectType::Commit, text.as_bytes())
        .unwrap()
        .to_string()
}

const CONFLICTED: &str = "[conflict] change\n\nGitButler-Conflict: This is a GitButler-managed conflicted commit.\n   It keeps more trees.\n";

#[test]
fn commit_patch_of_a_gitbutler_conflicted_commit_shows_its_auto_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    let base = commit_tree(
        &raw,
        tree_of(&raw, &[("a.txt", "one\n")]),
        None,
        "base",
        None,
    );
    let tree = conflicted_tree(&raw, &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    let id = commit_tree(
        &raw,
        tree,
        Some(Oid::from_str(&base).unwrap()),
        CONFLICTED,
        None,
    );

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("diff --git a/a.txt b/a.txt"), "{patch}");
    assert!(patch.contains("-one\n+two\n"), "{patch}");
    assert!(!patch.contains(".auto-resolution"), "{patch}");
    assert!(!patch.contains(".conflict-"), "{patch}");
}

#[test]
fn commit_patch_on_a_gitbutler_conflicted_parent_diffs_against_its_auto_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    let tree = conflicted_tree(&raw, &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    let parent = commit_tree(&raw, tree, None, CONFLICTED, None);
    let files = [("a.txt", "two\n"), ("b.txt", "new\n")];
    let child = tree_of(&raw, &files);
    let id = commit_tree(
        &raw,
        child,
        Some(Oid::from_str(&parent).unwrap()),
        "child",
        None,
    );

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("diff --git a/b.txt b/b.txt"), "{patch}");
    assert!(!patch.contains("a.txt"), "{patch}");
    assert!(!patch.contains(".auto-resolution"), "{patch}");
    assert!(!patch.contains(".conflict-"), "{patch}");
}

#[test]
fn tree_patch_to_a_gitbutler_conflicted_tip_shows_its_auto_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    let base = commit_tree(
        &raw,
        tree_of(&raw, &[("a.txt", "one\n")]),
        None,
        "base",
        None,
    );
    let tree = conflicted_tree(&raw, &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    let tip = commit_tree(
        &raw,
        tree,
        Some(Oid::from_str(&base).unwrap()),
        CONFLICTED,
        None,
    );

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.tree_patch(&format!("{tip}^"), &tip).unwrap();

    assert!(patch.contains("-one\n+two\n"), "{patch}");
    assert!(!patch.contains(".conflict-"), "{patch}");
}

#[test]
fn a_gitbutler_conflicted_header_marks_a_conflicted_commit() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    let base = commit_tree(
        &raw,
        tree_of(&raw, &[("a.txt", "one\n")]),
        None,
        "base",
        None,
    );
    let tree = conflicted_tree(&raw, &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    let parent = Some(Oid::from_str(&base).unwrap());
    let id = commit_tree(&raw, tree, parent, "change", Some("gitbutler-conflicted 1"));

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("-one\n+two\n"), "{patch}");
    assert!(!patch.contains(".conflict-"), "{patch}");
}

#[test]
fn a_conflict_subject_alone_shows_the_tree_as_committed() {
    let dir = tempfile::tempdir().unwrap();
    let raw = init_repo(dir.path());
    let base = commit_tree(
        &raw,
        tree_of(&raw, &[("a.txt", "one\n")]),
        None,
        "base",
        None,
    );
    let tree = conflicted_tree(&raw, &[("a.txt", "one\n")], &[("a.txt", "two\n")]);
    let parent = Some(Oid::from_str(&base).unwrap());
    let id = commit_tree(&raw, tree, parent, "[conflict] change", None);

    let repo = Repo::discover_at(dir.path()).unwrap();
    let patch = repo.commit_patch(&id).unwrap();

    assert!(patch.contains("b/.auto-resolution/a.txt"), "{patch}");
    assert!(!patch.contains("-one\n+two\n"), "{patch}");
}
