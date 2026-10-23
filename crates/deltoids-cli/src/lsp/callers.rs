//! Who uses a symbol: one lookup that answers for any symbol, the way a
//! reader means it.
//!
//! A function's callers come from the call hierarchy, which names the
//! calling function. A language server answers it only for functions,
//! and from an impl method only with the calls that resolve to that
//! impl. So the lookup also asks for the symbol's references, and, when
//! the function implements a trait's method (its declaration lies
//! elsewhere), for the trait method's calls and references: calls through
//! the trait, as in generic code, reach the impl too. Every location only
//! those wider questions found is checked with `definition`, and stays
//! when it goes to the function or to its trait method. That drops calls
//! to other impls and the definitions of the impls themselves.
//!
//! A symbol that is no function (a constant, a type, a field) has no
//! call hierarchy item; its references are the answer.
//!
//! [`Lookup`] is a state machine: it says which requests to send and
//! takes their responses, and knows nothing of request ids or the
//! transport.

use std::path::PathBuf;

use lsp_types::{CallHierarchyIncomingCall, GotoDefinitionResponse, Location, Position};
use serde_json::{Value, json};

use super::position;
use super::server::{Hit, definition_hits};

/// What the server advertised that the lookup uses.
#[derive(Clone, Copy)]
pub(super) struct Support {
    pub(super) call_hierarchy: bool,
    pub(super) declaration: bool,
}

/// A request the lookup wants sent; its response goes back to
/// [`Lookup::respond`] with `step`.
pub(super) struct Outgoing {
    pub(super) method: &'static str,
    pub(super) params: Value,
    pub(super) step: Step,
}

/// Which question a response answers.
#[derive(Clone, Copy)]
pub(super) struct Step(Kind);

#[derive(Clone, Copy)]
enum Kind {
    /// References of the symbol at the cursor.
    References,
    /// The call hierarchy item at the cursor.
    Prepare,
    /// Where the symbol at the cursor is declared.
    Declared,
    /// The calls to the item at the cursor.
    Incoming,
    /// The call hierarchy item of the trait's method.
    TraitPrepare,
    /// The calls to the trait's method.
    TraitIncoming,
    /// References of the trait's method.
    TraitReferences,
    /// Where candidate `n` goes.
    Target(usize),
}

pub(super) enum Progress {
    /// Send these, then wait.
    Ask(Vec<Outgoing>),
    /// The answer: every use, sorted by path and line.
    Done(Result<Vec<Hit>, String>),
}

/// A file and 1-based line, to tell whether two locations are one place.
type Place = (PathBuf, usize);

/// A location the server gave: where to ask about it again, and what
/// to show for it.
struct Use {
    hit: Hit,
    uri: String,
    position: Position,
}

impl Use {
    fn place(&self) -> Place {
        (self.hit.path.clone(), self.hit.line)
    }

    fn same_spot(&self, other: &Use) -> bool {
        self.uri == other.uri && self.position == other.position
    }

    fn params(&self) -> Value {
        json!({
            "textDocument": { "uri": self.uri },
            "position": self.position
        })
    }
}

/// A use found only through the wider questions, until its target is
/// known.
struct Candidate {
    found: Use,
    kept: bool,
}

/// One answer per step, kept until the lookup moves on.
#[derive(Default)]
struct Opening {
    references: Option<Vec<Use>>,
    item: Option<Option<Value>>,
    declared: Option<Option<Use>>,
}

#[derive(Default)]
struct Gathering {
    /// Calls to the item at the cursor: trusted as they are.
    calls: Vec<Use>,
    /// Uses to check, in the order found.
    wider: Vec<Use>,
}

enum State {
    Opening(Opening),
    Gathering(Gathering),
    Checking {
        calls: Vec<Use>,
        candidates: Vec<Candidate>,
    },
}

/// One callers lookup, from the cursor to the answer.
pub(super) struct Lookup {
    state: State,
    /// Where the function and its trait's method are: uses that go there
    /// stay; uses that are there are their definitions.
    keep: Vec<Place>,
    /// Requests sent and not yet answered.
    outstanding: usize,
    sent: usize,
    failed: usize,
    error: Option<String>,
}

