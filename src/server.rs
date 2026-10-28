//! LSP 主循环与请求处理。

use std::collections::HashMap;
use std::error::Error;

use bms_analysis::{Analysis, Severity, SymbolIndex, SymbolKind, channel_doc, header_doc};
use bms_syntax::{SyntaxKind, TextRange, TextSize};
use lsp_server::{
    Connection, ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbolResponse, Documentation, FoldingRange,
    FoldingRangeKind, FoldingRangeParams, FoldingRangeProviderCapability, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability, Location,
    MarkupContent, MarkupKind, NumberOrString, OneOf, PositionEncodingKind,
    PublishDiagnosticsParams, ReferenceParams, SemanticToken, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, SymbolInformation,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri, WorkDoneProgressOptions,
};

use crate::index::LineIndex;

/// 文档状态。
struct Document {
    /// 文档 URI。
    uri: Uri,
    /// 文档文本。
    text: String,
    /// 文档版本。
    version: i32,
    /// 分析结果。
    analysis: Analysis,
    /// 行索引。
    lines: LineIndex,
}

impl Document {
    /// 构建文档状态。
    fn new(uri: Uri, text: String, version: i32) -> Self {
        let analysis = Analysis::from_text(&text);
        let lines = LineIndex::new(&text);
        Self {
            uri,
            text,
            version,
            analysis,
            lines,
        }
    }

    /// 更新全文并重新分析。
    fn update(&mut self, text: String, version: i32) {
        self.text = text;
        self.version = version;
        self.analysis = Analysis::from_text(&self.text);
        self.lines = LineIndex::new(&self.text);
    }

    /// 把字节范围转为 LSP 范围。
    fn range(&self, range: TextRange) -> lsp_types::Range {
        lsp_types::Range {
            start: self.lines.position(&self.text, usize::from(range.start())),
            end: self.lines.position(&self.text, usize::from(range.end())),
        }
    }

    /// 把 LSP 位置转为字节偏移。
    fn offset(&self, position: lsp_types::Position) -> Option<usize> {
        self.lines.offset(&self.text, position)
    }

    /// 返回 LSP 诊断。
    fn diagnostics(&self) -> Vec<Diagnostic> {
        self.analysis
            .diagnostics()
            .iter()
            .map(|diagnostic| Diagnostic {
                range: self.range(diagnostic.range()),
                severity: Some(match diagnostic.severity() {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                    Severity::Information => DiagnosticSeverity::INFORMATION,
                }),
                code: diagnostic
                    .code()
                    .map(|code| NumberOrString::String(code.to_owned())),
                source: Some("bms".to_owned()),
                message: diagnostic.message().to_owned(),
                ..Diagnostic::default()
            })
            .collect()
    }
}

/// 所有打开的文档。
#[derive(Default)]
struct ServerState {
    /// URI -> 文档。
    documents: HashMap<String, Document>,
}

impl ServerState {
    /// 查找文档。
    fn get(&self, uri: &Uri) -> Option<&Document> {
        self.documents.get(&uri.to_string())
    }

    /// 插入或替换文档。
    fn insert(&mut self, uri: &Uri, document: Document) {
        self.documents.insert(uri.to_string(), document);
    }

    /// 删除文档。
    fn remove(&mut self, uri: &Uri) {
        self.documents.remove(&uri.to_string());
    }
}

/// 启动 stdio server。
pub fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let (id, _params) = connection.initialize_start()?;
    connection.initialize_finish(
        id,
        serde_json::json!({
            "capabilities": capabilities(),
            "serverInfo": { "name": "bms-lsp", "version": env!("CARGO_PKG_VERSION") }
        }),
    )?;

    let mut state = ServerState::default();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                handle_request(&connection, &state, &request)?;
            }
            Message::Notification(notification) => {
                handle_notification(&connection, &mut state, &notification)?;
            }
            Message::Response(_) => {}
        }
    }
    io_threads.join()?;
    Ok(())
}

/// 构建服务器能力。
fn capabilities() -> ServerCapabilities {
    let legend = SemanticTokensLegend {
        token_types: vec![
            lsp_types::SemanticTokenType::KEYWORD,
            lsp_types::SemanticTokenType::PROPERTY,
            lsp_types::SemanticTokenType::STRING,
            lsp_types::SemanticTokenType::NUMBER,
            lsp_types::SemanticTokenType::OPERATOR,
            lsp_types::SemanticTokenType::VARIABLE,
            lsp_types::SemanticTokenType::COMMENT,
        ],
        token_modifiers: Vec::new(),
    };
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec!["#".to_owned(), ":".to_owned(), " ".to_owned()]),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend,
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    }
}

