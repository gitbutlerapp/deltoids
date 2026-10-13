use std::fs;
use std::path::{Path, PathBuf};

use deltoids::git::StageChange;
use git2::{Repository, Signature};
use tempfile::TempDir;

use super::super::{Columns, DiscardKind, Vcs, Writer};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/browse/vcs/fixtures");

fn commit_all(repo: &Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Test", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap();
}

/// A repository with one commit holding `files`.
fn repo_with(files: &[&str]) -> (TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    for file in files {
        fs::write(dir.path().join(file), format!("{file}\n")).unwrap();
    }
    commit_all(&repo, "initial");
    (dir, repo)
}

/// Move `HEAD` onto a GitButler workspace branch at the current commit.
fn enter_workspace(repo: &Repository) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("gitbutler/workspace", &head, true).unwrap();
    repo.set_head("refs/heads/gitbutler/workspace").unwrap();
}

fn stage(repo: &Repository, path: &str) {
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
}

/// A `but` stand-in that logs each command line to `but.log` and
/// answers `status` and `branch list` from the recorded fixtures.
#[cfg(unix)]
fn stub_but(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("but");
    let log = dir.join("but.log");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nshift 3\necho \"$*\" >> '{log}'\ncase \"$1\" in\n  status) cat '{FIXTURES}/but-status.json' ;;\n  branch) cat '{FIXTURES}/but-branch-list.json' ;;\nesac\n",
            log = log.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn but_log(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("but.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_workspace_shows_one_column_of_changes_against_head() {
    let (dir, repo) = repo_with(&["modified.txt", "renamed.txt"]);
    enter_workspace(&repo);
    fs::write(dir.path().join("modified.txt"), "edit\n").unwrap();
    fs::write(dir.path().join("untracked.txt"), "new\n").unwrap();
    fs::write(dir.path().join("staged-new.txt"), "staged\n").unwrap();
    stage(&repo, "staged-new.txt");
    fs::write(dir.path().join("staged-new.txt"), "staged then edited\n").unwrap();
    fs::write(dir.path().join("gone.txt"), "gone\n").unwrap();
    stage(&repo, "gone.txt");
    fs::remove_file(dir.path().join("gone.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("renamed.txt")).unwrap();
    fs::rename(dir.path().join("renamed.txt"), dir.path().join("moved.txt")).unwrap();
    index.add_path(Path::new("moved.txt")).unwrap();
    index.write().unwrap();

    let working = Vcs::discover_at(dir.path())
        .unwrap()
        .working_tree()
        .unwrap();

    assert_eq!(working.columns, Columns::Single);
    assert!(working.snapshot.staged_patch.is_empty());
    assert!(working.snapshot.unstaged_patch.is_empty());
    let mut kinds: Vec<_> = working
        .snapshot
        .stages
        .iter()
        .map(|status| (status.path.as_str(), status.staged, status.unstaged))
        .collect();
    kinds.sort_by_key(|(path, _, _)| *path);
    assert_eq!(
        kinds,
        [
            ("gone.txt", None, Some(StageChange::Deleted)),
            ("modified.txt", None, Some(StageChange::Modified)),
            ("moved.txt", None, Some(StageChange::Renamed)),
            ("staged-new.txt", None, Some(StageChange::Added)),
            ("untracked.txt", None, Some(StageChange::Added)),
        ]
    );
}

#[test]
fn a_plain_repository_keeps_staged_and_unstaged_columns() {
    let (dir, _repo) = repo_with(&["a.txt"]);
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    let working = Vcs::discover_at(dir.path())
        .unwrap()
        .working_tree()
        .unwrap();
    assert_eq!(working.columns, Columns::Split);
}

#[test]
fn staging_in_a_workspace_leaves_the_index_alone() {
    let (dir, repo) = repo_with(&["a.txt"]);
    enter_workspace(&repo);
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    let index_before = fs::read(repo.path().join("index")).unwrap();
    let writer = Writer::at(dir.path());

    let one = writer.toggle_stage(vec![PathBuf::from("a.txt")]).unwrap();
    let all = writer.toggle_stage_all().unwrap();

    assert!(one.contains("no staging"), "{one}");
    assert!(all.contains("no staging"), "{all}");
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_before);
}

#[test]
fn a_workspace_offers_one_discard() {
    let (dir, repo) = repo_with(&["a.txt"]);
    enter_workspace(&repo);
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    let vcs = Vcs::discover_at(dir.path()).unwrap();

    let placeholders = Vcs::discard_placeholders(Some(&vcs));
    let pending = Writer::at(dir.path())
        .discard(vec![PathBuf::from("a.txt")])
        .unwrap();

    assert_eq!(placeholders.len(), 1);
    assert!(!placeholders[0].available);
    let choices = pending.choices();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].kind, DiscardKind::All);
    assert!(choices[0].available);
    assert!(choices[0].description.contains("but undo"));
}

#[test]
fn a_discard_read_before_entering_a_workspace_is_refused() {
    let (dir, repo) = repo_with(&["a.txt"]);
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    let writer = Writer::at(dir.path());
    let pending = writer.discard(vec![PathBuf::from("a.txt")]).unwrap();
    enter_workspace(&repo);

    let error = writer
        .complete_discard(pending, DiscardKind::All)
        .unwrap_err();

    assert!(error.contains("repository changed"), "{error}");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "edit\n"
    );
}

