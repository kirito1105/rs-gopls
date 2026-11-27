use std::cell::OnceCell;
use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_server::{Connection, ErrorCode, Request, RequestId};
use lsp_types::{
    ClientCapabilities, DocumentSymbolParams, HoverProviderCapability, OneOf, Position,
    ReferenceParams, RenameOptions, RenameParams, ServerCapabilities, SignatureHelpOptions,
    TextDocumentPositionParams, WorkspaceSymbolParams,
    request::{
        DocumentSymbolRequest, GotoDefinition, GotoImplementation, GotoTypeDefinition,
        HoverRequest, PrepareRenameRequest, References, Rename, Request as _, SignatureHelpRequest,
        WorkspaceSymbolRequest,
    },
};
use serde_json::Value;

use crate::protocol::{
    RequestResult, file_path, internal_error, invalid_params, send_response, serialize_location,
    serialize_locations, serialize_query,
};
use crate::scheduler::{DeferredQuery, RequestProgress, Scheduler, SearchTask};
use crate::semantic::rename::{RenameError, RenameResult};
use crate::semantic::{DefinitionKind, Engine, QueryProgress, SemanticTask};

type Handler = fn(&mut RequestContext<'_>, RequestId, Value) -> Result<()>;

#[derive(Clone, Copy)]
pub(super) struct ClientFeatures {
    hierarchical_document_symbols: bool,
    rename: RenameSupport,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenameSupport {
    Unavailable,
    Direct,
    WithPrepare,
}

impl ClientFeatures {
    pub(super) fn from_capabilities(caps: &ClientCapabilities) -> Self {
        let document = caps.text_document.as_ref();
        let versioned_edits = caps
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.workspace_edit.as_ref())
            .and_then(|edit| edit.document_changes)
            == Some(true);
        let prepare = document
            .and_then(|text| text.rename.as_ref())
            .and_then(|rename| rename.prepare_support)
            == Some(true);
        Self {
            hierarchical_document_symbols: document
                .and_then(|text| text.document_symbol.as_ref())
                .and_then(|symbols| symbols.hierarchical_document_symbol_support)
                == Some(true),
            rename: match (versioned_edits, prepare) {
                (false, _) => RenameSupport::Unavailable,
                (true, false) => RenameSupport::Direct,
                (true, true) => RenameSupport::WithPrepare,
            },
        }
    }

    fn supports(self, method: &str) -> bool {
        match method {
            Rename::METHOD => self.rename != RenameSupport::Unavailable,
            PrepareRenameRequest::METHOD => self.rename == RenameSupport::WithPrepare,
            _ => true,
        }
    }
}

struct Feature {
    capabilities: fn(&mut ServerCapabilities, ClientFeatures),
    methods: &'static [(&'static str, Handler)],
}

const FEATURES: &[Feature] = &[
    Feature {
        capabilities: |caps, _| {
            caps.definition_provider = Some(OneOf::Left(true));
            caps.type_definition_provider = Some(true.into());
            caps.implementation_provider = Some(true.into());
            caps.references_provider = Some(OneOf::Left(true));
        },
        methods: &[
            (GotoDefinition::METHOD, definition),
            (GotoTypeDefinition::METHOD, type_definition),
            (GotoImplementation::METHOD, implementation),
            (References::METHOD, references),
        ],
    },
    Feature {
        capabilities: |caps, _| {
            caps.hover_provider = Some(HoverProviderCapability::Simple(true));
            caps.signature_help_provider = Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".into(), ",".into()]),
                retrigger_characters: Some(vec![",".into()]),
                work_done_progress_options: Default::default(),
            });
        },
        methods: &[
            (HoverRequest::METHOD, hover),
            (SignatureHelpRequest::METHOD, signature_help),
        ],
    },
    Feature {
        capabilities: |caps, _| {
            caps.document_symbol_provider = Some(OneOf::Left(true));
            caps.workspace_symbol_provider = Some(OneOf::Left(true));
        },
        methods: &[
            (DocumentSymbolRequest::METHOD, document_symbols),
            (WorkspaceSymbolRequest::METHOD, workspace_symbols),
        ],
    },
    Feature {
        capabilities: |caps, client| {
            caps.rename_provider = match client.rename {
                RenameSupport::Unavailable => None,
                RenameSupport::Direct => Some(OneOf::Left(true)),
                RenameSupport::WithPrepare => Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
            };
        },
        methods: &[
            (PrepareRenameRequest::METHOD, prepare_rename),
            (Rename::METHOD, rename),
        ],
    },
];