/// 处理请求。
fn handle_request(
    connection: &Connection,
    state: &ServerState,
    request: &Request,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match request.method.as_str() {
        "textDocument/hover" => {
            let Some(params) =
                request_params::<HoverParams>(connection, &request.id, &request.params)?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, hover(state, &params))?;
        }
        "textDocument/completion" => {
            let Some(params) =
                request_params::<CompletionParams>(connection, &request.id, &request.params)?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, completion(state, &params))?;
        }
        "textDocument/definition" => {
            let Some(params) =
                request_params::<GotoDefinitionParams>(connection, &request.id, &request.params)?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, definition(state, &params))?;
        }
        "textDocument/references" => {
            let Some(params) =
                request_params::<ReferenceParams>(connection, &request.id, &request.params)?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, references(state, &params))?;
        }
        "textDocument/documentSymbol" => {
            let Some(params) = request_params::<lsp_types::DocumentSymbolParams>(
                connection,
                &request.id,
                &request.params,
            )?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, document_symbols(state, &params))?;
        }
        "textDocument/semanticTokens/full" => {
            let Some(params) = request_params::<lsp_types::SemanticTokensParams>(
                connection,
                &request.id,
                &request.params,
            )?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, semantic_tokens(state, &params))?;
        }
        "textDocument/foldingRange" => {
            let Some(params) =
                request_params::<FoldingRangeParams>(connection, &request.id, &request.params)?
            else {
                return Ok(());
            };
            send_result(connection, &request.id, folding_ranges(state, &params))?;
        }
        _ => send_error(
            connection,
            &request.id,
            ErrorCode::MethodNotFound,
            "method not found",
        )?,
    }
    Ok(())
}

/// 处理通知。
fn handle_notification(
    connection: &Connection,
    state: &mut ServerState,
    notification: &Notification,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match notification.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = parse_params(&notification.params)?;
            let uri = params.text_document.uri.clone();
            let document = Document::new(
                uri.clone(),
                params.text_document.text,
                params.text_document.version,
            );
            let diagnostics = document.diagnostics();
            let version = document.version;
            state.insert(&uri, document);
            publish(connection, uri, diagnostics, Some(version))?;
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = parse_params(&notification.params)?;
            let uri = params.text_document.uri;
            if let Some(change) = params.content_changes.last()
                && let Some(document) = state.documents.get_mut(&uri.to_string())
            {
                document.update(change.text.clone(), params.text_document.version);
                let diagnostics = document.diagnostics();
                let version = document.version;
                publish(connection, uri, diagnostics, Some(version))?;
            }
        }
        "textDocument/didClose" => {
            let params: DidCloseTextDocumentParams = parse_params(&notification.params)?;
            let uri = params.text_document.uri;
            state.remove(&uri);
            publish(connection, uri, Vec::new(), None)?;
        }
        _ => {}
    }
    Ok(())
}

/// 发布诊断。
fn publish(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    connection.sender.send(Message::Notification(Notification {
        method: "textDocument/publishDiagnostics".to_owned(),
        params: serde_json::to_value(params)?,
    }))?;
    Ok(())
}

/// 解析请求参数。
fn parse_params<T: serde::de::DeserializeOwned>(
    params: &serde_json::Value,
) -> Result<T, Box<dyn Error + Send + Sync>> {
    serde_json::from_value(params.clone())
        .map_err(|error| Box::new(error) as Box<dyn Error + Send + Sync>)
}

/// 解析请求参数；失败时回复 `InvalidParams`。
fn request_params<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    id: &RequestId,
    params: &serde_json::Value,
) -> Result<Option<T>, Box<dyn Error + Send + Sync>> {
    match serde_json::from_value(params.clone()) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            send_error(connection, id, ErrorCode::InvalidParams, &error.to_string())?;
            Ok(None)
        }
    }
}

/// 发送成功响应。
fn send_result<T: serde::Serialize>(
    connection: &Connection,
    id: &RequestId,
    result: T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection.sender.send(Message::Response(Response {
        id: id.clone(),
        response_result: Ok(serde_json::to_value(result)?),
    }))?;
    Ok(())
}

/// 发送错误响应。
fn send_error(
    connection: &Connection,
    id: &RequestId,
    code: ErrorCode,
    message: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection.sender.send(Message::Response(Response {
        id: id.clone(),
        response_result: Err(ResponseError {
            code: code as i32,
            message: message.to_owned(),
            data: None,
        }),
    }))?;
    Ok(())
}

