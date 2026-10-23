//! One running language server: the handshake, the queue that holds
//! messages until it completes, document versions, requests in flight,
//! progress, and shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::TryRecvError;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{CallHierarchyIncomingCall, GotoDefinitionResponse, Location, Position};
use serde_json::{Value, json};

use super::config::{ServerSettings, ServerSpec};
use super::position::{self, Encoding};
use super::transport::Link;
use super::watched_files::{self, Change, Registrations};
use super::{Phase, Query};

/// How long quitting waits for a server to shut down before killing it.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// A result location, before its path is made relative to the workdir.
#[derive(Clone)]
pub(super) struct Hit {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) caller: Option<String>,
}

pub(super) type Answer = Result<Vec<Hit>, String>;

/// A query, with what it needs to place its position once the server
/// has said how it counts columns.
pub(super) struct Ask {
    pub(super) ticket: u64,
    pub(super) query: Query,
    pub(super) uri: String,
    /// 0-based.
    pub(super) line: usize,
    pub(super) line_text: String,
    pub(super) byte: usize,
}

enum Outgoing {
    Message(Message),
    Ask(Ask),
}

/// What a request in flight answers.
#[derive(Clone, Copy)]
enum Stage {
    Definition,
    Prepare,
    Incoming,
    References,
    /// Where the function at the cursor is declared: its trait's method,
    /// when it implements one.
    Declared,
    /// The call hierarchy item at the cursor, beside [`Stage::Declared`].
    Own,
    /// The call hierarchy item of the trait's method.
    TraitPrepare,
    /// The calls to the trait's method.
    TraitIncoming,
    /// Where candidate call `n` goes.
    Target(usize),
}

impl Stage {
    fn looked_for(self) -> &'static str {
        match self {
            Stage::Definition => "definition",
            _ => "callers",
        }
    }
}

/// A file and 1-based line, to tell whether two locations are one place.
type Place = (PathBuf, usize);

/// A location the server gave, to ask about again.
struct Spot {
    uri: String,
    position: Position,
    place: Place,
}

/// A call to the trait's method, until its target is known.
struct Candidate {
    hit: Hit,
    spot: Spot,
    kept: bool,
}

/// A callers query that may widen from an impl method to its trait's
/// method: calls through the trait, as in generic code, reach the impl
/// too. Calls that go to another impl are sifted out.
enum Widening {
    /// Waiting for the declaration and the item at the cursor; `None`
    /// until each answers.
    Joining {
        declared: Option<Option<Spot>>,
        own: Option<Option<Value>>,
    },
    /// Asking for the trait's callers; calls going to `keep` stay.
    Tracing { keep: [Place; 2] },
    /// Waiting for where `outstanding` of the calls go.
    Sifting {
        keep: [Place; 2],
        calls: Vec<Candidate>,
        outstanding: usize,
    },
}

struct Waiting {
    ticket: u64,
    stage: Stage,
}

struct Document {
    version: i32,
    text: String,
}

/// One work-done progress the server reported and has not ended.
struct Work {
    token: String,
    percent: Option<u8>,
}

pub(super) struct Server {
    name: String,
    link: Link,
    folders: Value,
    settings: Option<ServerSettings>,
    next_id: i32,
    /// The `initialize` request, until the server answers it.
    initialize: Option<RequestId>,
    encoding: Encoding,
    call_hierarchy: bool,
    declaration: bool,
    queue: Vec<Outgoing>,
    waiting: HashMap<RequestId, Waiting>,
    widening: HashMap<u64, Widening>,
    work: Vec<Work>,
    documents: HashMap<String, Document>,
    /// The files the server asked to hear about.
    watched: Registrations,
    /// `watched` changed since [`Server::take_watched_changed`].
    watched_changed: bool,
    stopped: bool,
}

