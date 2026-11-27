mod dispatch;
mod protocol;
mod scheduler;
mod scope;
mod semantic;
#[cfg(test)]
mod symbol_request_tests;
mod symbols;
#[cfg(test)]
mod test_support;
mod workspace;
mod workspace_roots;

use std::{env, path::PathBuf};

use anyhow::{Context, Result, ensure};
use crossbeam_channel::{bounded, never, select_biased, unbounded};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request};
use lsp_types::{
    CancelParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializeResult, NumberOrString, PositionEncodingKind, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
    notification::{
        Cancel, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument,
        DidOpenTextDocument, DidSaveTextDocument, Exit, Initialized, Notification as _,
    },
    request::{Initialize, Request as _, Shutdown},
};
use serde_json::Value;

use protocol::{RequestResult, file_path, invalid_params, send_response};
use scheduler::{Scheduler, WorkspaceResult};
use semantic::Engine;
use semantic::completion::CompletionClientCapabilities;

#[cfg(test)]
use lsp_server::{RequestId, Response};
#[cfg(test)]
use lsp_types::{
    Uri,
    request::{DocumentSymbolRequest, WorkspaceSymbolRequest},
};
#[cfg(test)]
use protocol::path_uri;

enum State {
    Starting,
    Running(Box<Running>),
    Shutdown,
}

struct Running {
    engine: Engine,
    watch_files: bool,
    hierarchical_document_symbols: bool,
    completion_capabilities: CompletionClientCapabilities,
    scheduler: Scheduler,
}

impl Running {
    fn workspace_loaded(&mut self, connection: &Connection, update: WorkspaceResult) -> Result<()> {
        self.scheduler
            .workspace_loaded(&mut self.engine, connection, update)
    }

    fn advance_search(&mut self, connection: &Connection) -> Result<()> {
        self.scheduler.advance_search(&mut self.engine, connection)
    }
}

// Keep the existing in-memory symbol request harness attached to the session.
#[cfg(test)]
impl std::ops::Deref for Running {
    type Target = Scheduler;

    fn deref(&self) -> &Self::Target {
        &self.scheduler
    }
}

#[cfg(test)]
impl std::ops::DerefMut for Running {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scheduler
    }
}

fn main() -> Result<()> {
    let (connection, threads) = Connection::stdio();
    let result = serve(&connection);
    drop(connection);
    result?;
    threads.join().context("joining LSP transport threads")
}

fn serve(connection: &Connection) -> Result<()> {
    let mut state = State::Starting;
    let (load_sender, load_receiver) = unbounded();
    let (search_token, search_ready) = bounded(1);
    let search_idle = never();
    search_token
        .send(())
        .context("scheduling navigation searches")?;
    loop {
        let (search_receiver, completion_wake) = match &state {
            State::Running(running) => (
                if running.scheduler.ready_search(&running.engine).is_some() {
                    &search_ready
                } else {
                    &search_idle
                },
                running.engine.completion_wake(),
            ),
            _ => (&search_idle, &search_idle),
        };
        select_biased! {
            recv(connection.receiver) -> message => {
                let Ok(message) = message else {
                    break;
                };
                match message {
                    Message::Request(request) => {
                        let initializing = matches!(state, State::Starting) && request.method == Initialize::METHOD;
                        handle_request(connection, &mut state, request)?;
                        if initializing && let State::Running(running) = &state {
                            running.engine.prewarm_completion_sdk(false);
                        }
                    }
                    Message::Notification(notification) if notification.method == Exit::METHOD => {
                        ensure!(
                            matches!(state, State::Shutdown),
                            "exit arrived before shutdown"
                        );
                        break;
                    }
                    Message::Notification(notification) => {
                        if let State::Running(running) = &mut state {
                            let Notification { method, params } = notification;
                            if method == Initialized::METHOD && std::mem::take(&mut running.watch_files) {
                                register_file_watches(connection)?;
                            } else if let Err(error) = handle_notification(connection, running, &method, params) {
                                eprintln!("rs-gopls: {method}: {error:#}");
                            }
                        }
                    }
                    Message::Response(_) => {}
                }
            }
            recv(load_receiver) -> update => {
                if let State::Running(running) = &mut state {
                    running.workspace_loaded(connection, update.context("receiving Go package metadata")?)?;
                }
            }
            recv(completion_wake) -> _ => {}
            recv(search_receiver) -> ready => {
                ready.context("receiving navigation search work")?;
                if let State::Running(running) = &mut state {
                    running.advance_search(connection)?;
                }
                search_token.send(()).context("scheduling further navigation searches")?;
            }
        }
        if let State::Running(running) = &mut state {
            running
                .scheduler
                .start_workspace_load(&running.engine, &load_sender)?;
        }
    }
    Ok(())
}

