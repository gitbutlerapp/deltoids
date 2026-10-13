use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use git2::{Repository, Signature, Time};

use deltoids::Theme;

use super::Workspace;
use crate::cli::browse::files::FilesMode;
use crate::cli::browse::mode::{Mode, Viewport};
use crate::cli::browse::test_screen::{PANE_HEIGHT, left_text, right_focused, right_text};

/// A repo on `main` with two commits, a `feature` branch forked from
/// the first with one commit of its own, and a staged edit on `main`.
/// The edit is staged so its blob resolves from the object database
/// rather than the process cwd.
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
    fs::write(
        dir.path().join("greet.rs"),
        "fn greet() {\n    println!(\"Hi\");\n}\n",
    )
    .unwrap();
    stage_all(&repo);
    dir
}

/// Edit the staged `greet.rs` again without staging, so it has both a
/// staged and an unstaged change. The edit's blob goes into the object
/// database so it resolves without reading the process cwd.
fn edit_unstaged(dir: &Path) {
    let text = "fn greet() {\n    println!(\"Hey\");\n}\n";
    fs::write(dir.join("greet.rs"), text).unwrap();
    Repository::open(dir)
        .unwrap()
        .blob(text.as_bytes())
        .unwrap();
}

fn stage_all(repo: &Repository) -> git2::Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    index.write_tree().unwrap()
}

fn commit(dir: &Path, repo: &Repository, files: &[(&str, &str)], msg: &str, seconds: i64) {
    for (path, text) in files {
        fs::write(dir.join(path), text).unwrap();
    }
    let tree = repo.find_tree(stage_all(repo)).unwrap();
    let sig = Signature::new("Ada Lovelace", "ada@example.com", &Time::new(seconds, 0)).unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .unwrap();
}

fn workspace(dir: &Path) -> Workspace {
    Workspace::new(FilesMode::discover_at(dir, &Theme::default(), 60))
}

fn press(workspace: &mut Workspace, keys: &[KeyCode]) {
    for key in keys {
        workspace.handle_key(*key, PANE_HEIGHT);
    }
}

fn click(workspace: &mut Workspace, column: u16, row: u16) {
    workspace.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        PANE_HEIGHT,
    );
}

/// The rows of the left column, top to bottom.
fn left_rows(workspace: &mut Workspace) -> Vec<String> {
    left_text(workspace).lines().map(str::to_string).collect()
}

/// Screen rows of the side panel whose title shows `[n]`: from its top
/// border to its bottom border.
fn panel_rows(workspace: &mut Workspace, n: char) -> std::ops::RangeInclusive<usize> {
    let rows = left_rows(workspace);
    let badge = format!("[{n}]");
    let start = rows
        .iter()
        .position(|row| row.contains(&badge))
        .unwrap_or_else(|| panic!("no panel {badge}:\n{}", rows.join("\n")));
    let end = start
        + rows[start..]
            .iter()
            .position(|row| row.starts_with('╰'))
            .expect("panel has a bottom border");
    start..=end
}

/// The text of the side panel whose title shows `[n]`.
fn panel(workspace: &mut Workspace, n: char) -> String {
    let rows = left_rows(workspace);
    rows[panel_rows(workspace, n)].join("\n")
}

fn panel_height(workspace: &mut Workspace, n: char) -> usize {
    panel_rows(workspace, n).count()
}

/// The side panel with the most rows: the current one.
fn tallest(workspace: &mut Workspace) -> char {
    ['1', '2']
        .into_iter()
        .max_by_key(|&n| panel_height(workspace, n))
        .unwrap()
}

#[test]
fn side_panels_stack_files_over_branches_beside_the_main_view() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    let left = left_text(&mut workspace);
    let files = left.find("[1]─Files").expect("Files panel");
    let branches = left.find("[2]─Local branches").expect("branches panel");

    assert!(files < branches, "{left}");
    assert!(!left.contains("Status"), "{left}");
    assert!(!left.contains("[3]"), "{left}");
    let right = right_text(&mut workspace);
    assert!(right.lines().next().unwrap().contains("[0]"), "{right}");
}

#[test]
fn opens_on_the_files_panel_with_the_working_tree_diff() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    assert_eq!(tallest(&mut workspace), '1');
    assert!(!right_focused(&mut workspace));
    assert!(panel(&mut workspace, '1').contains("greet.rs"));
    let right = right_text(&mut workspace);
    assert!(right.contains("Hi"), "{right}");
}

#[test]
fn the_branches_panel_lists_local_branches_checked_out_first() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    let branches = panel(&mut workspace, '2');

    let main = branches.find("main").expect("main listed");
    let feature = branches.find("feature").expect("feature listed");
    assert!(main < feature, "{branches}");
}

