//! Files the servers watch. A server registers glob patterns through
//! `workspace/didChangeWatchedFiles`; the workdir watcher feeds every
//! change into [`FileEvents`], which keeps the paths some server
//! watches until the next pump hands each server its own.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, RegistrationParams,
    UnregistrationParams, WatchKind,
};
use notify::EventKind;
use notify::event::{AccessKind, AccessMode};
use serde_json::{Value, json};

use super::position;

/// The method servers register to hear about file changes.
pub(super) const METHOD: &str = "workspace/didChangeWatchedFiles";

/// Changes to the files the servers watch, fed by the workdir watcher
/// from its own thread. Clones share one queue.
#[derive(Clone)]
pub(crate) struct FileEvents(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    /// Each watching server's patterns, shared with its
    /// [`Registrations`]; other paths are dropped on arrival, so
    /// `changes` never outgrows the files the servers watch. It may
    /// still hold the patterns of a server that stopped: what they match
    /// reaches no server and is gone at the next pump.
    filter: Vec<Arc<GlobSet>>,
    /// Each changed path, and whether the watcher saw it created.
    changes: HashMap<PathBuf, bool>,
    lost: bool,
}

impl FileEvents {
    pub(super) fn new() -> Self {
        Self(Arc::default())
    }

    /// Keep the event's paths that some server watches. Reads are not
    /// changes; only a write that closes a file is.
    pub(crate) fn record(&self, event: &notify::Event) {
        if event.need_rescan() {
            self.lost();
            return;
        }
        let created = match event.kind {
            EventKind::Create(_) => true,
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
            EventKind::Access(_) => return,
            _ => false,
        };
        let mut pending = self.lock();
        for path in &event.paths {
            if pending.filter.iter().any(|set| set.is_match(path)) {
                *pending.changes.entry(path.clone()).or_default() |= created;
            }
        }
    }

    /// Changes went missing: the servers that watch files must read the
    /// disk again.
    pub(crate) fn lost(&self) {
        self.lock().lost = true;
    }

    pub(super) fn take_lost(&self) -> bool {
        std::mem::take(&mut self.lock().lost)
    }

    /// Keep the paths one of `sets` matches from now on.
    pub(super) fn watch(&self, sets: Vec<Arc<GlobSet>>) {
        self.lock().filter = sets;
    }

    /// The changes since the last call, typed by what is on disk now.
    pub(super) fn take(&self) -> Vec<Change> {
        let changes = std::mem::take(&mut self.lock().changes);
        let mut changes: Vec<Change> = changes
            .into_iter()
            .filter_map(|(path, created)| Change::on_disk(path, created))
            .collect();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.0.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// One file that changed, typed as the protocol types it.
pub(super) struct Change {
    path: PathBuf,
    kind: ChangeKind,
}

#[derive(Clone, Copy)]
enum ChangeKind {
    Created,
    Changed,
    Deleted,
}

impl Change {
    /// `path` as the disk has it now; `None` for a directory.
    fn on_disk(path: PathBuf, created: bool) -> Option<Self> {
        let kind = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => return None,
            Ok(_) if created => ChangeKind::Created,
            Ok(_) => ChangeKind::Changed,
            Err(err) if err.kind() == io::ErrorKind::NotFound => ChangeKind::Deleted,
            Err(_) => ChangeKind::Changed,
        };
        Some(Self { path, kind })
    }
}

impl ChangeKind {
    /// The protocol's `FileChangeType`.
    fn code(self) -> u8 {
        match self {
            ChangeKind::Created => 1,
            ChangeKind::Changed => 2,
            ChangeKind::Deleted => 3,
        }
    }

