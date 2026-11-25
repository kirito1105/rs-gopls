mod scope;
mod semantic;
mod symbols;
mod workspace;
mod workspace_roots;

use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionParams, InitializeParams, InitializeResult, Location,
    OneOf, PositionEncodingKind, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, Uri,
    notification::{
        DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
        Exit, Initialized, Notification as _,
    },
    request::{GotoDefinition, Initialize, Request as _, Shutdown},
};
use serde_json::Value;
use url::Url;

use semantic::Engine;

enum State {
    Starting,
    Running(Box<Engine>, bool),
    Shutdown,
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
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                let response = handle_request(&mut state, request);
                connection
                    .sender
                    .send(response.into())
                    .context("sending LSP response")?;
            }
            Message::Notification(notification) if notification.method == Exit::METHOD => {
                ensure!(
                    matches!(state, State::Shutdown),
                    "exit arrived before shutdown"
                );
                break;
            }
            Message::Notification(notification) => {
                if let State::Running(engine, watch_files) = &mut state {
                    let Notification { method, params } = notification;
                    if method == Initialized::METHOD && std::mem::take(watch_files) {
                        register_file_watches(connection)?;
                    } else if let Err(error) = handle_notification(engine, &method, params) {
                        eprintln!("rs-gopls: {method}: {error:#}");
                    }
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

fn handle_request(state: &mut State, request: Request) -> Response {
    let Request { id, method, params } = request;
    let result: RequestResult = match (state, method.as_str()) {
        (state @ State::Starting, Initialize::METHOD) => initialize(params)
            .map(|(engine, capabilities, watch_files)| {
                *state = State::Running(Box::new(engine), watch_files);
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
            *state = State::Shutdown;
            Ok(Value::Null)
        }
        (State::Running(engine, _), GotoDefinition::METHOD) => definition(engine, params),
        (State::Running(..), _) => Err((
            ErrorCode::MethodNotFound,
            format!("unsupported request: {method}"),
        )),
    };

    match result {
        Ok(value) => Response::new_ok(id, value),
        Err((code, message)) => Response::new_err(id, code as i32, message),
    }
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

fn definition(engine: &mut Engine, params: Value) -> RequestResult {
    let (path, position) = (|| {
        let params: GotoDefinitionParams = serde_json::from_value(params)?;
        let document = params.text_document_position_params;
        Ok::<_, anyhow::Error>((file_path(&document.text_document.uri)?, document.position))
    })()
    .map_err(|error| (ErrorCode::InvalidParams, format!("{error:#}")))?;

    let location = (|| {
        let Some((path, range)) = engine.definition(&path, position)? else {
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

fn handle_notification(engine: &mut Engine, method: &str, params: Value) -> Result<()> {
    match method {
        DidChangeWatchedFiles::METHOD => {
            let params: DidChangeWatchedFilesParams = serde_json::from_value(params)?;
            for change in params.changes {
                if let Ok(path) = file_path(&change.uri) {
                    engine.file_changed(&path, change.typ);
                }
            }
            Ok(())
        }
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(params)?;
            let document = params.text_document;
            engine.open(file_path(&document.uri)?, document.text)
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(params)?;
            let path = file_path(&params.text_document.uri)?;
            engine.edit(&path, &params.content_changes)
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(params)?;
            engine.close(&file_path(&params.text_document.uri)?);
            Ok(())
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