/// 处理 hover。
fn hover(state: &ServerState, params: &HoverParams) -> Option<Hover> {
    let position = params.text_document_position_params.position;
    let document = state.get(&params.text_document_position_params.text_document.uri)?;
    let offset = document.offset(position)?;
    let token = document
        .analysis
        .source_file()
        .token_at_offset(TextSize::from(u32::try_from(offset).ok()?))?;
    let range = document.range(token.text_range());
    let value = hover_text(document, &token)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    })
}

/// 生成 hover 文本。
fn hover_text(document: &Document, token: &bms_syntax::SyntaxToken) -> Option<String> {
    match token.kind() {
        SyntaxKind::CommandName => header_doc(token.text()).map(header_markdown),
        SyntaxKind::Channel => channel_doc(token.text()).map(|doc| {
            format!(
                "### {}\n\n{}\n\n[原始资料]({})",
                doc.title, doc.summary, doc.link
            )
        }),
        SyntaxKind::BodyChunk | SyntaxKind::Value => symbol_markdown(document, token.text_range()),
        SyntaxKind::Hash | SyntaxKind::Percent | SyntaxKind::Prefix => token
            .parent()
            .and_then(bms_syntax::Line::cast)
            .and_then(|line| line.command_name())
            .and_then(|command| header_doc(command.text()))
            .map(header_markdown),
        _ => None,
    }
}

/// 生成头部说明 Markdown。
fn header_markdown(doc: &bms_analysis::HeaderDoc) -> String {
    format!(
        "### {}\n\n{}\n\n[原始资料]({})",
        doc.title, doc.summary, doc.link
    )
}

/// 生成符号说明 Markdown。
#[expect(
    clippy::format_push_string,
    reason = "hover 文本拼接优先可读性，分配开销可忽略"
)]
fn symbol_markdown(document: &Document, range: TextRange) -> Option<String> {
    let offset = range.start();
    let index = document.analysis.index();
    if let Some(reference) = index.reference_at(offset) {
        let definitions: Vec<_> = index
            .definitions_for(reference.kind(), reference.id())
            .collect();
        if definitions.is_empty() {
            return Some(format!(
                "未定义 {} `{}`",
                reference.kind().label(),
                reference.id()
            ));
        }
        let mut text = format!("### {} `{}`\n\n", reference.kind().label(), reference.id());
        for definition in definitions {
            text.push_str(&format!(
                "- 定义于 `{}`\n",
                document
                    .lines
                    .position(&document.text, usize::from(definition.range().start()))
                    .line
            ));
        }
        return Some(text);
    }
    index.definition_at(offset).map(|definition| {
        format!(
            "### {} `{}`\n\n定义；引用 {} 次",
            definition.kind().label(),
            definition.id(),
            index
                .references_for(definition.kind(), definition.id())
                .count()
        )
    })
}

/// 处理补全。
fn completion(state: &ServerState, params: &CompletionParams) -> Option<CompletionResponse> {
    let document = state.get(&params.text_document_position.text_document.uri)?;
    let offset = document.offset(params.text_document_position.position)?;
    let line_start = document
        .text
        .get(..offset)
        .unwrap_or_default()
        .rfind(['\n', '\r'])
        .map_or(0, |index| index.saturating_add(1));
    let prefix = document.text.get(line_start..offset).unwrap_or_default();
    let items = completion_items(document, prefix);
    (!items.is_empty()).then_some(CompletionResponse::Array(items))
}

/// 根据行前缀生成补全项。
fn completion_items(document: &Document, prefix: &str) -> Vec<CompletionItem> {
    let trimmed = prefix.trim_start();
    if !trimmed.starts_with('#') && !trimmed.starts_with('%') {
        return Vec::new();
    }
    if let Some((head, _body)) = trimmed.split_once(':') {
        return message_completion(document, head);
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let command = parts
        .next()
        .unwrap_or_default()
        .trim_start_matches(['#', '%']);
    if let Some(value) = parts.next() {
        return value_completion(document, command, value);
    }
    header_items()
}

/// 构造头部命令补全项。
fn header_items() -> Vec<CompletionItem> {
    bms_analysis::header_completions()
        .iter()
        .map(|doc| CompletionItem {
            label: format!("#{}", doc.command),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(doc.title.to_owned()),
            documentation: Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: doc.summary.to_owned(),
            })),
            insert_text: Some(format!("#{} ", doc.command)),
            ..CompletionItem::default()
        })
        .collect()
}

