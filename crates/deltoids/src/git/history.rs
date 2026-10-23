//! Read-only history: local branches, the commits reachable from one, and
//! the patch a commit introduces.

use std::collections::{BinaryHeap, HashSet};

use git2::{BranchType, DiffFindOptions, DiffFormat, DiffOptions, Oid, Repository};

use super::Repo;

/// One local branch, as a branch list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    /// Whether this branch is checked out.
    pub head: bool,
    /// Subject line of the branch's tip commit.
    pub summary: String,
    /// Committer time of the tip commit, in seconds since the epoch.
    pub time: i64,
}

/// One commit, as a commit list and its detail view show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Full hex object id.
    pub id: String,
    /// The first seven hex digits of `id`.
    pub short_id: String,
    pub summary: String,
    /// The full message, subject and body.
    pub message: String,
    pub author: String,
    pub email: String,
    /// Author time, in seconds since the epoch.
    pub time: i64,
}

impl Repo {
    /// Every local branch: the checked-out one first, the rest by the
    /// time of their tip commit, newest first. Empty before the first
    /// commit.
    pub fn local_branches(&self) -> Result<Vec<Branch>, String> {
        let error = |e: git2::Error| format!("failed to list branches: {e}");
        let mut branches = Vec::new();
        for entry in self.0.branches(Some(BranchType::Local)).map_err(error)? {
            let (branch, _) = entry.map_err(error)?;
            let Some(name) = branch.name().map_err(error)? else {
                continue;
            };
            let tip = branch.get().peel_to_commit().map_err(error)?;
            branches.push(Branch {
                name: name.to_string(),
                head: branch.is_head(),
                summary: tip.summary().ok().flatten().unwrap_or_default().to_string(),
                time: tip.time().seconds(),
            });
        }
        branches.sort_by(|a, b| {
            b.head
                .cmp(&a.head)
                .then(b.time.cmp(&a.time))
                .then(a.name.cmp(&b.name))
        });
        Ok(branches)
    }

    /// Up to `limit` commits reachable from `rev` (a branch name or any
    /// revision), newest commit date first, as `git log` lists them. Reads
    /// only the commits it returns and their parents, so the cost follows
    /// `limit`, whatever the size of the history.
    pub fn commits(&self, rev: &str, limit: usize) -> Result<Vec<Commit>, String> {
        self.walk(rev, None, limit)
    }

    /// [`Self::commits`] stopping at `base`: the commits reachable from
    /// `rev` but not from `base`, as `git log base..rev` lists them. A
    /// `base` that does not resolve hides nothing.
    pub fn commits_after(
        &self,
        base: &str,
        rev: &str,
        limit: usize,
    ) -> Result<Vec<Commit>, String> {
        let base = self
            .0
            .revparse_single(base)
            .and_then(|object| object.peel_to_commit())
            .ok()
            .map(|commit| commit.id());
        self.walk(rev, base, limit)
    }

    fn walk(&self, rev: &str, base: Option<Oid>, limit: usize) -> Result<Vec<Commit>, String> {
        let error = |e: git2::Error| format!("failed to read commits of {rev}: {e}");
        let hidden = |id: Oid| {
            base.is_some_and(|base| {
                id == base || self.0.graph_descendant_of(base, id).unwrap_or(false)
            })
        };
        let start = self
            .0
            .revparse_single(rev)
            .and_then(|object| object.peel_to_commit())
            .map_err(error)?;
        if hidden(start.id()) {
            return Ok(Vec::new());
        }
        let mut seen = HashSet::from([start.id()]);
        let mut queue = BinaryHeap::from([(start.time().seconds(), start.id())]);
        let mut commits = Vec::new();
        while commits.len() < limit {
            let Some((_, id)) = queue.pop() else {
                break;
            };
            let commit = self.0.find_commit(id).map_err(error)?;
            for parent in commit
                .parent_ids()
                .filter(|id| seen.insert(*id) && !hidden(*id))
            {
                let time = self.0.find_commit(parent).map_err(error)?.time();
                queue.push((time.seconds(), parent));
            }
            commits.push(commit_of(&commit));
        }
        Ok(commits)
    }

