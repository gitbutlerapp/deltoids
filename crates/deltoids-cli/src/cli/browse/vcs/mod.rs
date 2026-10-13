//! Version control for the TUI: the one place that knows a repository
//! can be a plain git repository or a GitButler workspace.
//!
//! [`Vcs`] answers every question the panels ask (the working tree's
//! changes, the branch list, a branch's commits) and hands out a
//! [`Writer`] for stage and discard. It works out which kind of
//! repository it holds on every call, from where `HEAD` points, so it
//! follows `but setup` and `but teardown` while the TUI runs. Callers
//! never learn which kind it is.
//!
//! - Git: staged and unstaged columns, local branches, and writes through
//!   the index and the git CLI ([`git_writes`]).
//! - GitButler: one column of uncommitted changes, GitButler's branches,
//!   and writes through the `but` CLI ([`gitbutler`]). GitButler has no
//!   staging, so staging changes nothing there.
//!
//! Objects (blobs, commits, patches) read the same way in both, through
//! [`Vcs::repo`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use deltoids::git;

mod git_writes;
mod gitbutler;

/// Which kind of repository a call found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Git,
    GitButler,
}

impl Backend {
    fn of(repo: &git::Repo) -> Self {
        if gitbutler::is_workspace(repo.head_ref().as_deref()) {
            Self::GitButler
        } else {
            Self::Git
        }
    }
}

/// How the working tree's changes split into columns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::cli::browse) enum Columns {
    /// Staged (HEAD → index) and unstaged (index → worktree).
    #[default]
    Split,
    /// One column of uncommitted changes (HEAD → worktree).
    Single,
}

/// The working tree's changes, shaped for the repository's kind.
pub(in crate::cli::browse) struct WorkingTree {
    pub(in crate::cli::browse) snapshot: git::WorkingTreeSnapshot,
    pub(in crate::cli::browse) columns: Columns,
}

/// The branches panel: its title and its rows in display order.
pub(in crate::cli::browse) struct BranchList {
    pub(in crate::cli::browse) label: &'static str,
    pub(in crate::cli::browse) branches: Result<Vec<Branch>, String>,
}

/// One row of the branches panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli::browse) struct Branch {
    pub(in crate::cli::browse) name: String,
    /// Checked out (git) or applied (GitButler).
    pub(in crate::cli::browse) current: bool,
    /// Subject line of the tip commit.
    pub(in crate::cli::browse) summary: String,
    /// Time of the tip commit, in seconds since the epoch.
    pub(in crate::cli::browse) time: i64,
    /// The ref [`Vcs::commits`] walks.
    rev: String,
}

/// A repository the TUI reads and writes.
pub(in crate::cli::browse) struct Vcs {
    repo: git::Repo,
    but: OsString,
    /// GitButler's branch list and the refs fingerprint it was read at.
    branch_cache: Option<(u64, Result<Vec<Branch>, String>)>,
}

impl Vcs {
    /// The repository enclosing `path`, if any.
    pub(in crate::cli::browse) fn discover_at(path: &Path) -> Option<Self> {
        git::Repo::discover_at(path).map(Self::new)
    }

    fn new(repo: git::Repo) -> Self {
        Self {
            repo,
            but: OsString::from(gitbutler::BUT),
            branch_cache: None,
        }
    }

    /// Object and path reads, the same in every kind of repository.
    pub(in crate::cli::browse) fn repo(&self) -> &git::Repo {
        &self.repo
    }

    fn backend(&self) -> Backend {
        Backend::of(&self.repo)
    }

    /// The working tree's changes against `HEAD`.
    pub(in crate::cli::browse) fn working_tree(&self) -> Result<WorkingTree, String> {
        let snapshot = self.repo.working_tree_snapshot()?;
        Ok(match self.backend() {
            Backend::Git => WorkingTree {
                snapshot,
                columns: Columns::Split,
            },
            Backend::GitButler => WorkingTree {
                snapshot: gitbutler::uncommitted(snapshot),
                columns: Columns::Single,
            },
        })
    }

