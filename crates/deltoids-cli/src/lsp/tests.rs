//! The `Lsp` interface against scripted fake servers.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use deltoids::Language;
use lsp_server::{Message, Notification, Request, RequestId};
use serde_json::{Value, json};

use super::fake::{self, Reply, answering};
use super::*;

const ROOT: &str = "/work/shop";

const MAIN: &str = "mod cart;\n\nfn main() {\n    let total = cart::total();\n}\n";

fn utf8() -> Value {
    json!({ "positionEncoding": "utf-8", "callHierarchyProvider": true })
}

fn uri(path: &str) -> String {
    format!("file://{ROOT}/{path}")
}

fn location(path: &str, line: u32) -> Value {
    json!({
        "uri": uri(path),
        "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 1 } }
    })
}

/// `name` on 1-based `line` of `text`, in a Rust file at `path`.
fn symbol<'a>(
    language: Language,
    path: &'a str,
    text: &'a str,
    line: usize,
    name: &str,
) -> Symbol<'a> {
    let start = text.lines().nth(line - 1).unwrap().find(name).unwrap();
    Symbol {
        language,
        path,
        text,
        line,
        columns: start..start + name.len(),
    }
}

fn total(text: &str) -> Symbol<'_> {
    symbol(Language::Rust, "src/main.rs", text, 4, "total()")
}

fn wait(lsp: &Lsp, ticket: &Ticket) -> Result<Vec<Found>, String> {
    for _ in 0..400 {
        if let Some(answer) = lsp.answer(ticket) {
            return answer;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("no answer");
}

fn found(path: &str, line: usize, caller: Option<&str>) -> Found {
    Found {
        path: path.to_string(),
        line,
        caller: caller.map(str::to_string),
    }
}

#[test]
fn a_definition_location_comes_back_workdir_relative() {
    let (lsp, _) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/definition", location("src/cart.rs", 9)),
    );
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Ok(vec![found("src/cart.rs", 10, None)])
    );
}

#[test]
fn definition_links_and_arrays_become_locations_without_repeats() {
    let links = json!([{
        "targetUri": uri("src/cart.rs"),
        "targetRange": { "start": { "line": 7, "character": 0 }, "end": { "line": 12, "character": 1 } },
        "targetSelectionRange": { "start": { "line": 8, "character": 4 }, "end": { "line": 8, "character": 9 } }
    }]);
    let (lsp, _) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/definition", links),
    );
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(wait(&lsp, &ticket), Ok(vec![found("src/cart.rs", 9, None)]));

    let mut elsewhere = location("src/b.rs", 2);
    elsewhere["uri"] = json!("file:///elsewhere/b.rs");
    let array = json!([location("src/a.rs", 0), location("src/a.rs", 0), elsewhere]);
    let (lsp, _) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/definition", array),
    );
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Ok(vec![
            found("src/a.rs", 1, None),
            found("/elsewhere/b.rs", 3, None)
        ])
    );
}

#[test]
fn callers_come_from_the_call_hierarchy_with_their_function_names() {
    let item = json!({
        "name": "total", "kind": 12, "uri": uri("src/cart.rs"),
        "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 11, "character": 1 } },
        "selectionRange": { "start": { "line": 9, "character": 7 }, "end": { "line": 9, "character": 12 } },
        "data": { "opaque": 7 }
    });
    let caller = |name: &str, path: &str, lines: &[u32]| {
        let mut from = item.clone();
        from["name"] = json!(name);
        from["uri"] = json!(uri(path));
        let ranges: Vec<Value> = lines
            .iter()
            .map(|line| json!({ "start": { "line": line, "character": 4 }, "end": { "line": line, "character": 9 } }))
            .collect();
        json!({ "from": from, "fromRanges": ranges })
    };
    let calls = json!([
        caller("main", "src/main.rs", &[3]),
        caller("summary", "src/report.rs", &[3, 5])
    ]);
    let prepared = json!([item]);
    let script: fake::Script = Arc::new(move |message| match message {
        Message::Request(r) if r.method == "textDocument/prepareCallHierarchy" => {
            Reply::Result(prepared.clone())
        }
        Message::Request(r) if r.method == "callHierarchy/incomingCalls" => {
            Reply::Result(calls.clone())
        }
        _ => Reply::Default,
    });
    let (lsp, record) = fake::lsp(Path::new(ROOT), utf8(), script);
    let ticket = lsp.ask(Query::Callers, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Ok(vec![
            found("src/main.rs", 4, Some("main")),
            found("src/report.rs", 4, Some("summary")),
            found("src/report.rs", 6, Some("summary")),
        ])
    );
    let incoming = record.params("callHierarchy/incomingCalls");
    assert_eq!(incoming[0]["item"]["data"], json!({ "opaque": 7 }));
}