impl Lookup {
    /// Start a lookup at `cursor` (`textDocument` and `position`
    /// params, in the server's encoding).
    pub(super) fn start(support: Support, cursor: Value) -> (Self, Vec<Outgoing>) {
        let mut asks = vec![ask(
            "textDocument/references",
            references_at(&cursor),
            Kind::References,
        )];
        let mut opening = Opening::default();
        if support.call_hierarchy {
            asks.push(ask(
                "textDocument/prepareCallHierarchy",
                cursor.clone(),
                Kind::Prepare,
            ));
        } else {
            opening.item = Some(None);
        }
        if support.call_hierarchy && support.declaration {
            asks.push(ask("textDocument/declaration", cursor, Kind::Declared));
        } else {
            opening.declared = Some(None);
        }
        let lookup = Lookup {
            state: State::Opening(opening),
            keep: Vec::new(),
            outstanding: asks.len(),
            sent: asks.len(),
            failed: 0,
            error: None,
        };
        (lookup, asks)
    }

    /// Take the response to `step`: a result, or the server's error.
    pub(super) fn respond(&mut self, step: Step, result: Result<Value, String>) -> Progress {
        self.outstanding = self.outstanding.saturating_sub(1);
        let value = match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.failed += 1;
                self.error.get_or_insert(error);
                None
            }
        };
        let mut asks = Vec::new();
        match (&mut self.state, step.0) {
            (State::Opening(opening), Kind::References) => {
                opening.references = Some(value.map_or_else(Vec::new, locations));
            }
            (State::Opening(opening), Kind::Prepare) => {
                opening.item = Some(value.and_then(first_item));
            }
            (State::Opening(opening), Kind::Declared) => {
                opening.declared = Some(value.and_then(first_location));
            }
            (State::Gathering(gathering), Kind::Incoming) => {
                gathering.calls = value.map_or_else(Vec::new, incoming_calls);
            }
            (State::Gathering(_), Kind::TraitPrepare) => {
                if let Some(item) = value.and_then(first_item) {
                    asks.push(ask(
                        "callHierarchy/incomingCalls",
                        json!({ "item": item }),
                        Kind::TraitIncoming,
                    ));
                }
            }
            (State::Gathering(gathering), Kind::TraitIncoming) => {
                gathering
                    .wider
                    .extend(value.map_or_else(Vec::new, incoming_calls));
            }
            (State::Gathering(gathering), Kind::TraitReferences) => {
                gathering
                    .wider
                    .extend(value.map_or_else(Vec::new, locations));
            }
            (State::Checking { candidates, .. }, Kind::Target(index)) => {
                let targets = value.map(definition_hits).unwrap_or_default();
                let kept = targets.is_empty()
                    || targets
                        .iter()
                        .any(|target| self.keep.contains(&(target.path.clone(), target.line)));
                if let Some(candidate) = candidates.get_mut(index) {
                    candidate.kept = kept;
                }
            }
            _ => {}
        }
        if !asks.is_empty() {
            return self.ask(asks);
        }
        if self.outstanding > 0 {
            return Progress::Ask(Vec::new());
        }
        self.advance()
    }

    /// Every response of the current state is in: move on.
    fn advance(&mut self) -> Progress {
        match std::mem::replace(&mut self.state, State::Opening(Opening::default())) {
            State::Opening(opening) => self.opened(opening),
            State::Gathering(gathering) => self.gathered(gathering),
            State::Checking { calls, candidates } => {
                let kept = candidates
                    .into_iter()
                    .filter(|candidate| candidate.kept)
                    .map(|candidate| candidate.found);
                self.done(calls.into_iter().chain(kept).collect())
            }
        }
    }

    fn opened(&mut self, opening: Opening) -> Progress {
        let references = opening.references.unwrap_or_default();
        let Some(Some(item)) = opening.item else {
            return self.done(references);
        };
        let Some(own) = item_place(&item) else {
            return self.done(references);
        };
        self.keep.push(own.clone());
        let mut asks = vec![ask(
            "callHierarchy/incomingCalls",
            json!({ "item": item }),
            Kind::Incoming,
        )];
        if let Some(Some(declared)) = opening.declared
            && declared.place() != own
        {
            self.keep.push(declared.place());
            let at = declared.params();
            asks.push(ask(
                "textDocument/prepareCallHierarchy",
                at.clone(),
                Kind::TraitPrepare,
            ));
            asks.push(ask(
                "textDocument/references",
                references_at(&at),
                Kind::TraitReferences,
            ));
        }
        self.state = State::Gathering(Gathering {
            calls: Vec::new(),
            wider: references,
        });
        self.ask(asks)
    }

    fn gathered(&mut self, gathering: Gathering) -> Progress {
        let Gathering { calls, wider } = gathering;
        let mut candidates: Vec<Candidate> = Vec::new();
        for found in wider {
            if calls.iter().any(|call| call.same_spot(&found)) || self.keep.contains(&found.place())
            {
                continue;
            }
            match candidates.iter_mut().find(|c| c.found.same_spot(&found)) {
                Some(known) => {
                    known.found.hit.caller = known.found.hit.caller.take().or(found.hit.caller);
                }
                None => candidates.push(Candidate { found, kept: false }),
            }
        }
        if candidates.is_empty() {
            return self.done(calls);
        }
        let asks = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                ask(
                    "textDocument/definition",
                    candidate.found.params(),
                    Kind::Target(index),
                )
            })
            .collect();
        self.state = State::Checking { calls, candidates };
        self.ask(asks)
    }

    fn ask(&mut self, asks: Vec<Outgoing>) -> Progress {
        self.outstanding += asks.len();
        self.sent += asks.len();
        Progress::Ask(asks)
    }

    fn done(&self, uses: Vec<Use>) -> Progress {
        if uses.is_empty()
            && self.failed == self.sent
            && let Some(error) = &self.error
        {
            return Progress::Done(Err(error.clone()));
        }
        let mut hits: Vec<Hit> = Vec::new();
        for found in uses {
            let hit = found.hit;
            match hits
                .iter_mut()
                .find(|seen| seen.path == hit.path && seen.line == hit.line)
            {
                Some(seen) => seen.caller = seen.caller.take().or(hit.caller),
                None => hits.push(hit),
            }
        }
        hits.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Progress::Done(Ok(hits))
    }
}