    /// The bit a watcher sets to hear about this kind of change.
    fn watch_kind(self) -> WatchKind {
        match self {
            ChangeKind::Created => WatchKind::Create,
            ChangeKind::Changed => WatchKind::Change,
            ChangeKind::Deleted => WatchKind::Delete,
        }
    }
}

/// The patterns one server watches, by registration id, compiled once:
/// a large server registers hundreds of patterns, and each compiled copy
/// costs megabytes. Patterns are grouped by the kinds of change they
/// hear, so a match never needs to know which pattern matched; servers
/// usually hear every kind, which makes one set.
#[derive(Default)]
pub(super) struct Registrations {
    by_id: Vec<(String, Vec<Pattern>)>,
    /// Every valid pattern, one set per kinds of change; shared with
    /// [`FileEvents`] as its filter.
    sets: Vec<(WatchKind, Arc<GlobSet>)>,
}

struct Pattern {
    glob: String,
    kind: WatchKind,
}

impl Registrations {
    /// Take in `client/registerCapability` params. A registration
    /// replaces the one with its id. Returns whether the watched files
    /// changed.
    pub(super) fn register(&mut self, params: &Value) -> bool {
        let Ok(params) = serde_json::from_value::<RegistrationParams>(params.clone()) else {
            return false;
        };
        let mut changed = false;
        for registration in params.registrations {
            if registration.method != METHOD {
                continue;
            }
            let watchers = registration
                .register_options
                .and_then(|options| {
                    serde_json::from_value::<DidChangeWatchedFilesRegistrationOptions>(options).ok()
                })
                .map_or_else(Vec::new, |options| {
                    options.watchers.iter().filter_map(Pattern::new).collect()
                });
            self.by_id.retain(|(id, _)| *id != registration.id);
            self.by_id.push((registration.id, watchers));
            changed = true;
        }
        if changed {
            self.compile();
        }
        changed
    }

    /// Take in `client/unregisterCapability` params. Returns whether
    /// the watched files changed.
    pub(super) fn unregister(&mut self, params: &Value) -> bool {
        let Ok(params) = serde_json::from_value::<UnregistrationParams>(params.clone()) else {
            return false;
        };
        let before = self.by_id.len();
        for gone in params.unregisterations {
            if gone.method == METHOD {
                self.by_id.retain(|(id, _)| *id != gone.id);
            }
        }
        let changed = self.by_id.len() != before;
        if changed {
            self.compile();
        }
        changed
    }

    /// Forget every registration. Returns whether there were any.
    pub(super) fn clear(&mut self) -> bool {
        let had = !self.by_id.is_empty();
        *self = Self::default();
        had
    }

    /// Whether the server watches any file.
    pub(super) fn any(&self) -> bool {
        !self.sets.is_empty()
    }

    /// The compiled patterns, for [`FileEvents::watch`].
    pub(super) fn sets(&self) -> impl Iterator<Item = Arc<GlobSet>> + '_ {
        self.sets.iter().map(|(_, set)| Arc::clone(set))
    }

    /// The `didChangeWatchedFiles` params for the changes this server
    /// watches, if it watches any of them.
    pub(super) fn notification(&self, changes: &[Change]) -> Option<Value> {
        let changes: Vec<Value> = changes
            .iter()
            .filter(|change| self.hears(change))
            .filter_map(|change| {
                Some(json!({ "uri": position::uri(&change.path)?, "type": change.kind.code() }))
            })
            .collect();
        (!changes.is_empty()).then(|| json!({ "changes": changes }))
    }

    fn hears(&self, change: &Change) -> bool {
        let wanted = change.kind.watch_kind();
        self.sets
            .iter()
            .any(|(kind, set)| kind.contains(wanted) && set.is_match(&change.path))
    }

    /// Compile every registered pattern, one set per kinds of change.
    /// Invalid patterns match nothing. Patterns too many for one set
    /// match every path, which the server can ignore.
    fn compile(&mut self) {
        let mut groups: Vec<(WatchKind, GlobSetBuilder)> = Vec::new();
        for pattern in self.by_id.iter().flat_map(|(_, patterns)| patterns) {
            let Ok(glob) = GlobBuilder::new(&pattern.glob)
                .literal_separator(true)
                .build()
            else {
                continue;
            };
            let at = match groups.iter().position(|(kind, _)| *kind == pattern.kind) {
                Some(at) => at,
                None => {
                    groups.push((pattern.kind, GlobSetBuilder::new()));
                    groups.len() - 1
                }
            };
            groups[at].1.add(glob);
        }
        self.sets = groups
            .into_iter()
            .map(|(kind, builder)| {
                (
                    kind,
                    Arc::new(builder.build().unwrap_or_else(|_| everything())),
                )
            })
            .collect();
    }
}

fn everything() -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    builder.add(GlobBuilder::new("**").build().expect("a valid glob"));
    builder.build().expect("one glob fits")
}

impl Pattern {
    /// A pattern for an absolute glob string. Relative patterns are not
    /// advertised, so a server that sends one anyway is not heard.
    fn new(watcher: &FileSystemWatcher) -> Option<Self> {
        let GlobPattern::String(glob) = &watcher.glob_pattern else {
            return None;
        };
        Some(Self {
            glob: glob.clone(),
            kind: watcher.kind.unwrap_or(WatchKind::all()),
        })
    }
}
