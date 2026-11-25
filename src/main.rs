mod scope;
mod semantic;
mod symbols;
mod workspace;
mod workspace_roots;

use std::{
    env,
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, Result, anyhow, ensure};
use crossbeam_channel::{Sender, select_biased, unbounded};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CancelParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams, InitializeParams,
    InitializeResult, Location, NumberOrString, OneOf, Position, PositionEncodingKind, Range,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, Uri,
    notification::{
        Cancel, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument,
        DidOpenTextDocument, Exit, Initialized, Notification as _,
    },
    request::{GotoDefinition, GotoTypeDefinition, Initialize, Request as _, Shutdown},
};
use serde_json::Value;
use url::Url;

use semantic::{DefinitionKind, DefinitionProgress, Engine};
use workspace::{Workspace, WorkspaceLoad, path_key};

enum State {
    Starting,
    Running(Box<Running>),
    Shutdown,
}

struct DefinitionRequest {
    id: RequestId,
    kind: DefinitionKind,
    path: PathBuf,
    position: Position,
}

struct WorkspaceResult {
    load: WorkspaceLoad,
    result: Result<Workspace>,
}

struct Running {
    engine: Engine,
    watch_files: bool,
    pending: Vec<DefinitionRequest>,
    loading: Option<WorkspaceLoad>,
    requested: Option<WorkspaceLoad>,
}

impl Running {
    fn request_workspace(&mut self, load: WorkspaceLoad) {
        if self.loading != Some(load) {
            self.requested = Some(load);
        }
    }

    fn start_workspace_load(&mut self, results: &Sender<WorkspaceResult>) -> Result<()> {
        if self.loading.is_some() {
            return Ok(());
        }
        let Some(load) = self.requested.take() else {
            return Ok(());
        };

        let root = self.engine.root().to_owned();
        let results = results.clone();
        thread::Builder::new()
            .name("go-package-loader".into())
            .spawn(move || {
                let result = Workspace::load(&root, load.include_tests);
                let _ = results.send(WorkspaceResult { load, result });
            })
            .context("starting Go package loader")?;
        self.loading = Some(load);
        Ok(())
    }

    fn workspace_loaded(&mut self, connection: &Connection, update: WorkspaceResult) -> Result<()> {
        self.loading = None;
        match update.result {
            Ok(workspace) => {
                if self.engine.install_workspace(update.load, workspace) {
                    for request in std::mem::take(&mut self.pending) {
                        self.resolve_definition(connection, request)?;
                    }
                }
            }
            Err(error) if update.load == self.engine.workspace_load() => {
                let message = format!("{error:#}");
                if self.pending.is_empty() {
                    eprintln!("rs-gopls: loading Go packages: {message}");
                }
                for request in self.pending.drain(..) {
                    send_response(
                        connection,
                        request.id,
                        Err((ErrorCode::RequestFailed, message.clone())),
                    )?;
                }
            }
            Err(_) => {}
        }
        if !self.pending.is_empty() {
            self.request_workspace(self.engine.workspace_load());
        }
        Ok(())
    }

    fn resolve_definition(
        &mut self,
        connection: &Connection,
        request: DefinitionRequest,
    ) -> Result<()> {
        let progress = self
            .engine
            .definition(&request.path, request.position, request.kind);
        let result = match progress {
            Ok(DefinitionProgress::Pending(load)) => {
                self.pending.push(request);
                self.request_workspace(load);
                return Ok(());
            }
            Ok(DefinitionProgress::Ready(location)) => serialize_location(location),
            Err(error) => Err((ErrorCode::InternalError, format!("{error:#}"))),
        };
        send_response(connection, request.id, result)
    }

    fn cancel_definitions(
        &mut self,
        connection: &Connection,
        matches: impl FnMut(&mut DefinitionRequest) -> bool,
        code: ErrorCode,
        message: &str,
    ) -> Result<()> {
        for request in self.pending.extract_if(.., matches) {
            send_response(connection, request.id, Err((code, message.to_owned())))?;
        }
        Ok(())
    }