impl Server {
    /// Start the handshake with `spec`'s server on `link` for the
    /// workspace at `root`. Messages wait in a queue until the server
    /// answers.
    pub(super) fn start(spec: &ServerSpec, link: Link, root: &Path) -> Self {
        let root_uri = position::uri(root).unwrap_or_default();
        let folder_name = root
            .file_name()
            .map_or(String::new(), |name| name.to_string_lossy().into_owned());
        let folders = json!([{ "uri": root_uri, "name": folder_name }]);
        let mut server = Server {
            name: spec.name(),
            link,
            folders: folders.clone(),
            settings: spec.settings.clone(),
            next_id: 1,
            initialize: None,
            encoding: Encoding::Utf16,
            call_hierarchy: false,
            declaration: false,
            queue: Vec::new(),
            waiting: HashMap::new(),
            widening: HashMap::new(),
            work: Vec::new(),
            documents: HashMap::new(),
            watched: Registrations::default(),
            watched_changed: false,
            stopped: false,
        };
        let id = server.id();
        let mut params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "deltoids", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": folders,
            "capabilities": {
                "general": { "positionEncodings": ["utf-8", "utf-16"] },
                "window": { "workDoneProgress": true },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "didChangeWatchedFiles": {
                        "dynamicRegistration": true,
                        "relativePatternSupport": false
                    }
                },
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false },
                    "definition": { "linkSupport": true },
                    "declaration": { "linkSupport": true },
                    "references": {},
                    "callHierarchy": {}
                }
            }
        });
        if let Some(settings) = &server.settings {
            params["initializationOptions"] = settings.options();
        }
        server.write(Message::Request(Request::new(
            id.clone(),
            "initialize".to_string(),
            params,
        )));
        server.initialize = Some(id);
        server
    }

    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    /// The files the server asked to hear about.
    pub(super) fn watched(&self) -> &Registrations {
        &self.watched
    }

    /// Whether the files the server watches changed since the last call.
    pub(super) fn take_watched_changed(&mut self) -> bool {
        std::mem::take(&mut self.watched_changed)
    }

    /// Tell the server about the `changes` it watches.
    pub(super) fn files_changed(&mut self, changes: &[Change]) {
        if self.stopped {
            return;
        }
        if let Some(params) = self.watched.notification(changes) {
            self.send(Outgoing::Message(notification(
                watched_files::METHOD,
                params,
            )));
        }
    }

    /// Give up on every query in flight, answering each with `reason`.
    pub(super) fn abandon(&mut self, reason: &str) -> Vec<(u64, Answer)> {
        let mut answers = Vec::new();
        self.fail_with(reason, &mut answers);
        answers
    }

    /// From now on the server is stopped and watches nothing.
    fn stop(&mut self) {
        self.stopped = true;
        self.watched_changed |= self.watched.clear();
    }

    /// Open `uri` with `text`, or send the new text when it changed.
    pub(super) fn open(&mut self, uri: &str, language_id: &str, text: &str) {
        if self.documents.contains_key(uri) {
            self.sync(uri, text);
            return;
        }
        self.documents.insert(
            uri.to_string(),
            Document {
                version: 1,
                text: text.to_string(),
            },
        );
        self.send(Outgoing::Message(notification(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": language_id, "version": 1, "text": text
            }}),
        )));
    }

    /// Send `text` for an open `uri` when it differs from what the
    /// server has. Unopened documents are left to the server's own view
    /// of the disk.
    pub(super) fn sync(&mut self, uri: &str, text: &str) {
        let Some(document) = self.documents.get_mut(uri) else {
            return;
        };
        if document.text == text {
            return;
        }
        document.version += 1;
        document.text = text.to_string();
        let version = document.version;
        self.send(Outgoing::Message(notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }]
            }),
        )));
    }

    pub(super) fn ask(&mut self, ask: Ask) {
        self.send(Outgoing::Ask(ask));
    }

    /// Drop `ticket`: unsent, or cancelled at the server.
    pub(super) fn cancel(&mut self, ticket: u64) {
        self.queue
            .retain(|outgoing| !matches!(outgoing, Outgoing::Ask(ask) if ask.ticket == ticket));
        let ids: Vec<RequestId> = self
            .waiting
            .iter()
            .filter(|(_, waiting)| waiting.ticket == ticket)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.waiting.remove(&id);
            self.write(notification("$/cancelRequest", json!({ "id": id })));
        }
        self.widening.remove(&ticket);
    }

    /// What the server is busy with, if anything.
    pub(super) fn phase(&self) -> Option<Phase> {
        if self.stopped {
            return None;
        }
        if self.initialize.is_some() {
            return Some(Phase::Starting);
        }
        self.work.last().map(|work| Phase::Working {
            percent: work.percent,
        })
    }

    /// Handle everything the server sent since the last call, returning
    /// the tickets it answered.
    pub(super) fn pump(&mut self) -> Vec<(u64, Answer)> {
        let mut answers = Vec::new();
        loop {
            match self.link.connection.receiver.try_recv() {
                Ok(message) => self.receive(message, &mut answers),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.stop();
                    break;
                }
            }
        }
        if self.stopped {
            self.fail_all(&mut answers);
        }
        answers
    }

    fn fail_all(&mut self, answers: &mut Vec<(u64, Answer)>) {
        if self.waiting.is_empty() && self.queue.is_empty() {
            return;
        }
        let message = match self.link.last_error() {
            Some(reason) => format!("{} stopped: {reason}", self.name),
            None => format!("{} stopped; it restarts on the next request", self.name),
        };
        self.fail_with(&message, answers);
    }

    fn fail_with(&mut self, message: &str, answers: &mut Vec<(u64, Answer)>) {
        let mut tickets: Vec<u64> = self.waiting.drain().map(|(_, w)| w.ticket).collect();
        tickets.extend(self.queue.drain(..).filter_map(|outgoing| match outgoing {
            Outgoing::Ask(ask) => Some(ask.ticket),
            Outgoing::Message(_) => None,
        }));
        tickets.sort_unstable();
        tickets.dedup();
        self.widening.clear();
        for ticket in tickets {
            answers.push((ticket, Err(message.to_string())));
        }
    }

    fn receive(&mut self, message: Message, answers: &mut Vec<(u64, Answer)>) {
        match message {
            Message::Response(response) if self.initialize.as_ref() == Some(&response.id) => {
                self.initialized(response);
            }
            Message::Response(response) => {
                let Some(waiting) = self.waiting.remove(&response.id) else {
                    return;
                };
                match (response.response_result, waiting.stage) {
                    (Ok(value), _) => self.answered(waiting, value, answers),
                    (Err(_), Stage::Declared) => {
                        self.join(waiting.ticket, Some(None), None, answers);
                    }
                    (Err(_), Stage::Target(index)) => {
                        self.targeted(waiting.ticket, index, true, answers);
                    }
                    (Err(error), _) => {
                        self.forget(waiting.ticket);
                        answers.push((
                            waiting.ticket,
                            Err(format!("{}: {}", self.name, error.message)),
                        ));
                    }
                }
            }
            Message::Request(request) => self.reply(request),
            Message::Notification(note) if note.method == "$/progress" => {
                self.progressed(&note.params);
            }
            Message::Notification(_) => {}
        }
    }

    fn initialized(&mut self, response: Response) {
        self.initialize = None;
        let Ok(result) = response.response_result else {
            self.stop();
            return;
        };
        let capabilities = &result["capabilities"];
        self.encoding = match capabilities["positionEncoding"].as_str() {
            Some("utf-8") => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        self.call_hierarchy = matches!(
            capabilities["callHierarchyProvider"],
            Value::Bool(true) | Value::Object(_)
        );
        self.declaration = matches!(
            capabilities["declarationProvider"],
            Value::Bool(true) | Value::Object(_)
        );
        self.write(notification("initialized", json!({})));
        for outgoing in std::mem::take(&mut self.queue) {
            self.dispatch(outgoing);
        }
    }

    fn answered(&mut self, waiting: Waiting, value: Value, answers: &mut Vec<(u64, Answer)>) {
        let ticket = waiting.ticket;
        let hits = match waiting.stage {
            Stage::Declared => {
                return self.join(ticket, Some(first_spot(value)), None, answers);
            }
            Stage::Own => {
                let own = value.as_array().and_then(|items| items.first()).cloned();
                return self.join(ticket, None, Some(own), answers);
            }
            Stage::TraitPrepare => match value.as_array().and_then(|items| items.first()) {
                Some(item) => {
                    let params = json!({ "item": item });
                    self.call(
                        ticket,
                        "callHierarchy/incomingCalls",
                        params,
                        Stage::TraitIncoming,
                    );
                    return;
                }
                None => Vec::new(),
            },
            Stage::TraitIncoming => {
                let calls = incoming_candidates(value);
                if !calls.is_empty() {
                    return self.sift(ticket, calls);
                }
                Vec::new()
            }
            Stage::Target(index) => {
                let targets = definition_hits(value);
                let kept = targets.is_empty() || self.goes_to_kept(ticket, &targets);
                return self.targeted(ticket, index, kept, answers);
            }
            Stage::Definition => definition_hits(value),
            Stage::Prepare => match value.as_array().and_then(|items| items.first()) {
                Some(item) => {
                    let params = json!({ "item": item });
                    self.call(
                        waiting.ticket,
                        "callHierarchy/incomingCalls",
                        params,
                        Stage::Incoming,
                    );
                    return;
                }
                None => Vec::new(),
            },
            Stage::Incoming => incoming_candidates(value)
                .into_iter()
                .map(|call| call.hit)
                .collect(),
            Stage::References => serde_json::from_value::<Option<Vec<Location>>>(value)
                .ok()
                .flatten()
                .unwrap_or_default()
                .iter()
                .filter_map(|location| location_hit(location, None))
                .collect(),
        };
        self.finish(ticket, waiting.stage, hits, answers);
    }

    /// Answer `ticket` with `hits`, or say none were found.
    fn finish(
        &mut self,
        ticket: u64,
        stage: Stage,
        hits: Vec<Hit>,
        answers: &mut Vec<(u64, Answer)>,
    ) {
        self.forget(ticket);
        let answer = if hits.is_empty() {
            let indexing = if self.work.is_empty() {
                String::new()
            } else {
                format!("; {} is still indexing", self.name)
            };
            Err(format!("No {} found{indexing}", stage.looked_for()))
        } else {
            Ok(dedup(hits))
        };
        answers.push((ticket, answer));
    }

    /// Drop what `ticket` still waits for.
    fn forget(&mut self, ticket: u64) {
        self.waiting.retain(|_, waiting| waiting.ticket != ticket);
        self.widening.remove(&ticket);
    }

    /// Take the declaration or the item at the cursor. With both, go
    /// through the trait's method when the declaration lies elsewhere,
    /// or ask for the item's own callers.
    fn join(
        &mut self,
        ticket: u64,
        declared: Option<Option<Spot>>,
        own: Option<Option<Value>>,
        answers: &mut Vec<(u64, Answer)>,
    ) {
        let Some(Widening::Joining {
            declared: has_declared,
            own: has_own,
        }) = self.widening.get_mut(&ticket)
        else {
            return;
        };
        if declared.is_some() {
            *has_declared = declared;
        }
        if own.is_some() {
            *has_own = own;
        }
        if has_declared.is_none() || has_own.is_none() {
            return;
        }
        let Some(Widening::Joining {
            declared: Some(declared),
            own: Some(own),
        }) = self.widening.remove(&ticket)
        else {
            return;
        };
        let Some(own) = own else {
            return self.finish(ticket, Stage::Own, Vec::new(), answers);
        };
        match (item_place(&own), declared) {
            (Some(own_place), Some(spot)) if own_place != spot.place => {
                let keep = [own_place, spot.place];
                self.widening.insert(ticket, Widening::Tracing { keep });
                let params = json!({
                    "textDocument": { "uri": spot.uri },
                    "position": spot.position
                });
                self.call(
                    ticket,
                    "textDocument/prepareCallHierarchy",
                    params,
                    Stage::TraitPrepare,
                );
            }
            _ => {
                let params = json!({ "item": own });
                self.call(
                    ticket,
                    "callHierarchy/incomingCalls",
                    params,
                    Stage::Incoming,
                );
            }
        }
    }

    /// Ask where each call to the trait's method goes.
    fn sift(&mut self, ticket: u64, calls: Vec<Candidate>) {
        let Some(Widening::Tracing { keep }) = self.widening.remove(&ticket) else {
            return;
        };
        for (index, call) in calls.iter().enumerate() {
            let params = json!({
                "textDocument": { "uri": call.spot.uri },
                "position": call.spot.position
            });
            self.call(
                ticket,
                "textDocument/definition",
                params,
                Stage::Target(index),
            );
        }
        let outstanding = calls.len();
        self.widening.insert(
            ticket,
            Widening::Sifting {
                keep,
                calls,
                outstanding,
            },
        );
    }

    /// Whether any of `targets` is the impl or the trait's method.
    fn goes_to_kept(&self, ticket: u64, targets: &[Hit]) -> bool {
        let Some(Widening::Sifting { keep, .. }) = self.widening.get(&ticket) else {
            return false;
        };
        targets.iter().any(|target| {
            keep.iter()
                .any(|(path, line)| *path == target.path && *line == target.line)
        })
    }

    /// Record whether call `index` stays; answer once every call is in.
    fn targeted(
        &mut self,
        ticket: u64,
        index: usize,
        kept: bool,
        answers: &mut Vec<(u64, Answer)>,
    ) {
        let Some(Widening::Sifting {
            calls, outstanding, ..
        }) = self.widening.get_mut(&ticket)
        else {
            return;
        };
        if let Some(call) = calls.get_mut(index) {
            call.kept = kept;
        }
        *outstanding = outstanding.saturating_sub(1);
        if *outstanding > 0 {
            return;
        }
        let Some(Widening::Sifting { calls, .. }) = self.widening.remove(&ticket) else {
            return;
        };
        let hits = calls
            .into_iter()
            .filter(|call| call.kept)
            .map(|call| call.hit)
            .collect();
        self.finish(ticket, Stage::TraitIncoming, hits, answers);
    }

    fn reply(&mut self, request: Request) {
        let result = match request.method.as_str() {
            "workspace/configuration" => Some(self.configuration(&request.params)),
            "workspace/workspaceFolders" => Some(self.folders.clone()),
            "client/registerCapability" => {
                self.watched_changed |= self.watched.register(&request.params);
                Some(Value::Null)
            }
            "client/unregisterCapability" => {
                self.watched_changed |= self.watched.unregister(&request.params);
                Some(Value::Null)
            }
            "window/workDoneProgress/create" | "window/showMessageRequest" => Some(Value::Null),
            _ => None,
        };
        let response = match result {
            Some(value) => Response::new_ok(request.id, value),
            None => Response::new_err(
                request.id,
                ErrorCode::MethodNotFound as i32,
                format!("deltoids does not handle {}", request.method),
            ),
        };
        self.write(Message::Response(response));
    }

    /// One answer per item: the options for this server's section,
    /// `null` for any other.
    fn configuration(&self, params: &Value) -> Value {
        let items = params["items"].as_array().map_or(&[][..], Vec::as_slice);
        items
            .iter()
            .map(|item| match &self.settings {
                Some(settings) if item["section"].as_str() == Some(&settings.section) => {
                    settings.options()
                }
                _ => Value::Null,
            })
            .collect()
    }

    fn progressed(&mut self, params: &Value) {
        let token = params["token"].to_string();
        let value = &params["value"];
        let percent = value["percentage"]
            .as_u64()
            .map(|percent| percent.min(100) as u8);
        let at = self.work.iter().position(|work| work.token == token);
        match value["kind"].as_str() {
            Some("begin") => {
                if let Some(at) = at {
                    self.work.remove(at);
                }
                self.work.push(Work { token, percent });
            }
            Some("report") => {
                if let Some(at) = at {
                    let mut work = self.work.remove(at);
                    work.percent = percent.or(work.percent);
                    self.work.push(work);
                }
            }
            Some("end") => {
                if let Some(at) = at {
                    self.work.remove(at);
                }
            }
            _ => {}
        }
    }

    fn id(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id += 1;
        id.into()
    }

    fn write(&mut self, message: Message) {
        if self.link.connection.sender.send(message).is_err() {
            self.stop();
        }
    }

    fn send(&mut self, outgoing: Outgoing) {
        if self.initialize.is_some() {
            self.queue.push(outgoing);
        } else {
            self.dispatch(outgoing);
        }
    }

    fn dispatch(&mut self, outgoing: Outgoing) {
        match outgoing {
            Outgoing::Message(message) => self.write(message),
            Outgoing::Ask(ask) => self.request(ask),
        }
    }

    fn request(&mut self, ask: Ask) {
        let character = position::character(&ask.line_text, ask.byte, self.encoding);
        let mut params = json!({
            "textDocument": { "uri": ask.uri },
            "position": { "line": ask.line, "character": character }
        });
        if ask.query == Query::Callers && self.call_hierarchy && self.declaration {
            self.widening.insert(
                ask.ticket,
                Widening::Joining {
                    declared: None,
                    own: None,
                },
            );
            self.call(
                ask.ticket,
                "textDocument/declaration",
                params.clone(),
                Stage::Declared,
            );
            self.call(
                ask.ticket,
                "textDocument/prepareCallHierarchy",
                params,
                Stage::Own,
            );
            return;
        }
        let (method, stage) = match ask.query {
            Query::Definition => ("textDocument/definition", Stage::Definition),
            Query::Callers if self.call_hierarchy => {
                ("textDocument/prepareCallHierarchy", Stage::Prepare)
            }
            Query::Callers => {
                params["context"] = json!({ "includeDeclaration": false });
                ("textDocument/references", Stage::References)
            }
        };
        self.call(ask.ticket, method, params, stage);
    }

    fn call(&mut self, ticket: u64, method: &str, params: Value, stage: Stage) {
        let id = self.id();
        self.waiting.insert(id.clone(), Waiting { ticket, stage });
        self.write(Message::Request(Request::new(
            id,
            method.to_string(),
            params,
        )));
    }
}

