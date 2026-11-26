use std::path::{Path, PathBuf};

use anyhow::Result;
use lsp_server::{Connection, ErrorCode, Request, RequestId};
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCallsParams,
    CallHierarchyServerCapability, CodeActionKind, CodeActionOptions, CodeActionParams,
    CodeActionProviderCapability, DiagnosticOptions, DiagnosticServerCapabilities,
    DocumentDiagnosticParams, DocumentDiagnosticReportKind, DocumentFormattingParams,
    DocumentSymbolParams, FullDocumentDiagnosticReport, HoverProviderCapability, OneOf, Position,
    ReferenceParams, ServerCapabilities, SignatureHelpOptions, TextDocumentPositionParams,
    TypeHierarchySubtypesParams, TypeHierarchySupertypesParams, UnchangedDocumentDiagnosticReport,
    Uri, WorkspaceSymbolParams,
    request::{
        CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
        CodeActionRequest, DocumentDiagnosticRequest, DocumentSymbolRequest, Formatting,
        GotoDefinition, GotoImplementation, GotoTypeDefinition, HoverRequest, References,
        Request as _, SignatureHelpRequest, TypeHierarchyPrepare, TypeHierarchySubtypes,
        TypeHierarchySupertypes, WorkspaceSymbolRequest,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::protocol::{
    RequestResult, file_path, internal_error, invalid_params, send_response, serialize_location,
    serialize_locations, serialize_query,
};
use crate::scheduler::{DeferredQuery, RequestProgress, Scheduler, SearchTask};
use crate::semantic::diagnostics::DiagnosticReport;
use crate::semantic::{DefinitionKind, Engine, QueryProgress, SemanticTask};

type Handler = fn(&mut RequestContext<'_>, RequestId, Value) -> Result<()>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    #[serde(flatten)]
    standard: ServerCapabilities,
    // lsp-types 0.97 supplies the type hierarchy requests but omits this capability.
    type_hierarchy_provider: bool,
}

struct Feature {
    capabilities: fn(&mut Capabilities),
    methods: &'static [(&'static str, Handler)],
}

const FEATURES: &[Feature] = &[
    Feature {
        capabilities: |caps| {
            caps.standard.definition_provider = Some(OneOf::Left(true));
            caps.standard.type_definition_provider = Some(true.into());
            caps.standard.implementation_provider = Some(true.into());
            caps.standard.references_provider = Some(OneOf::Left(true));
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
            caps.standard.hover_provider = Some(HoverProviderCapability::Simple(true));
            caps.standard.signature_help_provider = Some(SignatureHelpOptions {
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
            caps.standard.document_symbol_provider = Some(OneOf::Left(true));
            caps.standard.workspace_symbol_provider = Some(OneOf::Left(true));
        },
        methods: &[
            (DocumentSymbolRequest::METHOD, document_symbols),
            (WorkspaceSymbolRequest::METHOD, workspace_symbols),
        ],
    },
    Feature {
        capabilities: |caps| {
            caps.standard.diagnostic_provider =
                Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                    identifier: Some("rs-gopls".into()),
                    inter_file_dependencies: true,
                    workspace_diagnostics: false,
                    ..Default::default()
                }));
        },
        methods: &[(DocumentDiagnosticRequest::METHOD, document_diagnostics)],
    },
    Feature {
        capabilities: |caps| {
            caps.standard.call_hierarchy_provider =
                Some(CallHierarchyServerCapability::Simple(true));
            caps.type_hierarchy_provider = true;
        },
        methods: &[
            (CallHierarchyPrepare::METHOD, prepare_call_hierarchy),
            (CallHierarchyIncomingCalls::METHOD, incoming_calls),
            (CallHierarchyOutgoingCalls::METHOD, outgoing_calls),
            (TypeHierarchyPrepare::METHOD, prepare_type_hierarchy),
            (TypeHierarchySupertypes::METHOD, supertypes),
            (TypeHierarchySubtypes::METHOD, subtypes),
        ],
    },
    Feature {
        capabilities: |caps| {
            caps.standard.document_formatting_provider = Some(OneOf::Left(true));
            caps.standard.code_action_provider =
                Some(CodeActionProviderCapability::Options(CodeActionOptions {
                    code_action_kinds: Some(vec![
                        CodeActionKind::QUICKFIX,
                        CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                    ]),
                    ..Default::default()
                }));
        },
        methods: &[
            (Formatting::METHOD, formatting),
            (CodeActionRequest::METHOD, code_actions),
        ],
    },
];