    /// The branches panel. GitButler's list costs a `but` run, so it is
    /// read again only after a ref moves.
    pub(in crate::cli::browse) fn branches(&mut self) -> BranchList {
        match self.backend() {
            Backend::Git => BranchList {
                label: "Local branches",
                branches: self.repo.local_branches().map(|branches| {
                    branches
                        .into_iter()
                        .map(|branch| Branch {
                            rev: format!("refs/heads/{}", branch.name),
                            name: branch.name,
                            current: branch.head,
                            summary: branch.summary,
                            time: branch.time,
                        })
                        .collect()
                }),
            },
            Backend::GitButler => {
                let fingerprint = self.repo.refs_fingerprint();
                let fresh = self
                    .branch_cache
                    .as_ref()
                    .is_some_and(|(at, _)| *at == fingerprint);
                if !fresh {
                    let branches = gitbutler::branches(&self.but, &self.repo);
                    self.branch_cache = Some((fingerprint, branches));
                }
                let (_, branches) = self.branch_cache.as_ref().expect("filled above");
                BranchList {
                    label: "Branches",
                    branches: branches.clone(),
                }
            }
        }
    }

    /// Up to `limit` commits of `branch`, newest first.
    pub(in crate::cli::browse) fn commits(
        &self,
        branch: &Branch,
        limit: usize,
    ) -> Result<Vec<git::Commit>, String> {
        self.repo.commits(&branch.rev, limit)
    }

    /// Writes on this repository's working tree, to run on a worker
    /// thread. `None` for a bare repository.
    pub(in crate::cli::browse) fn writer(&self) -> Option<Writer> {
        let workdir = self.repo.workdir()?.to_path_buf();
        Some(Writer {
            workdir,
            but: self.but.clone(),
        })
    }

    /// The discard menu's rows before a selection is read: this kind of
    /// repository's choices, none available yet.
    pub(in crate::cli::browse) fn discard_placeholders(vcs: Option<&Self>) -> Vec<DiscardChoice> {
        let backend = vcs.map_or(Backend::Git, Self::backend);
        DiscardKind::offered(backend)
            .iter()
            .map(|kind| kind.choice(backend, false))
            .collect()
    }
}

/// What a discard throws away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::browse) enum DiscardKind {
    /// Everything since `HEAD`, staged changes included.
    All,
    /// Worktree changes only; staged changes stay.
    Unstaged,
}

impl DiscardKind {
    fn offered(backend: Backend) -> &'static [Self] {
        match backend {
            Backend::Git => &[Self::All, Self::Unstaged],
            Backend::GitButler => &[Self::All],
        }
    }

    fn choice(self, backend: Backend, available: bool) -> DiscardChoice {
        let (label, description) = match (backend, self) {
            (Backend::Git, Self::All) => (
                "Discard all changes",
                "Restore HEAD, including staged changes. Delete added files.",
            ),
            (Backend::Git, Self::Unstaged) => (
                "Discard unstaged changes",
                "Restore the index. Keep staged changes. Delete untracked files.",
            ),
            (Backend::GitButler, _) => (
                "Discard all changes",
                "Discard through GitButler. `but undo` brings them back.",
            ),
        };
        DiscardChoice {
            kind: self,
            label,
            description,
            available,
        }
    }
}

/// One row of the discard menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cli::browse) struct DiscardChoice {
    pub(in crate::cli::browse) kind: DiscardKind,
    pub(in crate::cli::browse) label: &'static str,
    pub(in crate::cli::browse) description: &'static str,
    pub(in crate::cli::browse) available: bool,
}

/// A read selection waiting for the user to pick a discard.
#[derive(Debug)]
pub(in crate::cli::browse) struct PendingDiscard {
    backend: Backend,
    targets: Vec<PathBuf>,
    state: git_writes::State,
    choices: Vec<DiscardChoice>,
}