/// 通道行体补全。
fn message_completion(document: &Document, head: &str) -> Vec<CompletionItem> {
    let normalized = head.trim_start_matches(['#', '%']);
    let channel = normalized.get(3..5).unwrap_or_default();
    let Some(kind) = channel_symbol_kind(channel) else {
        return Vec::new();
    };
    document
        .analysis
        .index()
        .definitions()
        .iter()
        .filter(|definition| definition.kind() == kind)
        .map(|definition| CompletionItem {
            label: definition.id().to_owned(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some(format!("{} {}", kind.label(), definition.id())),
            ..CompletionItem::default()
        })
        .collect()
}

/// 头部值补全。
fn value_completion(document: &Document, command: &str, _value: &str) -> Vec<CompletionItem> {
    let Some(kind) = command_symbol_kind(command) else {
        return Vec::new();
    };
    document
        .analysis
        .index()
        .definitions()
        .iter()
        .filter(|definition| definition.kind() == kind)
        .map(|definition| CompletionItem {
            label: definition.id().to_owned(),
            kind: Some(CompletionItemKind::VALUE),
            detail: Some(format!("{} {}", kind.label(), definition.id())),
            ..CompletionItem::default()
        })
        .collect()
}

/// 通道对应的资源种类。
fn channel_symbol_kind(channel: &str) -> Option<SymbolKind> {
    match channel.to_ascii_uppercase().as_str() {
        "01" => Some(SymbolKind::Wav),
        "04" | "06" | "07" | "0A" => Some(SymbolKind::Bga),
        "08" => Some(SymbolKind::Bpm),
        "09" => Some(SymbolKind::Stop),
        "99" => Some(SymbolKind::Text),
        "A6" => Some(SymbolKind::ChangeOption),
        "SC" => Some(SymbolKind::Scroll),
        "SP" => Some(SymbolKind::Speed),
        value => {
            let bytes = value.as_bytes();
            (bytes.len() == 2 && matches!(bytes.first(), Some(b'1'..=b'6' | b'D' | b'E')))
                .then_some(SymbolKind::Wav)
        }
    }
}

/// 命令对应的资源种类。
fn command_symbol_kind(command: &str) -> Option<SymbolKind> {
    let upper = command.to_ascii_uppercase();
    if upper.starts_with("WAV") || upper.starts_with("EXWAV") {
        Some(SymbolKind::Wav)
    } else if upper.starts_with("BMP") || upper.starts_with("EXBMP") {
        Some(SymbolKind::Bmp)
    } else if upper.starts_with("BPM") || upper.starts_with("EXBPM") {
        Some(SymbolKind::Bpm)
    } else if upper.starts_with("STOP") {
        Some(SymbolKind::Stop)
    } else if upper.starts_with("SCROLL") {
        Some(SymbolKind::Scroll)
    } else if upper.starts_with("SPEED") {
        Some(SymbolKind::Speed)
    } else if upper.starts_with("EXRANK") {
        Some(SymbolKind::ExRank)
    } else if upper.starts_with("CHANGEOPTION") {
        Some(SymbolKind::ChangeOption)
    } else if upper.starts_with("TEXT") || upper.starts_with("SONG") {
        Some(SymbolKind::Text)
    } else if upper.starts_with("SEEK") {
        Some(SymbolKind::Seek)
    } else {
        None
    }
}

/// 处理跳转定义。
fn definition(
    state: &ServerState,
    params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let position = params.text_document_position_params.position;
    let document = state.get(&params.text_document_position_params.text_document.uri)?;
    let offset = document.offset(position)?;
    let index = document.analysis.index();
    let locations = definitions_at(document, index, TextSize::from(u32::try_from(offset).ok()?));
    (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
}

/// 收集指定偏移处的定义位置。
fn definitions_at(document: &Document, index: &SymbolIndex, offset: TextSize) -> Vec<Location> {
    let uri = document.uri();
    if let Some(reference) = index.reference_at(offset) {
        return index
            .definitions_for(reference.kind(), reference.id())
            .map(|definition| Location {
                uri: uri.clone(),
                range: document.range(definition.name_range()),
            })
            .collect();
    }
    index
        .definition_at(offset)
        .map(|definition| Location {
            uri,
            range: document.range(definition.name_range()),
        })
        .into_iter()
        .collect()
}

/// 处理引用查找。
fn references(state: &ServerState, params: &ReferenceParams) -> Option<Vec<Location>> {
    let position = params.text_document_position.position;
    let document = state.get(&params.text_document_position.text_document.uri)?;
    let offset = document.offset(position)?;
    let index = document.analysis.index();
    let text_offset = TextSize::from(u32::try_from(offset).ok()?);
    if let Some(reference) = index.reference_at(text_offset) {
        return Some(reference_locations(
            document,
            index,
            reference.kind(),
            reference.id(),
        ));
    }
    let definition = index.definition_at(text_offset)?;
    Some(reference_locations(
        document,
        index,
        definition.kind(),
        definition.id(),
    ))
}

/// 收集定义与引用位置。
fn reference_locations(
    document: &Document,
    index: &SymbolIndex,
    kind: SymbolKind,
    id: &str,
) -> Vec<Location> {
    let uri = document.uri();
    let mut locations: Vec<_> = index
        .definitions_for(kind, id)
        .map(|definition| Location {
            uri: uri.clone(),
            range: document.range(definition.name_range()),
        })
        .collect();
    locations.extend(index.references_for(kind, id).map(|reference| Location {
        uri: uri.clone(),
        range: document.range(reference.range()),
    }));
    locations
}

impl Document {
    /// 返回文档 URI；由 server 在插入时补充。
    fn uri(&self) -> Uri {
        self.uri.clone()
    }
}

/// 处理文档符号。
#[expect(
    deprecated,
    reason = "lsp-types 0.97 的 SymbolInformation 仍保留 deprecated 字段"
)]
fn document_symbols(
    state: &ServerState,
    params: &lsp_types::DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let document = state.get(&params.text_document.uri)?;
    let uri = document.uri();
    let symbols = document
        .analysis
        .source_file()
        .lines()
        .filter_map(|line| {
            let name = match line.kind() {
                bms_syntax::LineKind::Header => {
                    format!("#{}", line.command_name()?.text())
                }
                bms_syntax::LineKind::Message => {
                    format!("#{} {}", line.measure()?.text(), line.channel()?.text())
                }
                _ => return None,
            };
            Some(SymbolInformation {
                name,
                kind: lsp_types::SymbolKind::PROPERTY,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: uri.clone(),
                    range: document.range(line.range()),
                },
                container_name: None,
            })
        })
        .collect();
    Some(DocumentSymbolResponse::Flat(symbols))
}