pub(super) fn write_capabilities(capabilities: &mut ServerCapabilities, client: ClientFeatures) {
    for feature in FEATURES {
        (feature.capabilities)(capabilities, client);
    }
}

pub(super) fn handle(
    connection: &Connection,
    engine: &mut Engine,
    scheduler: &mut Scheduler,
    client: ClientFeatures,
    request: Request,
) -> Result<()> {
    let Request { id, method, params } = request;
    let mut context = RequestContext {
        connection,
        engine,
        scheduler,
        hierarchical_document_symbols: client.hierarchical_document_symbols,
    };
    for &(name, handler) in FEATURES.iter().flat_map(|feature| feature.methods) {
        if method == name && client.supports(name) {
            return handler(&mut context, id, params);
        }
    }
    context.respond(
        id,
        Err((
            ErrorCode::MethodNotFound,
            format!("unsupported request: {method}"),
        )),
    )
}

struct RequestContext<'a> {
    connection: &'a Connection,
    engine: &'a mut Engine,
    scheduler: &'a mut Scheduler,
    hierarchical_document_symbols: bool,
}

impl RequestContext<'_> {
    fn respond(&self, id: RequestId, result: RequestResult) -> Result<()> {
        send_response(self.connection, id, result)
    }

    fn document_query<P: 'static>(
        &mut self,
        id: RequestId,
        params: Result<(PathBuf, P)>,
        run: DocumentHandler<P>,
        fallback: Option<DocumentFallback<P>>,
    ) -> Result<()> {
        match params {
            Ok((path, params)) => self.scheduler.submit(
                self.engine,
                self.connection,
                id,
                DocumentQuery {
                    path,
                    params,
                    run,
                    fallback,
                },
            ),
            Err(error) => self.respond(id, Err(invalid_params(error))),
        }
    }
}

type DocumentHandler<P> = fn(&mut Engine, &Path, &P) -> Result<RequestProgress>;
type DocumentFallback<P> = fn(&mut Engine, &Path, &P) -> RequestResult;

struct DocumentQuery<P> {
    path: PathBuf,
    params: P,
    run: DocumentHandler<P>,
    fallback: Option<DocumentFallback<P>>,
}

impl<P> DeferredQuery for DocumentQuery<P> {
    fn run(&self, engine: &mut Engine) -> Result<RequestProgress> {
        (self.run)(engine, &self.path, &self.params)
    }

    fn document_path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn load_failed(&self, engine: &mut Engine, message: &str) -> RequestResult {
        match self.fallback {
            Some(fallback) => fallback(engine, &self.path, &self.params),
            None => Err((ErrorCode::RequestFailed, message.to_owned())),
        }
    }
}

struct WorkspaceSymbolsQuery(String);

impl DeferredQuery for WorkspaceSymbolsQuery {
    fn run(&self, engine: &mut Engine) -> Result<RequestProgress> {
        Ok(adapt(engine.workspace_symbols(&self.0), serialize_query))
    }
}

fn adapt<T: 'static, Task: SemanticTask<T> + 'static>(
    progress: QueryProgress<T, Task>,
    encode: fn(T) -> RequestResult,
) -> RequestProgress {
    match progress {
        QueryProgress::Ready(result) => QueryProgress::Ready(encode(result)),
        QueryProgress::Pending(load) => QueryProgress::Pending(load),
        QueryProgress::Searching(task) => {
            QueryProgress::Searching(Box::new(EncodedTask { task, encode }))
        }
    }
}

struct EncodedTask<T, Task> {
    task: Task,
    encode: fn(T) -> RequestResult,
}

impl<T, Task: SemanticTask<T>> SearchTask for EncodedTask<T, Task> {
    fn advance(&mut self, engine: &mut Engine) -> Result<bool> {
        self.task.advance(engine)
    }

    fn finish(self: Box<Self>) -> RequestResult {
        (self.encode)(self.task.finish())
    }
}

fn document_position(params: Value) -> Result<(PathBuf, Position)> {
    let params: TextDocumentPositionParams = serde_json::from_value(params)?;
    Ok((file_path(&params.text_document.uri)?, params.position))
}

fn prepare_rename(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &position| {
            Ok(adapt(
                engine.prepare_rename(path, position)?,
                serialize_query,
            ))
        },
        None,
    )
}

struct RenameQuery {
    path: PathBuf,
    position: Position,
    new_name: String,
    disk_source: OnceCell<Option<Vec<u8>>>,
}