#[test]
fn callers_fall_back_to_references_without_a_call_hierarchy() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        json!({ "positionEncoding": "utf-8" }),
        answering(
            "textDocument/references",
            json!([location("src/report.rs", 3)]),
        ),
    );
    let ticket = lsp.ask(Query::Callers, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Ok(vec![found("src/report.rs", 4, None)])
    );
    let params = record.params("textDocument/references");
    assert_eq!(params[0]["context"]["includeDeclaration"], json!(false));
}

#[test]
fn a_utf16_server_gets_columns_in_utf16_units() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        json!({}),
        answering("textDocument/definition", location("src/cart.rs", 0)),
    );
    let text = "fn main() {\n    let é😀 = go();\n}\n";
    let ticket = lsp
        .ask(
            Query::Definition,
            symbol(Language::Rust, "src/main.rs", text, 2, "go"),
        )
        .unwrap();
    wait(&lsp, &ticket).unwrap();
    let position = &record.params("textDocument/definition")[0]["position"];
    assert_eq!(position, &json!({ "line": 1, "character": 14 }));
}

#[test]
fn documents_open_once_and_change_only_when_their_text_does() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/definition", location("src/cart.rs", 0)),
    );
    for _ in 0..2 {
        let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
        wait(&lsp, &ticket).unwrap();
    }
    let edited = MAIN.replace("total", "sum");
    lsp.sync([("src/main.rs", MAIN), ("src/other.rs", "fn x() {}")]);
    lsp.sync([("src/main.rs", edited.as_str())]);
    lsp.sync([("src/main.rs", edited.as_str())]);
    thread::sleep(Duration::from_millis(50));

    let methods = record.methods();
    let first_open = methods
        .iter()
        .position(|m| m == "textDocument/didOpen")
        .unwrap();
    let first_ask = methods
        .iter()
        .position(|m| m == "textDocument/definition")
        .unwrap();
    assert!(first_open < first_ask, "{methods:?}");
    assert_eq!(record.params("textDocument/didOpen").len(), 1);
    let changes = record.params("textDocument/didChange");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["textDocument"]["version"], json!(2));
    assert_eq!(changes[0]["contentChanges"][0]["text"], json!(edited));
}

#[test]
fn typescript_and_tsx_share_one_server() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/definition", location("src/a.ts", 0)),
    );
    let text = "go();\n";
    for (language, path) in [
        (Language::TypeScript, "src/a.ts"),
        (Language::Tsx, "src/b.tsx"),
    ] {
        let ticket = lsp
            .ask(Query::Definition, symbol(language, path, text, 1, "go"))
            .unwrap();
        wait(&lsp, &ticket).unwrap();
    }
    assert_eq!(record.launches(), 1);
    assert_eq!(record.params("initialize").len(), 1);
    let languages: Vec<Value> = record
        .params("textDocument/didOpen")
        .iter()
        .map(|params| params["textDocument"]["languageId"].clone())
        .collect();
    assert_eq!(languages, [json!("typescript"), json!("typescriptreact")]);
}

fn progress(token: &str, value: Value) -> Message {
    Message::Notification(Notification::new(
        "$/progress".to_string(),
        json!({ "token": token, "value": value }),
    ))
}

#[test]
fn state_goes_from_off_to_loading_with_progress_to_ready() {
    let (release, gate) = crossbeam_channel::unbounded::<Vec<Message>>();
    let script: fake::Script = Arc::new(move |message| match message {
        Message::Request(r) if r.method == "initialize" => Reply::Then(
            gate.recv().unwrap(),
            Some(json!({ "capabilities": utf8() })),
        ),
        Message::Notification(n) if n.method == "initialized" => {
            Reply::Then(gate.recv().unwrap(), None)
        }
        Message::Request(r) if r.method == "textDocument/definition" => {
            Reply::Then(gate.recv().unwrap(), Some(location("src/cart.rs", 0)))
        }
        _ => Reply::Default,
    });
    let (lsp, _) = fake::lsp(Path::new(ROOT), utf8(), script);
    assert_eq!(lsp.state(Language::Rust), State::Off, "not started");
    assert_eq!(lsp.state(Language::Json), State::Off, "no server");
    lsp.warm(Language::Rust);
    assert_eq!(lsp.state(Language::Rust), State::Loading(None));

    release.send(Vec::new()).unwrap();
    release
        .send(vec![
            progress(
                "index",
                json!({ "kind": "begin", "title": "Indexing", "percentage": 10 }),
            ),
            progress("index", json!({ "kind": "report", "percentage": 62 })),
        ])
        .unwrap();
    assert!(eventually(
        || lsp.state(Language::Rust) == State::Loading(Some(62))
    ));

    release
        .send(vec![progress("index", json!({ "kind": "end" }))])
        .unwrap();
    let _ = lsp.ask(Query::Definition, total(MAIN));
    assert!(eventually(|| lsp.state(Language::Rust) == State::Ready));
}

