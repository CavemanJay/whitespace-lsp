use lsp_server::{Connection, ExtractError, Message, RequestId, Response};
use lsp_types::{
    request::{DocumentHighlightRequest, HoverRequest, InlayHintRequest, Request},
    DocumentHighlight, DocumentHighlightKind, HoverContents, HoverProviderCapability,
    InitializeParams, InlayHint, InlayHintKind, InlayHintLabel, MarkedString, OneOf, Position,
    Range, SemanticTokenType, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentPositionParams,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use std::error::Error;
use tree_sitter::{Node, Point, Query};
use whitespace::{
    parse::tree_sitter::{tokenize, NodeIterator, IGNORED_RULES},
    to_visible,
    tokens::{FlowControlOp, Label, Num},
};

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // Note that we must have our logging only write out to stderr.
    eprintln!("starting generic LSP server");

    // Create the transport. Includes the stdio (stdin and stdout) versions but this could
    // also be implemented to use sockets or HTTP.
    let (connection, io_threads) = Connection::stdio();

    // Run the server and wait for the two threads to end (typically by trigger LSP Exit event).
    let server_capabilities = serde_json::to_value(ServerCapabilities {
        // definition_provider: Some(OneOf::Left(true)),
        // inline_value_provider
        inlay_hint_provider: Some(OneOf::Left(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: vec![SemanticTokenType::KEYWORD],
                    token_modifiers: vec![],
                },
                range: None,
                full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                work_done_progress_options: Default::default(),
            },
        )),
        ..Default::default()
    })
    .unwrap();
    let initialization_params = connection.initialize(server_capabilities)?;
    main_loop(connection, initialization_params)?;
    io_threads.join()?;

    // Shut down gracefully.
    eprintln!("shutting down server");
    Ok(())
}