#[test]
fn enter_lists_a_branch_commits_newest_first() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Char('2'), KeyCode::Enter]);

    let commits = panel(&mut workspace, '2');
    assert!(commits.contains("Commits (main)"), "{commits}");
    let newest = commits.find("shout greeting").expect("head listed");
    let oldest = commits.find("add greeting").expect("root listed");
    assert!(newest < oldest, "{commits}");
    assert!(!commits.contains("add feature"), "{commits}");
}

#[test]
fn number_keys_focus_side_panels_and_the_current_one_grows() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    for n in ['2', '1'] {
        press(&mut workspace, &[KeyCode::Char(n)]);
        assert_eq!(tallest(&mut workspace), n);
    }
    // There is no third panel.
    press(&mut workspace, &[KeyCode::Char('3')]);
    assert_eq!(tallest(&mut workspace), '1');
}

#[test]
fn tab_and_shift_tab_switch_between_the_side_panels() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Tab]);
    assert_eq!(tallest(&mut workspace), '2');
    press(&mut workspace, &[KeyCode::Tab]);
    assert_eq!(tallest(&mut workspace), '1');
    press(&mut workspace, &[KeyCode::BackTab]);
    assert_eq!(tallest(&mut workspace), '2');
    assert!(!right_focused(&mut workspace));
}

#[test]
fn h_and_l_switch_between_the_side_panels() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Char('l')]);
    assert_eq!(tallest(&mut workspace), '2');
    press(&mut workspace, &[KeyCode::Right]);
    assert_eq!(tallest(&mut workspace), '1');
    press(&mut workspace, &[KeyCode::Char('h')]);
    assert_eq!(tallest(&mut workspace), '2');
    press(&mut workspace, &[KeyCode::Left]);
    assert_eq!(tallest(&mut workspace), '1');
}

#[test]
fn enter_on_a_file_focuses_its_diff_and_esc_comes_back() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Enter]);
    assert!(right_focused(&mut workspace));

    press(&mut workspace, &[KeyCode::Esc]);
    assert!(!right_focused(&mut workspace));
    assert_eq!(tallest(&mut workspace), '1');
}

#[test]
fn enter_on_a_commit_file_focuses_its_diff() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(
        &mut workspace,
        &[KeyCode::Char('2'), KeyCode::Enter, KeyCode::Enter],
    );
    assert!(panel(&mut workspace, '2').contains("greet.rs"));
    assert!(!right_focused(&mut workspace));

    press(&mut workspace, &[KeyCode::Enter]);
    assert!(right_focused(&mut workspace));

    press(&mut workspace, &[KeyCode::Esc]);
    assert!(!right_focused(&mut workspace));
    // Still in the commit's files: Esc left the diff, not the commit.
    assert!(panel(&mut workspace, '2').contains("greet.rs"));
}

#[test]
fn tab_in_the_main_view_switches_staged_and_unstaged() {
    let dir = fixture();
    edit_unstaged(dir.path());
    let mut workspace = workspace(dir.path());
    press(&mut workspace, &[KeyCode::Char('0')]);
    let unstaged = right_text(&mut workspace);
    assert!(unstaged.contains("Hey"), "{unstaged}");
    assert!(!unstaged.contains("HELLO"), "{unstaged}");

    press(&mut workspace, &[KeyCode::Tab]);
    let staged = right_text(&mut workspace);
    assert!(staged.contains("HELLO"), "{staged}");
    assert!(!staged.contains("Hey"), "{staged}");
    assert!(right_focused(&mut workspace));

    press(&mut workspace, &[KeyCode::BackTab]);
    assert!(right_text(&mut workspace).contains("Hey"));
}

#[test]
fn tab_in_the_main_view_never_leaves_it() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(&mut workspace, &[KeyCode::Char('2'), KeyCode::Char('0')]);

    press(&mut workspace, &[KeyCode::Tab]);

    assert!(right_focused(&mut workspace));
    assert_eq!(tallest(&mut workspace), '2');
}

#[test]
fn the_main_view_shows_the_focused_branch_log() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Char('2')]);
    let right = right_text(&mut workspace);
    assert!(right.contains("shout greeting"), "{right}");
    assert!(right.contains("add greeting"), "{right}");

    press(&mut workspace, &[KeyCode::Char('j')]);
    let right = right_text(&mut workspace);
    assert!(right.contains("add feature"), "{right}");
    assert!(!right.contains("shout greeting"), "{right}");
}

#[test]
fn keys_in_the_focused_log_scroll_it() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(&mut workspace, &[KeyCode::Char('2'), KeyCode::Char('0')]);

    press(&mut workspace, &[KeyCode::Char('j')]);

    let right = right_text(&mut workspace);
    assert!(!right.contains("shout greeting"), "{right}");
    assert!(right.contains("add greeting"), "{right}");
    // Still main's branch: the side selection did not move.
    assert!(!right.contains("add feature"), "{right}");
}