/// The refs `but-branch-list.json` names: three applied branches, a
/// parked local one, and one that only exists on `origin`.
fn workspace_with_branches() -> (TempDir, Repository) {
    let (dir, repo) = repo_with(&["a.txt"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    for name in ["side", "top-feature", "base-feature", "parked"] {
        repo.branch(name, &head, false).unwrap();
    }
    repo.remote("origin", "https://example.com/repo.git")
        .unwrap();
    repo.reference("refs/remotes/origin/remote-only", head.id(), false, "test")
        .unwrap();
    drop(head);
    enter_workspace(&repo);
    (dir, repo)
}

#[cfg(unix)]
#[test]
fn a_workspace_lists_gitbutlers_local_branches_applied_first() {
    let (dir, _repo) = workspace_with_branches();
    let stubs = tempfile::tempdir().unwrap();
    let mut vcs = Vcs::discover_at(dir.path()).unwrap();
    vcs.but = stub_but(stubs.path()).into_os_string();

    let list = vcs.branches();

    assert_eq!(list.label, "Branches");
    let branches = list.branches.unwrap();
    let rows: Vec<_> = branches
        .iter()
        .map(|b| (b.name.as_str(), b.current))
        .collect();
    assert_eq!(
        rows,
        [
            ("side", true),
            ("top-feature", true),
            ("base-feature", true),
            ("parked", false),
        ]
    );
    assert!(branches.iter().all(|b| b.summary == "initial"));
    assert_eq!(vcs.commits(&branches[3], 10).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn the_branch_list_runs_but_again_only_after_a_ref_moves() {
    let (dir, repo) = workspace_with_branches();
    let stubs = tempfile::tempdir().unwrap();
    let mut vcs = Vcs::discover_at(dir.path()).unwrap();
    vcs.but = stub_but(stubs.path()).into_os_string();

    vcs.branches();
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    vcs.branches();
    assert_eq!(but_log(stubs.path()), ["branch list"]);

    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("new-branch", &head, false).unwrap();
    vcs.branches();
    assert_eq!(but_log(stubs.path()), ["branch list", "branch list"]);
}

#[test]
fn a_workspace_without_but_reports_it_in_place_of_branches() {
    let (dir, repo) = repo_with(&["a.txt"]);
    enter_workspace(&repo);
    let mut vcs = Vcs::discover_at(dir.path()).unwrap();
    vcs.but = "/nonexistent/but".into();

    let error = vcs.branches().branches.unwrap_err();

    assert!(error.contains("not found"), "{error}");
}

#[test]
fn a_plain_repository_lists_local_branches() {
    let (dir, repo) = repo_with(&["a.txt"]);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("other", &head, false).unwrap();
    let mut vcs = Vcs::discover_at(dir.path()).unwrap();
    vcs.but = "/nonexistent/but".into();

    let list = vcs.branches();

    assert_eq!(list.label, "Local branches");
    let branches = list.branches.unwrap();
    assert_eq!(branches.len(), 2);
    assert!(branches[0].current);
    assert_eq!(vcs.commits(&branches[1], 10).unwrap().len(), 1);
}

/// The files `but-status.json` lists: `a.txt` modified, `new.txt`
/// added, and `ren.txt` renamed to `ren2.txt`.
#[cfg(unix)]
fn workspace_with_uncommitted_changes() -> TempDir {
    let (dir, repo) = repo_with(&["a.txt", "ren.txt", "kept.txt"]);
    enter_workspace(&repo);
    fs::write(dir.path().join("a.txt"), "edit\n").unwrap();
    fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    fs::rename(dir.path().join("ren.txt"), dir.path().join("ren2.txt")).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn discard_in_a_workspace_runs_but_discard_with_the_selections_ids() {
    let dir = workspace_with_uncommitted_changes();
    let stubs = tempfile::tempdir().unwrap();
    let writer = Writer {
        workdir: dir.path().to_path_buf(),
        but: stub_but(stubs.path()).into_os_string(),
    };
    let targets = ["a.txt", "ren.txt", "ren2.txt"].map(PathBuf::from).to_vec();
    let pending = writer.discard(targets).unwrap();

    let message = writer.complete_discard(pending, DiscardKind::All).unwrap();

    assert!(message.contains("but undo"), "{message}");
    assert_eq!(but_log(stubs.path()), ["status", "discard nkns svvx"]);
}

#[cfg(unix)]
#[test]
fn discard_in_a_workspace_refuses_a_file_gitbutler_does_not_list() {
    let dir = workspace_with_uncommitted_changes();
    fs::write(dir.path().join("kept.txt"), "edit\n").unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let writer = Writer {
        workdir: dir.path().to_path_buf(),
        but: stub_but(stubs.path()).into_os_string(),
    };
    let targets = ["a.txt", "kept.txt"].map(PathBuf::from).to_vec();
    let pending = writer.discard(targets).unwrap();

    let error = writer
        .complete_discard(pending, DiscardKind::All)
        .unwrap_err();

    assert!(error.contains("kept.txt"), "{error}");
    assert_eq!(but_log(stubs.path()), ["status"]);
}