fn ask(method: &'static str, params: Value, kind: Kind) -> Outgoing {
    Outgoing {
        method,
        params,
        step: Step(kind),
    }
}

fn references_at(at: &Value) -> Value {
    let mut params = at.clone();
    params["context"] = json!({ "includeDeclaration": false });
    params
}

fn first_item(value: Value) -> Option<Value> {
    value.as_array()?.first().cloned()
}

/// Where a call hierarchy item's name is.
fn item_place(item: &Value) -> Option<Place> {
    let path = position::path(item["uri"].as_str()?)?;
    let line = item["selectionRange"]["start"]["line"].as_u64()?;
    Some((path, line as usize + 1))
}

fn found(uri: &str, position: Position, caller: Option<String>) -> Option<Use> {
    Some(Use {
        hit: Hit {
            path: position::path(uri)?,
            line: position.line as usize + 1,
            caller,
        },
        uri: uri.to_string(),
        position,
    })
}

fn locations(value: Value) -> Vec<Use> {
    serde_json::from_value::<Option<Vec<Location>>>(value)
        .ok()
        .flatten()
        .unwrap_or_default()
        .iter()
        .filter_map(|location| found(location.uri.as_str(), location.range.start, None))
        .collect()
}

/// Each call site, named by the function it sits in.
fn incoming_calls(value: Value) -> Vec<Use> {
    serde_json::from_value::<Option<Vec<CallHierarchyIncomingCall>>>(value)
        .ok()
        .flatten()
        .unwrap_or_default()
        .iter()
        .flat_map(|call| {
            call.from_ranges.iter().filter_map(|range| {
                found(
                    call.from.uri.as_str(),
                    range.start,
                    Some(call.from.name.clone()),
                )
            })
        })
        .collect()
}

/// The first location of a definition-shaped answer.
fn first_location(value: Value) -> Option<Use> {
    match serde_json::from_value::<Option<GotoDefinitionResponse>>(value).ok()?? {
        GotoDefinitionResponse::Scalar(location) => {
            found(location.uri.as_str(), location.range.start, None)
        }
        GotoDefinitionResponse::Array(locations) => {
            let location = locations.first()?;
            found(location.uri.as_str(), location.range.start, None)
        }
        GotoDefinitionResponse::Link(links) => {
            let link = links.first()?;
            found(
                link.target_uri.as_str(),
                link.target_selection_range.start,
                None,
            )
        }
    }
}