fn main_loop(
    connection: Connection,
    params: serde_json::Value,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let _params: InitializeParams = serde_json::from_value(params).unwrap();
    eprintln!("starting main loop");
    for msg in &connection.receiver {
        eprintln!("got msg: {msg:?}");
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                match req.method.as_str() {
                    HoverRequest::METHOD => {
                        let (id, params) = cast::<HoverRequest>(req)?;
                        eprintln!("got Hover request #{id}: {params:?}");
                        let doc_params = &params.text_document_position_params;
                        let source = read_file(doc_params);
                        let tree = tokenize(&source);
                        let source_file = tree.root_node();
                        let pos = doc_params.position;
                        let point = Point::new(pos.line as usize, pos.character as usize);
                        let mut node = source_file
                            .descendant_for_point_range(point, point)
                            .unwrap();
                        while IGNORED_RULES.contains(&node.kind()) {
                            node = node.parent().unwrap();
                        }
                        eprintln!("descendant: {:#?}", node);
                        if node.kind() == "source_file" {
                            continue;
                        }
                        // eprintln!("tree: {:#?}", tree);
                        // eprintln!("source file: {:#?}", source_file);
                        let contents = match node.kind() {
                            "num" => {
                                let num: Num = node.try_into().unwrap();
                                // eprintln!(
                                //     "num: {:#?} -> {}",
                                //     num,
                                //     to_visible(node.utf8_text(source.as_bytes()).unwrap())
                                // );
                                num.to_string()
                            }
                            "label" => {
                                let label: Label = node.try_into().unwrap();
                                // eprintln!(
                                //     "num: {:#?} -> {}",
                                //     num,
                                //     to_visible(node.utf8_text(source.as_bytes()).unwrap())
                                // );
                                format!("{label:?}")
                            }
                            _ => node.kind().to_string(),
                        };
                        // let contents = "".to_string();
                        let response_params = lsp_types::Hover {
                            contents: HoverContents::Scalar(MarkedString::String(contents)),
                            range: Some(Range {
                                start: node.start_position().to_lsp_pos(),
                                end: node.end_position().to_lsp_pos(),
                            }),
                            // range: None,
                        };
                        let result = Some(serde_json::to_value(response_params).unwrap());

                        let resp = Response {
                            id,
                            result,
                            error: None,
                        };
                        connection.sender.send(Message::Response(resp))?;
                        continue;
                    }
                    DocumentHighlightRequest::METHOD => {
                        let (id, params) = cast::<DocumentHighlightRequest>(req)?;
                        eprintln!("got DocumentHighlight request #{id}: {params:?}");
                        let tree = lex_file(&params.text_document_position_params);
                        let source_file = tree.root_node();
                        let mut cursor = source_file.walk();
                        let highlights = source_file
                            .children(&mut cursor)
                            .map(|node| node.to_document_highlight())
                            .collect::<Vec<_>>();
                        let result = Some(serde_json::to_value(highlights).unwrap());
                        let resp = Response {
                            id,
                            result,
                            error: None,
                        };
                        connection.sender.send(Message::Response(resp))?;
                        continue;
                    }
                    InlayHintRequest::METHOD => {
                        let (id, params) = cast::<InlayHintRequest>(req)?;
                        eprintln!("got InlayHint request #{id}: {params:?}");

                        let path = params.text_document.uri.to_file_path().unwrap();
                        let source = std::fs::read_to_string(path).unwrap();
                        let ast = whitespace::parse::tree_sitter::parse(&source).unwrap();
                        let flows = ast.flow_control_ops(&source);
                        let hints = flows
                            .iter()
                            .map(|(n, op)| {
                                eprintln!("op: {:#?}", op);
                                // let hint = match op {
                                //     FlowControlOp::Label(label) => label.name(),
                                //     _ => "".to_string(),
                                // };
                                InlayHint {
                                    // label: InlayHintLabel::String(format!("{:#?}", op)),
                                    label: InlayHintLabel::String(
                                        format!("{}", op).replace("label ", ""),
                                    ),
                                    kind: Some(InlayHintKind::TYPE),
                                    position: n.end_position().to_lsp_pos(),
                                    text_edits: None,
                                    tooltip: None,
                                    padding_left: None,
                                    padding_right: None,
                                    data: None,
                                }
                            })
                            .collect::<Vec<_>>();

                        // let hints: Vec<InlayHint> = vec![InlayHint {
                        //     label: InlayHintLabel::String("hello".to_string()),
                        //     kind: Some(InlayHintKind::TYPE),
                        //     position: Position {
                        //         line: 0,
                        //         character: 0,
                        //     },
                        //     text_edits: None,
                        //     tooltip: None,
                        //     padding_left: None,
                        //     padding_right: None,
                        //     data: None,
                        // }];
                        let result = Some(serde_json::to_value(hints).unwrap());

                        let resp = Response {
                            id,
                            result,
                            error: None,
                        };
                        connection.sender.send(Message::Response(resp))?;
                        continue;
                    }
                    _ => {
                        eprintln!("got unknown request: {req:?}");
                    }
                }
                // match cast::<GotoDefinition>(req) {
                //     Ok((id, params)) => {
                //         eprintln!("got gotoDefinition request #{id}: {params:?}");
                //         let result = Some(GotoDefinitionResponse::Array(Vec::new()));
                //         let result = serde_json::to_value(&result).unwrap();
                //         let resp = Response {
                //             id,
                //             result: Some(result),
                //             error: None,
                //         };
                //         connection.sender.send(Message::Response(resp))?;
                //         continue;
                //     }
                //     Err(err @ ExtractError::JsonError { .. }) => panic!("{err:?}"),
                //     Err(ExtractError::MethodMismatch(req)) => req,
                // };
                // ...
            }
            Message::Response(resp) => {
                eprintln!("got response: {resp:?}");
            }
            Message::Notification(not) => {
                eprintln!("got notification: {not:?}");
            }
        }
    }
    Ok(())
}

fn lex_file(params: &TextDocumentPositionParams) -> tree_sitter::Tree {
    let file = read_file(params);
    let src = file.as_str();
    tokenize(src)
}

fn read_file(params: &TextDocumentPositionParams) -> String {
    let path = params.text_document.uri.to_file_path().unwrap();
    let file = std::fs::read_to_string(path).unwrap();
    file
}

fn cast<R>(
    req: lsp_server::Request,
) -> Result<(RequestId, R::Params), ExtractError<lsp_server::Request>>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    req.extract(R::METHOD)
}

trait RangeExt {
    fn to_ts_point(&self) -> tree_sitter::Point;
    fn to_lsp_pos(&self) -> lsp_types::Position;
}

impl RangeExt for tree_sitter::Point {
    fn to_ts_point(&self) -> tree_sitter::Point {
        *self
    }

    fn to_lsp_pos(&self) -> lsp_types::Position {
        Position {
            line: self.row as u32,
            character: self.column as u32,
        }
    }
}

trait HighlightExt {
    fn to_document_highlight(&self) -> DocumentHighlight;
}

impl HighlightExt for Node<'_> {
    fn to_document_highlight(&self) -> DocumentHighlight {
        let kind = match self.kind() {
            n if n.starts_with("op") => DocumentHighlightKind::READ,
            "num" => DocumentHighlightKind::WRITE,
            _ => DocumentHighlightKind::TEXT,
        };
        DocumentHighlight {
            range: Range {
                start: self.start_position().to_lsp_pos(),
                end: self.end_position().to_lsp_pos(),
            },
            kind: Some(kind),
        }
    }
}
