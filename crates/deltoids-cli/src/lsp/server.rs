//! One running language server: the handshake, the queue that holds
//! messages until it completes, document versions, requests in flight,
//! progress, and shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::TryRecvError;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{CallHierarchyIncomingCall, GotoDefinitionResponse, Location};
use serde_json::{Value, json};

use super::config::{ServerSettings, ServerSpec};
use super::position::{self, Encoding};
use super::transport::Link;
use super::{Phase, Query};

/// How long quitting waits for a server to shut down before killing it.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// A result location, before its path is made relative to the workdir.
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
}

impl Stage {
    fn looked_for(self) -> &'static str {
        match self {
            Stage::Definition => "definition",
            Stage::Prepare | Stage::Incoming | Stage::References => "callers",
        }
    }
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
    queue: Vec<Outgoing>,
    waiting: HashMap<RequestId, Waiting>,
    work: Vec<Work>,
    documents: HashMap<String, Document>,
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
            queue: Vec::new(),
            waiting: HashMap::new(),
            work: Vec::new(),
            documents: HashMap::new(),
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
                "workspace": { "workspaceFolders": true, "configuration": true },
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false },
                    "definition": { "linkSupport": true },
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
                    self.stopped = true;
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
        for (_, waiting) in self.waiting.drain() {
            answers.push((waiting.ticket, Err(message.clone())));
        }
        for outgoing in self.queue.drain(..) {
            if let Outgoing::Ask(ask) = outgoing {
                answers.push((ask.ticket, Err(message.clone())));
            }
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
                match response.response_result {
                    Ok(value) => self.answered(waiting, value, answers),
                    Err(error) => answers.push((
                        waiting.ticket,
                        Err(format!("{}: {}", self.name, error.message)),
                    )),
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
            self.stopped = true;
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
        self.write(notification("initialized", json!({})));
        for outgoing in std::mem::take(&mut self.queue) {
            self.dispatch(outgoing);
        }
    }

    fn answered(&mut self, waiting: Waiting, value: Value, answers: &mut Vec<(u64, Answer)>) {
        let hits = match waiting.stage {
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
            Stage::Incoming => incoming_hits(value),
            Stage::References => serde_json::from_value::<Option<Vec<Location>>>(value)
                .ok()
                .flatten()
                .unwrap_or_default()
                .iter()
                .filter_map(|location| location_hit(location, None))
                .collect(),
        };
        let answer = if hits.is_empty() {
            let indexing = if self.work.is_empty() {
                String::new()
            } else {
                format!("; {} is still indexing", self.name)
            };
            Err(format!("No {} found{indexing}", waiting.stage.looked_for()))
        } else {
            Ok(dedup(hits))
        };
        answers.push((waiting.ticket, answer));
    }

    fn reply(&mut self, request: Request) {
        let result = match request.method.as_str() {
            "workspace/configuration" => Some(self.configuration(&request.params)),
            "workspace/workspaceFolders" => Some(self.folders.clone()),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => Some(Value::Null),
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
            self.stopped = true;
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

fn incoming_hits(value: Value) -> Vec<Hit> {
    serde_json::from_value::<Option<Vec<CallHierarchyIncomingCall>>>(value)
        .ok()
        .flatten()
        .unwrap_or_default()
        .iter()
        .flat_map(|call| {
            call.from_ranges.iter().filter_map(|range| {
                hit(
                    call.from.uri.as_str(),
                    range.start.line,
                    Some(call.from.name.clone()),
                )
            })
        })
        .collect()
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