fn handle_request(connection: &Connection, state: &mut State, request: Request) -> Result<()> {
    let Request { id, method, params } = request;
    let result: RequestResult = match (state, method.as_str()) {
        (state @ State::Starting, Initialize::METHOD) => initialize(params)
            .map(|(running, capabilities)| {
                *state = State::Running(Box::new(running));
                capabilities
            })
            .map_err(invalid_params),
        (State::Starting, _) => Err((
            ErrorCode::ServerNotInitialized,
            "initialize must be called first".into(),
        )),
        (State::Shutdown, _) => Err((
            ErrorCode::InvalidRequest,
            "server has already shut down".into(),
        )),
        (State::Running(..), Initialize::METHOD) => Err((
            ErrorCode::InvalidRequest,
            "server is already initialized".into(),
        )),
        (state @ State::Running(..), Shutdown::METHOD) => {
            if let State::Running(running) = state {
                running.scheduler.shutdown(connection)?;
            }
            *state = State::Shutdown;
            Ok(Value::Null)
        }
        (State::Running(running), _) => {
            return dispatch::handle(
                connection,
                &mut running.engine,
                &mut running.scheduler,
                running.hierarchical_document_symbols,
                running.completion_capabilities,
                Request { id, method, params },
            );
        }
    };
    send_response(connection, id, result)
}

fn initialize(params: Value) -> Result<(Running, Value)> {
    let params = serde_json::from_value::<InitializeParams>(params)
        .context("invalid initialize parameters")?;
    let watch_files = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .is_some_and(|watches| watches.dynamic_registration == Some(true));
    let hierarchical_document_symbols = params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|document| document.document_symbol.as_ref())
        .is_some_and(|symbols| symbols.hierarchical_document_symbol_support == Some(true));
    let completion_capabilities = CompletionClientCapabilities::from_initialize(&params);
    let root = workspace_root(params)?;
    ensure!(
        root.is_dir(),
        "workspace is not a directory: {}",
        root.display()
    );

    let mut capabilities = ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        ..Default::default()
    };
    dispatch::write_capabilities(&mut capabilities);
    let result = serde_json::to_value(InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    })?;
    Ok((
        Running {
            watch_files,
            hierarchical_document_symbols,
            completion_capabilities,
            engine: Engine::new(root),
            scheduler: Scheduler::default(),
        },
        result,
    ))
}

#[allow(deprecated)]
fn workspace_root(params: InitializeParams) -> Result<PathBuf> {
    if let Some(folders) = params.workspace_folders {
        ensure!(folders.len() <= 1, "only one workspace folder is supported");
        if let Some(folder) = folders.first() {
            return file_path(&folder.uri);
        }
    }
    if let Some(uri) = params.root_uri {
        return file_path(&uri);
    }
    if let Some(path) = params.root_path {
        return Ok(env::current_dir()?.join(path));
    }
    env::current_dir().context("getting the current directory for a client without a workspace")
}

fn register_file_watches(connection: &Connection) -> Result<()> {
    let watchers: Vec<_> = [
        "**/*.go",
        "**/go.mod",
        "**/go.sum",
        "**/go.work",
        "**/go.work.sum",
    ]
    .into_iter()
    .map(|pattern| serde_json::json!({ "globPattern": pattern }))
    .collect();
    let request = Request::new(
        "rs-gopls/watch-go-files".to_owned().into(),
        "client/registerCapability".to_owned(),
        serde_json::json!({
            "registrations": [{
                "id": "rs-gopls/watch-go-files",
                "method": DidChangeWatchedFiles::METHOD,
                "registerOptions": { "watchers": watchers }
            }]
        }),
    );
    connection
        .sender
        .send(request.into())
        .context("registering Go file watches")
}

fn handle_notification(
    connection: &Connection,
    running: &mut Running,
    method: &str,
    params: Value,
) -> Result<()> {
    match method {
        DidChangeWatchedFiles::METHOD => {
            let params: DidChangeWatchedFilesParams = serde_json::from_value(params)?;
            let before = running.engine.workspace_load();
            for change in params.changes {
                if let Ok(path) = file_path(&change.uri) {
                    running.engine.file_changed(&path, change.typ);
                    if !running.engine.is_open(&path) {
                        running.scheduler.document_modified(connection, &path)?;
                    }
                }
            }
            let after = running.engine.workspace_load();
            if after != before {
                running.scheduler.workspace_modified(connection, after)?;
            }
            Ok(())
        }
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(params)?;
            let document = params.text_document;
            let path = file_path(&document.uri)?;
            running
                .engine
                .open(path.clone(), document.text, document.version)?;
            running.scheduler.document_modified(connection, &path)
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            running
                .engine
                .edit(&path, &params.content_changes, params.text_document.version)?;
            running.scheduler.document_modified(connection, &path)
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            running.engine.close(&path);
            running.scheduler.document_modified(connection, &path)
        }
        DidSaveTextDocument::METHOD => {
            let params: DidSaveTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            let before = running.engine.workspace_load();
            running
                .engine
                .file_changed(&path, lsp_types::FileChangeType::CHANGED);
            running.scheduler.document_modified(connection, &path)?;
            let after = running.engine.workspace_load();
            if after != before {
                running.scheduler.workspace_modified(connection, after)?;
            }
            Ok(())
        }
        Cancel::METHOD => {
            let params: CancelParams = serde_json::from_value(params)?;
            let id = match params.id {
                NumberOrString::Number(value) => value.into(),
                NumberOrString::String(value) => value.into(),
            };
            running.scheduler.cancel(connection, &id)
        }
        _ => Ok(()),
    }
}
