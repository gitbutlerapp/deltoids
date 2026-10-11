use std::fs;
use std::path::Path;

use crossterm::event::KeyCode;
use git2::{Repository, Signature};

use deltoids::Theme;

use super::Workspace;
use crate::cli::browse::files::FilesMode;
use crate::cli::browse::mode::{AppCommand, Mode};
use crate::cli::browse::test_screen::{PANE_HEIGHT, left_text};

/// A repo on `main` with one commit and a staged edit. Staged, so its
/// blob resolves from the object database rather than the process cwd.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Ada", "ada@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "add a", &tree, &[])
        .unwrap();
    fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();
    dir
}

fn workspace(dir: &Path) -> Workspace {
    Workspace::new(FilesMode::discover_at(dir, &Theme::default(), 60))
}

fn press(workspace: &mut Workspace, key: KeyCode) -> AppCommand {
    workspace.handle_key(key, PANE_HEIGHT)
}

#[test]
fn opens_on_the_working_tree() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    let left = left_text(&mut workspace);

    assert!(left.contains("Files"), "{left}");
    assert!(left.contains("a.txt"), "{left}");
}

#[test]
fn b_opens_the_branches_and_esc_comes_back_to_the_working_tree() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    assert!(workspace.reserves_key(KeyCode::Char('b')));

    press(&mut workspace, KeyCode::Char('b'));
    let left = left_text(&mut workspace);
    assert!(left.contains("Branches"), "{left}");
    assert!(left.contains("main"), "{left}");

    press(&mut workspace, KeyCode::Esc);
    let left = left_text(&mut workspace);
    assert!(left.contains("Files"), "{left}");
    assert!(left.contains("a.txt"), "{left}");
}

#[test]
fn history_drills_down_to_a_commit_diff() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    for key in [KeyCode::Char('b'), KeyCode::Enter, KeyCode::Enter] {
        press(&mut workspace, key);
    }

    let left = left_text(&mut workspace);
    assert!(left.contains("Commit "), "{left}");
    assert!(left.contains("a.txt"), "{left}");
}

#[test]
fn b_does_nothing_outside_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, KeyCode::Char('b'));

    let left = left_text(&mut workspace);
    assert!(left.contains("Files"), "{left}");
}

#[test]
fn a_comment_editor_takes_b_as_text() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    // Settle the diff, then put the cursor on a diff line and comment.
    left_text(&mut workspace);
    for key in [KeyCode::Char('2'), KeyCode::Char('j'), KeyCode::Char('c')] {
        press(&mut workspace, key);
    }
    assert!(workspace.captures_text_input());

    press(&mut workspace, KeyCode::Char('b'));

    assert!(workspace.captures_text_input());
    let left = left_text(&mut workspace);
    assert!(left.contains("Files"), "{left}");
}