impl Drop for Server {
    /// Ask the server to shut down and exit, and kill it if it is still
    /// running after [`SHUTDOWN_GRACE`].
    fn drop(&mut self) {
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        if !self.stopped && self.initialize.is_none() {
            let id = self.id();
            self.write(Message::Request(Request::new(
                id.clone(),
                "shutdown".to_string(),
                Value::Null,
            )));
            self.await_response(&id, deadline);
            self.write(notification("exit", Value::Null));
        }
        self.link.finish(deadline);
    }
}

impl Server {
    /// Wait until `deadline` for the response to request `id`.
    fn await_response(&self, id: &RequestId, deadline: Instant) {
        loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            match self.link.connection.receiver.recv_timeout(left) {
                Ok(Message::Response(response)) if response.id == *id => return,
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    }
}

fn notification(method: &str, params: Value) -> Message {
    Message::Notification(Notification::new(method.to_string(), params))
}

fn definition_hits(value: Value) -> Vec<Hit> {
    match serde_json::from_value::<Option<GotoDefinitionResponse>>(value) {
        Ok(Some(GotoDefinitionResponse::Scalar(location))) => {
            location_hit(&location, None).into_iter().collect()
        }
        Ok(Some(GotoDefinitionResponse::Array(locations))) => locations
            .iter()
            .filter_map(|location| location_hit(location, None))
            .collect(),
        Ok(Some(GotoDefinitionResponse::Link(links))) => links
            .iter()
            .filter_map(|link| {
                hit(
                    link.target_uri.as_str(),
                    link.target_selection_range.start.line,
                    None,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Each call site, with where to ask about it again.
fn incoming_candidates(value: Value) -> Vec<Candidate> {
    serde_json::from_value::<Option<Vec<CallHierarchyIncomingCall>>>(value)
        .ok()
        .flatten()
        .unwrap_or_default()
        .iter()
        .flat_map(|call| {
            call.from_ranges.iter().filter_map(|range| {
                let hit = hit(
                    call.from.uri.as_str(),
                    range.start.line,
                    Some(call.from.name.clone()),
                )?;
                let spot = spot(call.from.uri.as_str(), range.start)?;
                Some(Candidate {
                    hit,
                    spot,
                    kept: false,
                })
            })
        })
        .collect()
}

/// The first location of a definition-shaped answer.
fn first_spot(value: Value) -> Option<Spot> {
    match serde_json::from_value::<Option<GotoDefinitionResponse>>(value).ok()?? {
        GotoDefinitionResponse::Scalar(location) => {
            spot(location.uri.as_str(), location.range.start)
        }
        GotoDefinitionResponse::Array(locations) => {
            let location = locations.first()?;
            spot(location.uri.as_str(), location.range.start)
        }
        GotoDefinitionResponse::Link(links) => {
            let link = links.first()?;
            spot(link.target_uri.as_str(), link.target_selection_range.start)
        }
    }
}

fn spot(uri: &str, position: Position) -> Option<Spot> {
    Some(Spot {
        uri: uri.to_string(),
        position,
        place: (position::path(uri)?, position.line as usize + 1),
    })
}

/// Where a call hierarchy item's name is.
fn item_place(item: &Value) -> Option<Place> {
    let path = position::path(item["uri"].as_str()?)?;
    let line = item["selectionRange"]["start"]["line"].as_u64()?;
    Some((path, line as usize + 1))
}

fn location_hit(location: &Location, caller: Option<String>) -> Option<Hit> {
    hit(location.uri.as_str(), location.range.start.line, caller)
}

fn hit(uri: &str, line: u32, caller: Option<String>) -> Option<Hit> {
    Some(Hit {
        path: position::path(uri)?,
        line: line as usize + 1,
        caller,
    })
}

/// `hits` with repeats of a path and line dropped, first one kept.
fn dedup(hits: Vec<Hit>) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter(|hit| seen.insert((hit.path.clone(), hit.line)))
        .collect()
}