fn eventually(mut check: impl FnMut() -> bool) -> bool {
    (0..400).any(|_| {
        let ok = check();
        if !ok {
            thread::sleep(Duration::from_millis(5));
        }
        ok
    })
}

#[test]
fn an_empty_answer_says_so_and_mentions_indexing_in_progress() {
    let script: fake::Script = Arc::new(|message| match message {
        Message::Notification(n) if n.method == "initialized" => Reply::Then(
            vec![progress(
                "t",
                json!({ "kind": "begin", "title": "Indexing" }),
            )],
            None,
        ),
        Message::Request(r) if r.method == "textDocument/definition" => Reply::Result(Value::Null),
        Message::Request(r) if r.method == "textDocument/prepareCallHierarchy" => {
            Reply::Result(json!([]))
        }
        _ => Reply::Default,
    });
    let (lsp, _) = fake::lsp(Path::new(ROOT), utf8(), script);
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Err("No definition found; rust-analyzer is still indexing".to_string())
    );

    let (lsp, _) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        answering("textDocument/prepareCallHierarchy", json!([])),
    );
    let ticket = lsp.ask(Query::Callers, total(MAIN)).unwrap();
    assert_eq!(wait(&lsp, &ticket), Err("No callers found".to_string()));
}

#[test]
fn requests_from_the_server_get_replies() {
    let script: fake::Script = Arc::new(|message| match message {
        Message::Notification(n) if n.method == "initialized" => Reply::Then(
            vec![
                Message::Request(Request::new(
                    RequestId::from("config".to_string()),
                    "workspace/configuration".to_string(),
                    json!({ "items": [{ "section": "a" }, { "section": "b" }] }),
                )),
                Message::Request(Request::new(
                    RequestId::from("odd".to_string()),
                    "workspace/applyEdit".to_string(),
                    json!({}),
                )),
            ],
            None,
        ),
        _ => Reply::Default,
    });
    let (lsp, record) = fake::lsp(Path::new(ROOT), utf8(), script);
    lsp.warm(Language::Rust);
    assert!(eventually(|| {
        lsp.warm(Language::Rust);
        record
            .messages()
            .iter()
            .filter(|message| matches!(message, Message::Response(_)))
            .count()
            == 2
    }));
    let responses: Vec<_> = record
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            Message::Response(response) => Some(response),
            _ => None,
        })
        .collect();
    assert_eq!(
        responses[0].response_result.as_ref().unwrap(),
        &json!([null, null])
    );
    assert!(responses[1].response_result.is_err());
}

fn rust_analyzer_options() -> Value {
    json!({ "cargo": { "targetDir": true }, "checkOnSave": false })
}

#[test]
fn rust_analyzer_starts_with_its_own_target_dir_and_no_check_on_save() {
    let (lsp, record) = fake::lsp(Path::new(ROOT), utf8(), answering("none", Value::Null));
    lsp.warm(Language::Rust);
    lsp.warm(Language::Go);
    assert!(eventually(|| record.params("initialize").len() == 2));
    let options: Vec<Option<Value>> = record
        .params("initialize")
        .iter()
        .map(|params| params.get("initializationOptions").cloned())
        .collect();
    assert!(options.contains(&Some(rust_analyzer_options())));
    assert!(options.contains(&None), "gopls gets no options");
}

#[test]
fn rust_analyzer_asking_for_its_configuration_gets_the_same_options() {
    let script: fake::Script = Arc::new(|message| match message {
        Message::Notification(n) if n.method == "initialized" => Reply::Then(
            vec![Message::Request(Request::new(
                RequestId::from("config".to_string()),
                "workspace/configuration".to_string(),
                json!({ "items": [{ "section": "rust-analyzer" }, { "section": "other" }] }),
            ))],
            None,
        ),
        _ => Reply::Default,
    });
    let (lsp, record) = fake::lsp(Path::new(ROOT), utf8(), script);
    lsp.warm(Language::Rust);
    let reply = || {
        record
            .messages()
            .into_iter()
            .find_map(|message| match message {
                Message::Response(response) => response.response_result.ok(),
                _ => None,
            })
    };
    assert!(eventually(|| {
        lsp.warm(Language::Rust);
        reply().is_some()
    }));
    assert_eq!(reply(), Some(json!([rust_analyzer_options(), null])));
}