/// 处理 semantic tokens。
fn semantic_tokens(
    state: &ServerState,
    params: &lsp_types::SemanticTokensParams,
) -> Option<SemanticTokens> {
    let document = state.get(&params.text_document.uri)?;
    let mut data = Vec::new();
    let mut last_line = 0u32;
    let mut last_start = 0u32;
    for element in document.analysis.syntax().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        let Some(token_type) = semantic_type(token.kind()) else {
            continue;
        };
        let start = usize::from(token.text_range().start());
        let position = document.lines.position(&document.text, start);
        let length = u32::try_from(token.text().encode_utf16().count()).unwrap_or(u32::MAX);
        let delta_line = position.line.saturating_sub(last_line);
        let delta_start = if delta_line == 0 {
            position.character.saturating_sub(last_start)
        } else {
            position.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });
        last_line = position.line;
        last_start = position.character;
    }
    Some(SemanticTokens {
        result_id: None,
        data,
    })
}

/// token 种类到 semantic token 类型索引。
const fn semantic_type(kind: SyntaxKind) -> Option<u32> {
    match kind {
        SyntaxKind::Hash | SyntaxKind::Percent | SyntaxKind::Prefix | SyntaxKind::ErrorToken => {
            Some(0)
        }
        SyntaxKind::CommandName => Some(1),
        SyntaxKind::Value | SyntaxKind::Text => Some(2),
        SyntaxKind::Measure | SyntaxKind::Channel => Some(3),
        SyntaxKind::Colon => Some(4),
        SyntaxKind::BodyChunk => Some(5),
        SyntaxKind::Comment => Some(6),
        _ => None,
    }
}

/// 处理折叠范围。
fn folding_ranges(state: &ServerState, params: &FoldingRangeParams) -> Option<Vec<FoldingRange>> {
    let document = state.get(&params.text_document.uri)?;
    let mut ranges = Vec::new();
    for node in document.analysis.syntax().descendants() {
        if !matches!(
            node.kind(),
            SyntaxKind::RandomBlock
                | SyntaxKind::RandomChain
                | SyntaxKind::RandomBranch
                | SyntaxKind::SwitchBlock
                | SyntaxKind::SwitchCase
        ) {
            continue;
        }
        let range = node.text_range();
        let start = document
            .lines
            .position(&document.text, usize::from(range.start()));
        let end_offset = usize::from(range.end()).saturating_sub(1);
        let end = document.lines.position(&document.text, end_offset);
        if end.line > start.line {
            ranges.push(FoldingRange {
                start_line: start.line,
                start_character: None,
                end_line: end.line,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }
    Some(ranges)
}