impl PendingDiscard {
    /// The menu's subject: the one file, or how many files.
    pub(in crate::cli::browse) fn label(&self) -> String {
        self.state.label()
    }

    pub(in crate::cli::browse) fn choices(&self) -> &[DiscardChoice] {
        &self.choices
    }
}

/// Stage and discard on one working tree. Each call works out the kind
/// of repository again, so a `Writer` stays correct across `but setup`.
#[derive(Debug, Clone)]
pub(in crate::cli::browse) struct Writer {
    workdir: PathBuf,
    but: OsString,
}

impl Writer {
    #[cfg(test)]
    fn at(workdir: &Path) -> Self {
        Self {
            workdir: workdir.to_path_buf(),
            but: OsString::from(gitbutler::BUT),
        }
    }

    fn backend(&self) -> Result<Backend, String> {
        git::Repo::discover_at(&self.workdir)
            .map(|repo| Backend::of(&repo))
            .ok_or_else(|| "Not a git repository".to_string())
    }

    /// Stage the selection, or unstage it when nothing in it is unstaged.
    /// Returns the status-line message.
    pub(in crate::cli::browse) fn toggle_stage(
        &self,
        targets: Vec<PathBuf>,
    ) -> Result<String, String> {
        match self.backend()? {
            Backend::Git => git_writes::toggle_stage(&self.workdir, targets),
            Backend::GitButler => Ok(NO_STAGING.into()),
        }
    }

    /// [`Self::toggle_stage`] over every changed file.
    pub(in crate::cli::browse) fn toggle_stage_all(&self) -> Result<String, String> {
        match self.backend()? {
            Backend::Git => git_writes::toggle_stage_all(&self.workdir),
            Backend::GitButler => Ok(NO_STAGING.into()),
        }
    }

    /// Read the selection and offer the discards this repository allows.
    /// Writes nothing.
    pub(in crate::cli::browse) fn discard(
        &self,
        targets: Vec<PathBuf>,
    ) -> Result<PendingDiscard, String> {
        let backend = self.backend()?;
        let state = git_writes::read_selection(&self.workdir, &targets)?;
        let choices = DiscardKind::offered(backend)
            .iter()
            .map(|&kind| {
                let available = match kind {
                    DiscardKind::All => !state.is_empty(),
                    DiscardKind::Unstaged => state.has_unstaged(),
                };
                kind.choice(backend, available)
            })
            .collect();
        Ok(PendingDiscard {
            backend,
            targets,
            state,
            choices,
        })
    }

    /// Apply `kind` to a pending selection, unless the selection or the
    /// kind of repository changed since it was read. Returns the
    /// status-line message.
    pub(in crate::cli::browse) fn complete_discard(
        &self,
        pending: PendingDiscard,
        kind: DiscardKind,
    ) -> Result<String, String> {
        if !pending
            .choices
            .iter()
            .any(|choice| choice.kind == kind && choice.available)
        {
            return Err("This discard action is unavailable".into());
        }
        if self.backend()? != pending.backend {
            return Err("The repository changed; press d again".into());
        }
        if git_writes::read_selection(&self.workdir, &pending.targets)? != pending.state {
            return Err("Selection changed; press d again to review the current changes".into());
        }
        match pending.backend {
            Backend::Git => {
                git_writes::discard(&self.workdir, &pending.state, kind)?;
                Ok(match kind {
                    DiscardKind::All => "Discarded all selected changes",
                    DiscardKind::Unstaged => "Discarded unstaged changes",
                }
                .into())
            }
            Backend::GitButler => {
                gitbutler::discard(&self.but, &self.workdir, &pending.state.change_paths())?;
                Ok("Discarded with GitButler (but undo brings them back)".into())
            }
        }
    }
}

const NO_STAGING: &str = "GitButler has no staging; changes stay uncommitted";

#[cfg(test)]
mod tests;