#[test]
fn the_main_view_shows_the_selected_commit_diff() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());

    press(&mut workspace, &[KeyCode::Char('2'), KeyCode::Enter]);
    let right = right_text(&mut workspace);
    assert!(right.contains("HELLO"), "{right}");

    press(&mut workspace, &[KeyCode::Char('j')]);
    let right = right_text(&mut workspace);
    assert!(right.contains("hello"), "{right}");
    assert!(!right.contains("HELLO"), "{right}");
}

#[test]
fn enter_on_a_branch_drills_into_its_commits_and_files_and_esc_climbs_back() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(
        &mut workspace,
        &[KeyCode::Char('2'), KeyCode::Char('j'), KeyCode::Enter],
    );

    let commits = panel(&mut workspace, '2');
    assert!(commits.contains("Commits (feature)"), "{commits}");
    assert!(commits.contains("add feature"), "{commits}");
    assert!(right_text(&mut workspace).contains("fn feature()"));

    press(&mut workspace, &[KeyCode::Enter]);
    let files = panel(&mut workspace, '2');
    let short = feature_tip(dir.path());
    assert!(files.contains(&format!("Commit {short}")), "{files}");
    assert!(files.contains("feature.rs"), "{files}");
    assert!(right_text(&mut workspace).contains("fn feature()"));

    press(&mut workspace, &[KeyCode::Esc]);
    assert!(panel(&mut workspace, '2').contains("Commits (feature)"));
    press(&mut workspace, &[KeyCode::Esc]);
    assert!(panel(&mut workspace, '2').contains("Local branches"));
    // The branch selection survived the round trip.
    assert!(right_text(&mut workspace).contains("add feature"));
    press(&mut workspace, &[KeyCode::Esc]);
    assert!(panel(&mut workspace, '2').contains("Local branches"));
}

fn feature_tip(dir: &Path) -> String {
    let repo = Repository::open(dir).unwrap();
    let id = repo.revparse_single("feature").unwrap().id().to_string();
    id[..7].to_string()
}

#[test]
fn zero_focuses_the_main_view_and_esc_returns_to_the_side() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(
        &mut workspace,
        &[KeyCode::Char('2'), KeyCode::Enter, KeyCode::Char('0')],
    );
    assert!(right_focused(&mut workspace));

    // `j` walks the diff, not the commit list.
    press(&mut workspace, &[KeyCode::Char('j')]);
    assert!(right_text(&mut workspace).contains("HELLO"));

    press(&mut workspace, &[KeyCode::Esc, KeyCode::Char('j')]);
    assert!(!right_focused(&mut workspace));
    let right = right_text(&mut workspace);
    assert!(right.contains("hello"), "{right}");
    assert!(!right.contains("HELLO"), "{right}");
}

#[test]
fn clicking_a_panel_row_focuses_the_panel_and_selects_the_row() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    let rows = left_rows(&mut workspace);
    let feature = rows
        .iter()
        .position(|row| row.contains("feature") && !row.contains("add feature"))
        .expect("feature branch on screen") as u16;

    click(&mut workspace, 5, feature);

    assert_eq!(tallest(&mut workspace), '2');
    assert!(right_text(&mut workspace).contains("add feature"));
}

#[test]
fn a_comment_editor_takes_digits_and_tab_as_text() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    right_text(&mut workspace);
    press(
        &mut workspace,
        &[KeyCode::Char('0'), KeyCode::Char('j'), KeyCode::Char('c')],
    );
    assert!(workspace.captures_text_input());

    press(&mut workspace, &[KeyCode::Char('2'), KeyCode::Tab]);

    assert!(workspace.captures_text_input());
    assert_eq!(tallest(&mut workspace), '1');
}

#[test]
fn panel_keys_are_reserved_over_custom_commands() {
    let dir = fixture();
    let workspace = workspace(dir.path());

    for key in ['0', '1', '2', 'h', 'l'] {
        assert!(workspace.reserves_key(KeyCode::Char(key)), "{key}");
    }
    assert!(!workspace.reserves_key(KeyCode::Char('3')));
}

#[test]
fn outside_a_repository_the_branches_panel_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = workspace(dir.path());

    assert!(panel(&mut workspace, '2').contains("Not a git repository"));
}

#[test]
fn a_reload_picks_up_new_commits_and_branches() {
    let dir = fixture();
    let mut workspace = workspace(dir.path());
    press(&mut workspace, &[KeyCode::Char('2')]);
    assert!(!right_text(&mut workspace).contains("say hi"));
    let repo = Repository::open(dir.path()).unwrap();
    commit(dir.path(), &repo, &[], "say hi", 400);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("fresh", &head, false).unwrap();

    let viewport = Viewport {
        height: PANE_HEIGHT,
        diff_width: 60,
    };
    workspace.reload(viewport, &Theme::default()).unwrap();

    // The checked-out branch's log, in the main view.
    assert!(right_text(&mut workspace).contains("say hi"));
    assert!(panel(&mut workspace, '2').contains("fresh"));
}
