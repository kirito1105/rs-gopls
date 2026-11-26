use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_server::{Connection, ErrorCode, Request, RequestId};
use lsp_types::{
    CompletionItem, CompletionOptions, CompletionOptionsCompletionItem, CompletionParams,
    DocumentSymbolParams, HoverProviderCapability, OneOf, Position, ReferenceParams,
    ServerCapabilities, SignatureHelpOptions, TextDocumentPositionParams, WorkspaceSymbolParams,
    request::{
        Completion, DocumentSymbolRequest, GotoDefinition, GotoImplementation, GotoTypeDefinition,
        HoverRequest, References, Request as _, ResolveCompletionItem, SignatureHelpRequest,
        WorkspaceSymbolRequest,
    },
};
use serde_json::Value;

use crate::protocol::{
    RequestResult, file_path, internal_error, invalid_params, send_response, serialize_location,
    serialize_locations, serialize_query,
};
use crate::scheduler::{DeferredQuery, RequestProgress, Scheduler, SearchTask};
use crate::semantic::completion::{
    CompletionClientCapabilities, ResolveCompletionResult, StaleCompletion,
};
use crate::semantic::{DefinitionKind, Engine, QueryProgress, SemanticTask};

type Handler = fn(&mut RequestContext<'_>, RequestId, Value) -> Result<()>;

struct Feature {
    capabilities: fn(&mut ServerCapabilities),
    methods: &'static [(&'static str, Handler)],
}

const FEATURES: &[Feature] = &[
    Feature {
        capabilities: |caps| {
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
        capabilities: |caps| {
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
        capabilities: |caps| {
            caps.document_symbol_provider = Some(OneOf::Left(true));
            caps.workspace_symbol_provider = Some(OneOf::Left(true));
        },
        methods: &[
            (DocumentSymbolRequest::METHOD, document_symbols),
            (WorkspaceSymbolRequest::METHOD, workspace_symbols),
        ],
    },
    Feature {
        capabilities: |caps| {
            caps.completion_provider = Some(CompletionOptions {
                resolve_provider: Some(true),
                trigger_characters: Some(vec![".".into()]),
                completion_item: Some(CompletionOptionsCompletionItem {
                    label_details_support: Some(true),
                }),
                ..CompletionOptions::default()
            });
        },
        methods: &[
            (Completion::METHOD, completion),
            (ResolveCompletionItem::METHOD, resolve_completion),
        ],
    },
];

pub(super) fn write_capabilities(capabilities: &mut ServerCapabilities) {
    for feature in FEATURES {
        (feature.capabilities)(capabilities);
    }
}

pub(super) fn handle(
    connection: &Connection,
    engine: &mut Engine,
    scheduler: &mut Scheduler,
    hierarchical_document_symbols: bool,
    completion_capabilities: CompletionClientCapabilities,
    request: Request,
) -> Result<()> {
    let Request { id, method, params } = request;
    let mut context = RequestContext {
        connection,
        engine,
        scheduler,
        hierarchical_document_symbols,
        completion_capabilities,
    };
    for &(name, handler) in FEATURES.iter().flat_map(|feature| feature.methods) {
        if method == name {
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
    completion_capabilities: CompletionClientCapabilities,
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
        QueryProgress::Searching(task) => QueryProgress::Searching(Box::new(EncodedTask {
            task,
            encode,
            stale: false,
        })),
    }
}

struct EncodedTask<T, Task> {
    task: Task,
    encode: fn(T) -> RequestResult,
    stale: bool,
}

impl<T, Task: SemanticTask<T>> SearchTask for EncodedTask<T, Task> {
    fn advance(&mut self, engine: &mut Engine) -> Result<bool> {
        match self.task.advance(engine) {
            Err(error) if error.is::<StaleCompletion>() => {
                self.stale = true;
                Ok(true)
            }
            progress => progress,
        }
    }

    fn finish(self: Box<Self>) -> RequestResult {
        if self.stale {
            Err((ErrorCode::ContentModified, StaleCompletion.to_string()))
        } else {
            (self.encode)(self.task.finish())
        }
    }
}

fn document_position(params: Value) -> Result<(PathBuf, Position)> {
    let params: TextDocumentPositionParams = serde_json::from_value(params)?;
    Ok((file_path(&params.text_document.uri)?, params.position))
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

fn completion(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    let caps = context.completion_capabilities;
    let params = (|| {
        let params: CompletionParams = serde_json::from_value(params)?;
        let document = params.text_document_position;
        Ok((
            file_path(&document.text_document.uri)?,
            (document.position, params.context, caps),
        ))
    })();
    context.document_query(
        id,
        params,
        |engine, path, (position, context, caps)| {
            Ok(adapt(
                engine.complete(path, *position, context.clone(), caps)?,
                serialize_query,
            ))
        },
        Some(|engine, path, (position, _, caps)| {
            engine
                .complete_after_load_failure(path, *position, caps)
                .map_err(internal_error)
                .and_then(serialize_query)
        }),
    )
}

fn resolve_completion(
    context: &mut RequestContext<'_>,
    id: RequestId,
    params: Value,
) -> Result<()> {
    let item = serde_json::from_value::<CompletionItem>(params)
        .map_err(|error| invalid_params(error.into()));
    let result = item.and_then(|item| {
        match context
            .engine
            .resolve_completion_item(item, &context.completion_capabilities)
            .map_err(internal_error)?
        {
            ResolveCompletionResult::Item(item) => serialize_query(item),
            ResolveCompletionResult::ContentModified => {
                Err((ErrorCode::ContentModified, StaleCompletion.to_string()))
            }
            ResolveCompletionResult::InvalidData => Err((
                ErrorCode::InvalidParams,
                "invalid completion item data".into(),
            )),
        }
    });
    context.respond(id, result)
}