    /// The patch commit `id` introduces against its first parent (every
    /// file added for a root commit), with full blob hashes so
    /// [`crate::content::retrieve`] resolves both sides from the object
    /// database. A GitButler conflicted commit, on either side, is
    /// diffed by its auto-resolved tree, as GitButler shows it.
    pub fn commit_patch(&self, id: &str) -> Result<String, String> {
        let error = |e: git2::Error| format!("failed to diff commit {id}: {e}");
        let oid = Oid::from_str(id).map_err(error)?;
        let commit = self.0.find_commit(oid).map_err(error)?;
        let tree = shown_tree(&self.0, &commit).map_err(error)?;
        let parent = match commit.parent(0) {
            Ok(parent) => Some(shown_tree(&self.0, &parent).map_err(error)?),
            Err(_) => None,
        };
        print_patch(&self.0, parent.as_ref(), &tree).map_err(error)
    }

    /// The patch from revision `base`'s tree to revision `tip`'s: the
    /// net change of every commit between them, in the form
    /// [`Self::commit_patch`] gives, GitButler conflicted commits
    /// included.
    pub fn tree_patch(&self, base: &str, tip: &str) -> Result<String, String> {
        let error = |e: git2::Error| format!("failed to diff {base}..{tip}: {e}");
        let tree = |rev: &str| {
            let object = self.0.revparse_single(rev)?;
            match object.peel_to_commit() {
                Ok(commit) => shown_tree(&self.0, &commit),
                Err(_) => object.peel_to_tree(),
            }
        };
        let old = tree(base).map_err(error)?;
        let new = tree(tip).map_err(error)?;
        print_patch(&self.0, Some(&old), &new).map_err(error)
    }
}

fn commit_of(commit: &git2::Commit<'_>) -> Commit {
    let id = commit.id().to_string();
    let author = commit.author();
    Commit {
        short_id: id[..7].to_string(),
        id,
        summary: commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string(),
        message: commit.message().unwrap_or_default().to_string(),
        author: author.name().unwrap_or_default().to_string(),
        email: author.email().unwrap_or_default().to_string(),
        time: author.when().seconds(),
    }
}

/// The tree `commit` shows: its `.auto-resolution` subtree when
/// GitButler wrote it as a conflicted commit, else its own tree.
///
/// GitButler's rule (`Commit::is_conflicted` in `but-core`): the commit
/// carries conflict metadata, a `GitButler-Conflict` message trailer or a
/// numeric `gitbutler-conflicted` header, and its tree holds synthetic
/// entries next to the files. Those entries are whole copies of the
/// repository, so diffing the raw tree would list every file several
/// times over. Without an `.auto-resolution` entry the raw tree is shown.
fn shown_tree<'r>(
    repo: &'r Repository,
    commit: &git2::Commit<'r>,
) -> Result<git2::Tree<'r>, git2::Error> {
    let tree = commit.tree()?;
    if !has_conflict_metadata(commit) {
        return Ok(tree);
    }
    let resolved = tree.get_name(".auto-resolution").map(|entry| entry.id());
    match resolved {
        Some(id) => repo.find_tree(id),
        None => Ok(tree),
    }
}

fn has_conflict_metadata(commit: &git2::Commit<'_>) -> bool {
    let trailer = git2::message_trailers_bytes(commit.message_bytes()).is_ok_and(|trailers| {
        trailers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(b"GitButler-Conflict"))
    });
    let header = commit
        .header_field_bytes("gitbutler-conflicted")
        .ok()
        .and_then(|value| std::str::from_utf8(&value).ok()?.trim().parse::<u64>().ok())
        .is_some();
    trailer || header
}

fn print_patch(
    repo: &Repository,
    old: Option<&git2::Tree<'_>>,
    new: &git2::Tree<'_>,
) -> Result<String, git2::Error> {
    let mut opts = DiffOptions::new();
    opts.include_typechange(true).id_abbrev(40);
    let mut diff = repo.diff_tree_to_tree(old, Some(new), Some(&mut opts))?;
    let mut find = DiffFindOptions::new();
    find.renames(true);
    diff.find_similar(Some(&mut find))?;
    let mut out = String::new();
    diff.print(DiffFormat::Patch, |_, _, line| {
        // Content lines need their origin prefix; headers already include it.
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(out)
}