impl DeferredQuery for RenameQuery {
    fn run(&self, engine: &mut Engine) -> Result<RequestProgress> {
        // A cold query re-resolves this cursor after go list completes. A client
        // without file watches may have replaced the declaration in the meantime.
        if !engine.is_open(&self.path) && self.path.extension().is_some_and(|ext| ext == "go") {
            let current = std::fs::read(&self.path).ok();
            if let Some(original) = self.disk_source.get() {
                if original != &current {
                    return Ok(QueryProgress::Ready(serialize_rename(Err(
                        RenameError::ContentModified,
                    ))));
                }
            } else {
                self.disk_source.get_or_init(|| current);
            }
        }
        Ok(adapt(
            engine.rename(&self.path, self.position, &self.new_name)?,
            serialize_rename,
        ))
    }

    fn requires_stable_workspace(&self) -> bool {
        true
    }
}

fn serialize_rename(result: RenameResult) -> RequestResult {
    match result {
        Ok(edit) => serialize_query(edit),
        Err(RenameError::Rejected(reason)) => Err((ErrorCode::RequestFailed, reason)),
        Err(RenameError::ContentModified) => Err((
            ErrorCode::ContentModified,
            "a rename input changed during validation".into(),
        )),
    }
}

fn rename(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    let query = (|| -> Result<RenameQuery> {
        let params: RenameParams = serde_json::from_value(params)?;
        let document = params.text_document_position;
        Ok(RenameQuery {
            path: file_path(&document.text_document.uri)?,
            position: document.position,
            new_name: params.new_name,
            disk_source: OnceCell::new(),
        })
    })();
    match query {
        Ok(query) => context
            .scheduler
            .submit(context.engine, context.connection, id, query),
        Err(error) => context.respond(id, Err(invalid_params(error))),
    }
}

fn definition(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    request_definition(context, id, params, DefinitionKind::Declaration)
}

fn type_definition(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    request_definition(context, id, params, DefinitionKind::Type)
}

fn request_definition(
    context: &mut RequestContext<'_>,
    id: RequestId,
    params: Value,
    kind: DefinitionKind,
) -> Result<()> {
    context.document_query(
        id,
        document_position(params).map(|(path, position)| (path, (position, kind))),
        |engine, path, &(position, kind)| {
            Ok(adapt(
                engine.definition(path, position, kind)?,
                serialize_location,
            ))
        },
        None,
    )
}

fn implementation(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &position| {
            Ok(adapt(
                engine.implementations(path, position)?,
                serialize_locations,
            ))
        },
        None,
    )
}

fn references(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    let params = (|| {
        let params: ReferenceParams = serde_json::from_value(params)?;
        let document = params.text_document_position;
        Ok((
            file_path(&document.text_document.uri)?,
            (document.position, params.context.include_declaration),
        ))
    })();
    context.document_query(
        id,
        params,
        |engine, path, &(position, include_declaration)| {
            Ok(adapt(
                engine.references(path, position, include_declaration)?,
                serialize_locations,
            ))
        },
        None,
    )
}

fn hover(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &position| Ok(adapt(engine.hover(path, position)?, serialize_query)),
        Some(|engine, path, &position| {
            engine
                .hover_after_load_failure(path, position)
                .map_err(internal_error)
                .and_then(serialize_query)
        }),
    )
}

fn signature_help(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &position| {
            Ok(adapt(
                engine.signature_help(path, position)?,
                serialize_query,
            ))
        },
        Some(|engine, path, &position| {
            engine
                .signature_help_after_load_failure(path, position)
                .map_err(internal_error)
                .and_then(serialize_query)
        }),
    )
}

fn document_symbols(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    let document = serde_json::from_value::<DocumentSymbolParams>(params)
        .map_err(anyhow::Error::from)
        .and_then(|params| {
            let uri = params.text_document.uri;
            Ok((file_path(&uri)?, uri))
        });
    let result = match document {
        Ok((path, uri)) => context
            .engine
            .document_symbols(&path, &uri, context.hierarchical_document_symbols)
            .map_err(internal_error)
            .and_then(serialize_query),
        Err(error) => Err(invalid_params(error)),
    };
    context.respond(id, result)
}

fn workspace_symbols(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    match serde_json::from_value::<WorkspaceSymbolParams>(params) {
        Ok(params) => context.scheduler.submit(
            context.engine,
            context.connection,
            id,
            WorkspaceSymbolsQuery(params.query),
        ),
        Err(error) => context.respond(id, Err((ErrorCode::InvalidParams, error.to_string()))),
    }
}
