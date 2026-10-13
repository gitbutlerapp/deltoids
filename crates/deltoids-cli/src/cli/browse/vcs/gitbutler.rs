//! The GitButler adapter. Reads the workspace through git2 like the git
//! adapter, and asks the `but` CLI (its `--json` output) for everything
//! GitButler owns: which branches exist and how they stack, and the IDs
//! its writes take.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use deltoids::git::{self, FileStageStatus, StageChange, WorkingTreeSnapshot};
use serde::Deserialize;

use super::Branch;

/// The `but` executable `Vcs` runs, looked up on `PATH`.
pub(super) const BUT: &str = "but";

/// Whether `HEAD` sits on one of GitButler's own branches, as it does in
/// a GitButler workspace.
pub(super) fn is_workspace(head_ref: Option<&str>) -> bool {
    head_ref.is_some_and(|reference| reference.starts_with("refs/heads/gitbutler/"))
}

/// The working tree as GitButler shows it: one column of uncommitted
/// changes against `HEAD`, each file's change taken against `HEAD` too.
pub(super) fn uncommitted(snapshot: WorkingTreeSnapshot) -> WorkingTreeSnapshot {
    let stages = snapshot
        .stages
        .into_iter()
        .map(|status| FileStageStatus {
            unstaged: Some(net_change(status.staged, status.unstaged)),
            staged: None,
            ..status
        })
        .collect();
    WorkingTreeSnapshot {
        patch: snapshot.patch,
        staged_patch: String::new(),
        unstaged_patch: String::new(),
        stages,
    }
}

/// A file's change from `HEAD` to the worktree, from its two git status
/// columns. A file `HEAD` lacks is added, unless the worktree lacks it
/// too: GitButler lists that leftover index entry as removed.
fn net_change(staged: Option<StageChange>, unstaged: Option<StageChange>) -> StageChange {
    use StageChange::{Added, Deleted, Modified, Renamed, TypeChanged, Untracked};
    let new_file = staged == Some(Added) || unstaged == Some(Untracked);
    let either = |change| staged == Some(change) || unstaged == Some(change);
    match (staged, unstaged) {
        (Some(Deleted), Some(Untracked)) => Modified,
        _ if new_file && unstaged == Some(Deleted) => Deleted,
        _ if new_file => Added,
        _ if either(Renamed) => Renamed,
        _ if either(Deleted) => Deleted,
        _ if either(TypeChanged) => TypeChanged,
        _ => Modified,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchList {
    #[serde(default)]
    applied_stacks: Vec<Stack>,
    #[serde(default)]
    branches: Vec<ListedBranch>,
}

#[derive(Deserialize)]
struct Stack {
    #[serde(default)]
    heads: Vec<ListedBranch>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedBranch {
    name: String,
    /// Whether a local branch exists; `false` for one only on a remote.
    #[serde(default = "local")]
    has_local: bool,
    /// Milliseconds since the epoch.
    #[serde(default)]
    last_commit_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    #[serde(default)]
    uncommitted_changes: Vec<UncommittedChange>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UncommittedChange {
    cli_id: String,
    file_path: String,
}

/// GitButler's branches: every applied branch in stack order, marked
/// current, then the unapplied local ones in the order `but` lists them.
/// Branches that exist only on a remote are left out.
pub(super) fn branches(program: &OsStr, repo: &git::Repo) -> Result<Vec<Branch>, String> {
    let workdir = repo.workdir().ok_or("GitButler needs a working tree")?;
    let list: BranchList = parse(&run(program, workdir, &["branch", "list"])?)?;
    let applied = list
        .applied_stacks
        .into_iter()
        .flat_map(|stack| stack.heads)
        .map(|branch| (branch, true));
    let unapplied = list
        .branches
        .into_iter()
        .filter(|branch| branch.has_local)
        .map(|branch| (branch, false));
    Ok(applied
        .chain(unapplied)
        .map(|(branch, current)| row(repo, branch, current))
        .collect())
}

fn row(repo: &git::Repo, branch: ListedBranch, current: bool) -> Branch {
    let rev = format!("refs/heads/{}", branch.name);
    let tip = repo
        .commits(&rev, 1)
        .ok()
        .and_then(|commits| commits.into_iter().next());
    Branch {
        summary: tip.as_ref().map(|c| c.summary.clone()).unwrap_or_default(),
        time: branch
            .last_commit_at
            .map(|ms| ms / 1000)
            .or(tip.map(|c| c.time))
            .unwrap_or(0),
        name: branch.name,
        current,
        rev,
    }
}

/// Discard every change in `changes` (each one's paths, rename sources
/// included) with one `but discard`, so `but undo` reverts it at once.
/// Fails without writing when GitButler does not list one of them.
pub(super) fn discard(
    program: &OsStr,
    workdir: &Path,
    changes: &[Vec<PathBuf>],
) -> Result<(), String> {
    let status: Status = parse(&run(program, workdir, &["status"])?)?;
    let mut ids = BTreeSet::new();
    for paths in changes {
        let id = status
            .uncommitted_changes
            .iter()
            .find(|change| {
                paths
                    .iter()
                    .any(|path| Path::new(&change.file_path) == path)
            })
            .ok_or_else(|| {
                let shown = paths
                    .last()
                    .map_or(String::new(), |p| p.display().to_string());
                format!("GitButler does not list {shown} as uncommitted")
            })?;
        ids.insert(id.cli_id.as_str());
    }
    if ids.is_empty() {
        return Ok(());
    }
    let args: Vec<&str> = std::iter::once("discard").chain(ids).collect();
    run(program, workdir, &args).map(drop)
}

fn local() -> bool {
    true
}

fn parse<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, String> {
    serde_json::from_str(json).map_err(|e| format!("Unexpected output from but: {e}"))
}

/// Run `but -C <workdir> --json <args>` and return its stdout.
fn run(program: &OsStr, workdir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .arg("-C")
        .arg(workdir)
        .arg("--json")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "GitButler CLI (but) not found".to_string(),
            _ => format!("Could not run but: {e}"),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    Err(failure(&stdout, &String::from_utf8_lossy(&output.stderr)))
}

/// The message of a failed `but` run: its JSON error, or else the first
/// line of stderr.
fn failure(stdout: &str, stderr: &str) -> String {
    #[derive(Deserialize)]
    struct Failure {
        message: String,
    }
    if let Ok(failure) = serde_json::from_str::<Failure>(stdout) {
        return failure.message;
    }
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || "but failed".to_string(),
            |line| line.trim_start_matches("Error: ").to_string(),
        )
}

#[cfg(test)]
mod tests;