pub(super) fn capabilities(standard: ServerCapabilities) -> impl Serialize {
    let mut capabilities = Capabilities {
        standard,
        type_hierarchy_provider: false,
    };
    for feature in FEATURES {
        (feature.capabilities)(&mut capabilities);
    }
    capabilities
}

pub(super) fn handle(
    connection: &Connection,
    engine: &mut Engine,
    scheduler: &mut Scheduler,
    hierarchical_document_symbols: bool,
    request: Request,
) -> Result<()> {
    let Request { id, method, params } = request;
    let mut context = RequestContext {
        connection,
        engine,
        scheduler,
        hierarchical_document_symbols,
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

fn document_params<P: DeserializeOwned>(
    value: Value,
    uri: impl FnOnce(&P) -> &Uri,
) -> Result<(PathBuf, P)> {
    let params = serde_json::from_value(value)?;
    Ok((file_path(uri(&params))?, params))
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

fn diagnostic_result(report: &DiagnosticReport, previous: Option<&str>) -> RequestResult {
    let result = if previous == Some(&report.result_id) {
        DocumentDiagnosticReportKind::Unchanged(UnchangedDocumentDiagnosticReport {
            result_id: report.result_id.clone(),
        })
    } else {
        DocumentDiagnosticReportKind::Full(FullDocumentDiagnosticReport {
            result_id: Some(report.result_id.clone()),
            items: report.to_lsp_diagnostics(),
        })
    };
    serialize_query(result)
}

fn document_diagnostics(
    context: &mut RequestContext<'_>,
    id: RequestId,
    params: Value,
) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &DocumentDiagnosticParams| &p.text_document.uri),
        |engine, path, params| {
            let (report, load) = engine.diagnostics_report(path)?;
            Ok(match load {
                Some(load) => QueryProgress::Pending(load),
                None => QueryProgress::Ready(diagnostic_result(
                    report,
                    params.previous_result_id.as_deref(),
                )),
            })
        },
        Some(|engine, path, _| {
            let (report, _) = engine.diagnostics_report(path).map_err(internal_error)?;
            diagnostic_result(report, None)
        }),
    )
}

fn formatting(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &DocumentFormattingParams| &p.text_document.uri),
        |engine, path, _| {
            Ok(QueryProgress::Ready(serialize_query(
                engine.format_document(path)?,
            )))
        },
        None,
    )
}

fn code_actions(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &CodeActionParams| &p.text_document.uri),
        |engine, path, p| {
            Ok(adapt(
                engine.code_actions(
                    path,
                    &p.text_document.uri,
                    p.range,
                    p.context.only.as_deref(),
                )?,
                serialize_query,
            ))
        },
        Some(|engine, path, p| {
            engine
                .local_code_actions(
                    path,
                    &p.text_document.uri,
                    p.range,
                    p.context.only.as_deref(),
                )
                .map_err(internal_error)
                .and_then(serialize_query)
        }),
    )
}

fn prepare_call_hierarchy(
    context: &mut RequestContext<'_>,
    id: RequestId,
    params: Value,
) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &pos| {
            Ok(adapt(
                engine.prepare_call_hierarchy(path, pos)?,
                serialize_query,
            ))
        },
        None,
    )
}

fn incoming_calls(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &CallHierarchyIncomingCallsParams| &p.item.uri),
        |engine, _, p| Ok(adapt(engine.incoming_calls(&p.item)?, serialize_query)),
        None,
    )
}

fn outgoing_calls(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &CallHierarchyOutgoingCallsParams| &p.item.uri),
        |engine, _, p| Ok(adapt(engine.outgoing_calls(&p.item)?, serialize_query)),
        None,
    )
}

fn prepare_type_hierarchy(
    context: &mut RequestContext<'_>,
    id: RequestId,
    params: Value,
) -> Result<()> {
    context.document_query(
        id,
        document_position(params),
        |engine, path, &pos| {
            Ok(adapt(
                engine.prepare_type_hierarchy(path, pos)?,
                serialize_query,
            ))
        },
        None,
    )
}

fn supertypes(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &TypeHierarchySupertypesParams| &p.item.uri),
        |engine, _, p| Ok(adapt(engine.supertypes(&p.item)?, serialize_query)),
        None,
    )
}

fn subtypes(context: &mut RequestContext<'_>, id: RequestId, params: Value) -> Result<()> {
    context.document_query(
        id,
        document_params(params, |p: &TypeHierarchySubtypesParams| &p.item.uri),
        |engine, _, p| Ok(adapt(engine.subtypes(&p.item)?, serialize_query)),
        None,
    )
}