#[test]
fn a_crash_fails_the_pending_query_and_the_next_one_restarts_the_server() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        Arc::new(|message| match message {
            Message::Request(r) if r.method == "textDocument/definition" => Reply::Hangup,
            _ => Reply::Default,
        }),
    );
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(
        wait(&lsp, &ticket),
        Err("rust-analyzer stopped; it restarts on the next request".to_string())
    );
    assert_eq!(lsp.state(Language::Rust), State::Failed);
    let _ = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert_eq!(record.launches(), 2);
}

#[test]
fn a_server_that_exits_reports_its_last_error_line() {
    let table = ServerTable::parse(
        "[lsp.rust]\ncommand = [\"sh\", \"-c\", \"echo 'error: no toolchain' >&2\"]\n",
    );
    let lsp = Lsp::with_launcher(Path::new("/"), table, Box::new(super::transport::spawn));
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    let answer = (0..400).find_map(|_| {
        thread::sleep(Duration::from_millis(5));
        lsp.answer(&ticket)
    });
    assert_eq!(
        answer,
        Some(Err("sh stopped: error: no toolchain".to_string()))
    );
}

#[test]
fn a_missing_or_disabled_server_fails_the_ask() {
    let table = ServerTable::parse(
        "[lsp.go]\ncommand = [\"deltoids-no-such-server\"]\n[lsp.python]\nenabled = false\n",
    );
    let lsp = Lsp::with_launcher(Path::new(ROOT), table, Box::new(super::transport::spawn));
    let go = lsp.ask(
        Query::Definition,
        symbol(Language::Go, "a.go", "go()\n", 1, "go"),
    );
    assert_eq!(
        go.unwrap_err(),
        "No language server for Go (deltoids-no-such-server not found)"
    );
    assert_eq!(
        lsp.state(Language::Go),
        State::Off,
        "a missing program is off"
    );
    let python = lsp.ask(
        Query::Definition,
        symbol(Language::Python, "a.py", "go()\n", 1, "go"),
    );
    assert_eq!(python.unwrap_err(), "No language server for Python");
    assert!(!lsp.serves(Language::Python));
}

#[test]
fn a_cancelled_query_is_cancelled_at_the_server_and_never_answers() {
    let (lsp, record) = fake::lsp(
        Path::new(ROOT),
        utf8(),
        Arc::new(|message| match message {
            Message::Request(r) if r.method == "textDocument/definition" => Reply::Silent,
            _ => Reply::Default,
        }),
    );
    let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
    assert!(eventually(|| {
        lsp.state(Language::Rust);
        !record.params("textDocument/definition").is_empty()
    }));
    let id = record
        .messages()
        .into_iter()
        .find_map(|message| match message {
            Message::Request(r) if r.method == "textDocument/definition" => Some(r.id),
            _ => None,
        })
        .unwrap();
    let probe = Ticket(ticket.0);
    lsp.cancel(ticket);
    assert!(eventually(|| !record.params("$/cancelRequest").is_empty()));
    assert_eq!(record.params("$/cancelRequest")[0]["id"], json!(id));
    thread::sleep(Duration::from_millis(30));
    assert_eq!(lsp.answer(&probe), None);
}

#[test]
fn dropping_the_last_handle_shuts_the_servers_down() {
    let (lsp, record) = fake::lsp(Path::new(ROOT), utf8(), Arc::new(|_| Reply::Default));
    lsp.warm(Language::Rust);
    assert!(eventually(|| lsp.state(Language::Rust) == State::Ready));
    let clone = lsp.clone();
    drop(lsp);
    assert!(record.params("shutdown").is_empty(), "a clone is alive");
    drop(clone);
    assert!(eventually(|| record
        .methods()
        .ends_with(&["shutdown".to_string(), "exit".to_string()])));
}

#[test]
#[ignore = "needs rust-analyzer (on PATH, or its path in RUST_ANALYZER); run with --ignored"]
fn rust_analyzer_finds_a_definition_in_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, text: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("src/cart.rs", "pub fn total() -> u32 {\n    42\n}\n");
    write("src/main.rs", MAIN);
    let program = std::env::var("RUST_ANALYZER").unwrap_or("rust-analyzer".to_string());
    let table = ServerTable::parse(&format!("[lsp.rust]\ncommand = [{program:?}]\n"));
    let lsp = Lsp::with_launcher(dir.path(), table, Box::new(super::transport::spawn));
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut last = Err(String::new());
    while std::time::Instant::now() < deadline {
        let ticket = lsp.ask(Query::Definition, total(MAIN)).unwrap();
        last = loop {
            if let Some(answer) = lsp.answer(&ticket) {
                break answer;
            }
            thread::sleep(Duration::from_millis(20));
        };
        if last.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(last, Ok(vec![found("src/cart.rs", 1, None)]));
    assert!(dir.path().join("target/rust-analyzer").is_dir());
    assert!(!dir.path().join("target/debug").exists());
}
