//! A scripted language server on an in-memory connection: the test
//! adapter at the transport seam.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use lsp_server::{Connection, ErrorCode, Message, Request, RequestId, Response};
use serde_json::{Value, json};

use super::Lsp;
use super::config::ServerTable;
use super::transport::Link;

/// How the fake answers one message from the client.
pub(crate) enum Reply {
    /// Answer `initialize` with the fake's capabilities, `shutdown` and
    /// any other request with `null`; ignore notifications.
    Default,
    /// Answer the request with this result.
    Result(Value),
    /// Send these messages, then answer the request with the result, if
    /// any.
    Then(Vec<Message>, Option<Value>),
    /// Answer the request with this error message.
    Error(String),
    /// Say nothing.
    Silent,
    /// Close the connection, as a crashing server would.
    Hangup,
}

pub(crate) type Script = Arc<dyn Fn(&Message) -> Reply + Send + Sync>;

/// What the client did, as the fake saw it.
#[derive(Clone, Default)]
pub(crate) struct Record {
    log: Arc<Mutex<Vec<Message>>>,
    launches: Arc<AtomicUsize>,
}

impl Record {
    /// Every message the client sent, in order.
    pub(crate) fn messages(&self) -> Vec<Message> {
        self.log.lock().unwrap().clone()
    }

    /// The params of every request or notification named `method`.
    pub(crate) fn params(&self, method: &str) -> Vec<Value> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                Message::Request(request) if request.method == method => Some(request.params),
                Message::Notification(note) if note.method == method => Some(note.params),
                _ => None,
            })
            .collect()
    }

    /// Methods of the requests and notifications, in order.
    pub(crate) fn methods(&self) -> Vec<String> {
        self.messages()
            .into_iter()
            .filter_map(|message| match message {
                Message::Request(request) => Some(request.method),
                Message::Notification(note) => Some(note.method),
                Message::Response(_) => None,
            })
            .collect()
    }

    /// How many servers were started.
    pub(crate) fn launches(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
    }

    /// Whether the client answered the server's request `id`.
    pub(crate) fn replied(&self, id: &str) -> bool {
        let id = RequestId::from(id.to_string());
        self.messages()
            .iter()
            .any(|message| matches!(message, Message::Response(response) if response.id == id))
    }
}

/// An [`Lsp`] for `root` whose servers (for every default language) are
/// fakes advertising `capabilities` and answering with `script`.
pub(crate) fn lsp(root: &Path, capabilities: Value, script: Script) -> (Lsp, Record) {
    let record = Record::default();
    let launched = record.clone();
    let launch = Box::new(move |_: &super::ServerSpec, _: &Path| {
        launched.launches.fetch_add(1, Ordering::SeqCst);
        let (client, server) = Connection::memory();
        let (log, script, capabilities) = (
            Arc::clone(&launched.log),
            Arc::clone(&script),
            capabilities.clone(),
        );
        thread::spawn(move || serve(server, &log, &*script, &capabilities));
        Ok(Link {
            connection: client,
            process: None,
            last_error: Arc::default(),
            stderr_closed: None,
        })
    });
    (
        Lsp::with_launcher(root, ServerTable::defaults(), launch),
        record,
    )
}

/// A request from the server to hear about changes to the files
/// `watchers` match, registered as `id`. `request` is the request's
/// own id, for [`Record::replied`].
pub(crate) fn register_watchers(request: &str, id: &str, watchers: Value) -> Message {
    Message::Request(Request::new(
        RequestId::from(request.to_string()),
        "client/registerCapability".to_string(),
        json!({ "registrations": [{
            "id": id,
            "method": "workspace/didChangeWatchedFiles",
            "registerOptions": { "watchers": watchers }
        }] }),
    ))
}

/// A script that registers `watchers` once initialized, in request
/// `"register"`, and leaves the rest to [`Reply::Default`].
pub(crate) fn watching(watchers: Value) -> Script {
    Arc::new(move |message| match message {
        Message::Notification(note) if note.method == "initialized" => Reply::Then(
            vec![register_watchers("register", "watch", watchers.clone())],
            None,
        ),
        _ => Reply::Default,
    })
}

/// A script that leaves everything to [`Reply::Default`] except
/// `method`, answered with `result`.
pub(crate) fn answering(method: &'static str, result: Value) -> Script {
    Arc::new(move |message| match message {
        Message::Request(request) if request.method == method => Reply::Result(result.clone()),
        _ => Reply::Default,
    })
}

fn serve(
    server: Connection,
    log: &Mutex<Vec<Message>>,
    script: &(dyn Fn(&Message) -> Reply + Send + Sync),
    capabilities: &Value,
) {
    for message in &server.receiver {
        log.lock().unwrap().push(message.clone());
        let request = match &message {
            Message::Request(request) => Some(request.clone()),
            _ => None,
        };
        let respond = |result: Value| {
            if let Some(Request { id, .. }) = &request {
                let _ = server
                    .sender
                    .send(Message::Response(Response::new_ok(id.clone(), result)));
            }
        };
        match script(&message) {
            Reply::Default => match &request {
                Some(request) if request.method == "initialize" => {
                    respond(json!({ "capabilities": capabilities }));
                }
                Some(_) => respond(Value::Null),
                None => {}
            },
            Reply::Result(result) => respond(result),
            Reply::Error(message) => {
                if let Some(Request { id, .. }) = &request {
                    let _ = server.sender.send(Message::Response(Response::new_err(
                        id.clone(),
                        ErrorCode::ContentModified as i32,
                        message,
                    )));
                }
            }
            Reply::Then(messages, result) => {
                for message in messages {
                    let _ = server.sender.send(message);
                }
                if let Some(result) = result {
                    respond(result);
                }
            }
            Reply::Silent => {}
            Reply::Hangup => return,
        }
        if matches!(&message, Message::Notification(note) if note.method == "exit") {
            return;
        }
    }
}