    fn document_modified(&mut self, connection: &Connection, path: &Path) -> Result<()> {
        let key = path_key(path);
        self.cancel_definitions(
            connection,
            |request| path_key(&request.path) == key,
            ErrorCode::ContentModified,
            "the document changed while loading Go packages",
        )
    }
}
type RequestResult = std::result::Result<Value, (ErrorCode, String)>;

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
    loop {
        select_biased! {
            recv(connection.receiver) -> message => {
                let Ok(message) = message else {
                    break;
                };
                match message {
                    Message::Request(request) => handle_request(connection, &mut state, request)?,
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
        }
        if let State::Running(running) = &mut state {
            running.start_workspace_load(&load_sender)?;
        }
    }
    Ok(())
}

fn handle_request(connection: &Connection, state: &mut State, request: Request) -> Result<()> {
    let Request { id, method, params } = request;
    let result: RequestResult = match (state, method.as_str()) {
        (state @ State::Starting, Initialize::METHOD) => initialize(params)
            .map(|(engine, capabilities, watch_files)| {
                *state = State::Running(Box::new(Running {
                    engine,
                    watch_files,
                    pending: Vec::new(),
                    loading: None,
                    requested: None,
                }));
                capabilities
            })
            .map_err(|error| (ErrorCode::InvalidParams, format!("{error:#}"))),
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
                running.cancel_definitions(
                    connection,
                    |_| true,
                    ErrorCode::RequestFailed,
                    "the server is shutting down",
                )?;
            }
            *state = State::Shutdown;
            Ok(Value::Null)
        }
        (State::Running(running), GotoDefinition::METHOD | GotoTypeDefinition::METHOD) => {
            match definition_position(params) {
                Ok((path, position)) => {
                    let kind = if method == GotoDefinition::METHOD {
                        DefinitionKind::Declaration
                    } else {
                        DefinitionKind::Type
                    };
                    return running.resolve_definition(
                        connection,
                        DefinitionRequest {
                            id,
                            kind,
                            path,
                            position,
                        },
                    );
                }
                Err(error) => Err((ErrorCode::InvalidParams, format!("{error:#}"))),
            }
        }
        (State::Running(..), _) => Err((
            ErrorCode::MethodNotFound,
            format!("unsupported request: {method}"),
        )),
    };

    send_response(connection, id, result)
}

fn send_response(connection: &Connection, id: RequestId, result: RequestResult) -> Result<()> {
    let response = match result {
        Ok(value) => Response::new_ok(id, value),
        Err((code, message)) => Response::new_err(id, code as i32, message),
    };
    connection
        .sender
        .send(response.into())
        .context("sending LSP response")
}
fn initialize(params: Value) -> Result<(Engine, Value, bool)> {
    let params = serde_json::from_value::<InitializeParams>(params)
        .context("invalid initialize parameters")?;
    let watch_files = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .is_some_and(|watches| watches.dynamic_registration == Some(true));
    let root = workspace_root(params)?;
    ensure!(
        root.is_dir(),
        "workspace is not a directory: {}",
        root.display()
    );

    let capabilities = ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                ..Default::default()
            },
        )),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(true.into()),
        ..Default::default()
    };
    let result = serde_json::to_value(InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    })?;
    Ok((Engine::new(root), result, watch_files))
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

fn definition_position(params: Value) -> Result<(PathBuf, Position)> {
    let params: GotoDefinitionParams = serde_json::from_value(params)?;
    let document = params.text_document_position_params;
    Ok((file_path(&document.text_document.uri)?, document.position))
}

fn serialize_location(location: Option<(PathBuf, Range)>) -> RequestResult {
    let location = (|| {
        let Some((path, range)) = location else {
            return Ok(Value::Null);
        };
        let uri = path_uri(&path)?;
        Ok::<_, anyhow::Error>(serde_json::to_value(Location::new(uri, range))?)
    })();
    location.map_err(|error| (ErrorCode::InternalError, format!("{error:#}")))
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
                        running.document_modified(connection, &path)?;
                    }
                }
            }
            let after = running.engine.workspace_load();
            if after != before {
                running.request_workspace(after);
            }
            Ok(())
        }
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(params)?;
            let document = params.text_document;
            let path = file_path(&document.uri)?;
            running.engine.open(path.clone(), document.text)?;
            running.document_modified(connection, &path)
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            running.engine.edit(&path, &params.content_changes)?;
            running.document_modified(connection, &path)
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            running.engine.close(&path);
            running.document_modified(connection, &path)
        }
        Cancel::METHOD => {
            let params: CancelParams = serde_json::from_value(params)?;
            let id = match params.id {
                NumberOrString::Number(value) => RequestId::from(value),
                NumberOrString::String(value) => RequestId::from(value),
            };
            running.cancel_definitions(
                connection,
                |request| request.id == id,
                ErrorCode::RequestCanceled,
                "the client cancelled the definition request",
            )
        }
        _ => Ok(()),
    }
}
fn file_path(uri: &Uri) -> Result<PathBuf> {
    Url::parse(uri.as_str())?
        .to_file_path()
        .map_err(|()| anyhow!("expected a file URI, got {}", uri.as_str()))
}

fn path_uri(path: &Path) -> Result<Uri> {
    let url = Url::from_file_path(path)
        .map_err(|()| anyhow!("cannot convert {} to a file URI", path.display()))?;
    url.as_str().parse().context("parsing definition URI")
}
