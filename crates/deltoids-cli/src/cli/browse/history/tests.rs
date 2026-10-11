use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use git2::{Repository, Signature, Time};

use deltoids::git;

use super::History;
use crate::cli::browse::mode::Mode;
use crate::cli::browse::test_screen::{PANE_HEIGHT, left_text, right_text, screen_text};

/// A repo on `main` with two commits, plus a `feature` branch forked
/// from the first commit with one commit of its own.
///
/// main:    add greeting -> shout greeting
/// feature: add greeting -> add feature
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    commit(
        dir.path(),
        &repo,
        &[("greet.rs", "fn greet() {\n    println!(\"hello\");\n}\n")],
        "add greeting",
        100,
    );
    let first = repo.head().unwrap().peel_to_commit().unwrap();
    commit(
        dir.path(),
        &repo,
        &[("greet.rs", "fn greet() {\n    println!(\"HELLO\");\n}\n")],
        "shout greeting",
        300,
    );
    repo.branch("feature", &first, false).unwrap();
    repo.set_head("refs/heads/feature").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    commit(
        dir.path(),
        &repo,
        &[("feature.rs", "fn feature() {}\n")],
        "add feature",
        200,
    );
    repo.set_head("refs/heads/main").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    dir
}

fn commit(dir: &Path, repo: &Repository, files: &[(&str, &str)], msg: &str, seconds: i64) {
    for (path, text) in files {
        fs::write(dir.join(path), text).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(seconds, 0)).unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .unwrap();
}

fn open(dir: &Path) -> History {
    History::open(git::Repo::discover_at(dir).unwrap())
}

fn press(history: &mut History, keys: &[KeyCode]) {
    for key in keys {
        history.handle_key(*key, PANE_HEIGHT);
    }
}

fn click(history: &mut History, column: u16, row: u16) {
    history.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
        PANE_HEIGHT,
    );
}

/// The row of the left column that shows `needle`.
fn row_of(history: &mut History, needle: &str) -> u16 {
    left_text(history)
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not on screen")) as u16
}

#[test]
fn opens_on_local_branches_with_the_checked_out_one_first() {
    let dir = fixture();
    let mut history = open(dir.path());

    let left = left_text(&mut history);

    assert!(left.contains("Branches"), "{left}");
    let main = left.find("main").expect("main listed");
    let feature = left.find("feature").expect("feature listed");
    assert!(main < feature, "{left}");
}

#[test]
fn the_right_pane_shows_the_selected_branch_log() {
    let dir = fixture();
    let mut history = open(dir.path());

    let right = right_text(&mut history);
    assert!(right.contains("shout greeting"), "{right}");
    assert!(right.contains("add greeting"), "{right}");
    assert!(!right.contains("add feature"), "{right}");

    press(&mut history, &[KeyCode::Char('j')]);
    let right = right_text(&mut history);
    assert!(right.contains("add feature"), "{right}");
    assert!(!right.contains("shout greeting"), "{right}");
}

#[test]
fn enter_lists_the_commits_of_the_selected_branch() {
    let dir = fixture();
    let mut history = open(dir.path());

    press(&mut history, &[KeyCode::Char('j'), KeyCode::Enter]);

    let left = left_text(&mut history);
    assert!(left.contains("Commits"), "{left}");
    assert!(left.contains("feature"), "{left}");
    let newest = left.find("add feature").expect("feature commit listed");
    let oldest = left.find("add greeting").expect("root commit listed");
    assert!(newest < oldest, "{left}");
    assert!(!left.contains("shout greeting"), "{left}");
}

#[test]
fn the_right_pane_previews_the_selected_commit_diff() {
    let dir = fixture();
    let mut history = open(dir.path());
    press(&mut history, &[KeyCode::Enter]);

    let right = right_text(&mut history);
    assert!(right.contains("greet.rs"), "{right}");
    assert!(right.contains("HELLO"), "{right}");

    press(&mut history, &[KeyCode::Char('j')]);
    let right = right_text(&mut history);
    assert!(right.contains("hello"), "{right}");
    assert!(!right.contains("HELLO"), "{right}");
}

#[test]
fn enter_on_a_commit_shows_its_files_and_diff() {
    let dir = fixture();
    let mut history = open(dir.path());
    press(
        &mut history,
        &[KeyCode::Char('j'), KeyCode::Enter, KeyCode::Enter],
    );

    let screen = screen_text(&mut history);
    let left = left_text(&mut history);
    assert!(left.contains("feature.rs"), "{screen}");
    let commit = git::Repo::discover_at(dir.path())
        .unwrap()
        .commits("feature", 1)
        .unwrap()
        .remove(0);
    assert!(left.contains(&commit.short_id), "{screen}");
    assert!(
        right_text(&mut history).contains("fn feature()"),
        "{screen}"
    );
}

#[test]
fn esc_walks_back_up_to_the_branches_and_then_closes() {
    let dir = fixture();
    let mut history = open(dir.path());
    press(
        &mut history,
        &[
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Enter,
        ],
    );

    press(&mut history, &[KeyCode::Esc]);
    let left = left_text(&mut history);
    assert!(left.contains("Commits"), "{left}");
    // The selection survives the round trip: the root commit's diff.
    assert!(right_text(&mut history).contains("hello"));

    press(&mut history, &[KeyCode::Esc]);
    let left = left_text(&mut history);
    assert!(left.contains("Branches"), "{left}");
    assert!(right_text(&mut history).contains("add feature"));
    assert!(!history.is_closed());

    press(&mut history, &[KeyCode::Esc]);
    assert!(history.is_closed());
}

#[test]
fn a_commit_view_never_touches_the_index_or_working_tree() {
    let dir = fixture();
    let mut history = open(dir.path());
    press(&mut history, &[KeyCode::Enter, KeyCode::Enter]);
    let status = || {
        let repo = Repository::open(dir.path()).unwrap();
        let statuses = repo.statuses(None).unwrap();
        statuses.len()
    };

    assert!(!history.reserves_key(KeyCode::Char(' ')));
    assert!(!history.reserves_key(KeyCode::Char('a')));
    press(&mut history, &[KeyCode::Char(' '), KeyCode::Char('a')]);
    let screen = screen_text(&mut history);
    // `d` on a file offers no discard menu.
    press(&mut history, &[KeyCode::Char('d')]);
    assert!(!history.captures_text_input());

    assert_eq!(status(), 0, "{screen}");
    assert_eq!(
        fs::read_to_string(dir.path().join("greet.rs")).unwrap(),
        "fn greet() {\n    println!(\"HELLO\");\n}\n"
    );
}

#[test]
fn clicking_a_branch_selects_it() {
    let dir = fixture();
    let mut history = open(dir.path());

    let row = row_of(&mut history, "feature");
    click(&mut history, 5, row);

    assert!(right_text(&mut history).contains("add feature"));
}

#[test]
fn clicking_a_commit_selects_it() {
    let dir = fixture();
    let mut history = open(dir.path());
    press(&mut history, &[KeyCode::Enter]);

    let row = row_of(&mut history, "add greeting");
    click(&mut history, 5, row);

    let right = right_text(&mut history);
    assert!(right.contains("hello"), "{right}");
    assert!(!right.contains("HELLO"), "{right}");
}

#[test]
fn an_unborn_repo_has_no_branches() {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(dir.path()).unwrap();
    let mut history = open(dir.path());

    press(&mut history, &[KeyCode::Enter, KeyCode::Char('j')]);

    let left = left_text(&mut history);
    assert!(left.contains("Branches"), "{left}");
    assert!(left.contains("No branches"), "{left}");
}

#[test]
fn ages_read_in_the_largest_whole_unit() {
    use super::age;
    assert_eq!(age(1_000, 1_000), "0s");
    assert_eq!(age(1_059, 1_000), "59s");
    assert_eq!(age(1_000 + 60, 1_000), "1m");
    assert_eq!(age(1_000 + 2 * 3600, 1_000), "2h");
    assert_eq!(age(1_000 + 3 * 86_400, 1_000), "3d");
    assert_eq!(age(1_000 + 14 * 86_400, 1_000), "2w");
    assert_eq!(age(1_000 + 60 * 86_400, 1_000), "2M");
    assert_eq!(age(1_000 + 800 * 86_400, 1_000), "2y");
    // A commit from the future (clock skew) reads as now.
    assert_eq!(age(1_000, 2_000), "0s");
}
