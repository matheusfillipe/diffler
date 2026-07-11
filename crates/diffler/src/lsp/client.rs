//! Minimal LSP client over a child process's stdio: just enough JSON-RPC to
//! initialize, open documents, and ask for symbols and references. Server-
//! initiated requests are answered with `null` so servers like rust-analyzer
//! don't stall waiting on capability registration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::lsp::{Caller, LspError, RefSite, Symbol};

/// A `Position`/`Range` are atomic in the LSP spec (`line`+`character`,
/// `start`+`end` always travel together), so unlike the fields below they're
/// modeled as plain required fields rather than defended with fallbacks.
#[derive(Deserialize)]
struct LspPosition {
    line: u32,
    character: u32,
}

#[derive(Deserialize)]
struct LspRange {
    start: LspPosition,
    end: LspPosition,
}

#[derive(Deserialize)]
struct Location {
    uri: String,
    range: LspRange,
}

/// Gate fields for `textDocument/documentSymbol`: if any of these fail to
/// parse, there was never a resolvable symbol to push (matching the old
/// pointer-walk, which also skipped the node whenever `name` or `range`
/// didn't resolve). `kind` degrades to 0 rather than failing the node, since
/// a missing kind still needs to fall through the function-kind filter below.
#[derive(Deserialize)]
struct DocumentSymbolCore {
    name: String,
    #[serde(default)]
    kind: u32,
    range: LspRange,
}

/// `selectionRange` is genuinely optional in the wild: some servers omit it
/// or leave `start` partial, and the symbol is still pushed using `range`'s
/// start line and character 0 as the fallback position — so this is parsed
/// independently of `DocumentSymbolCore` and never fails the whole node.
#[derive(Deserialize, Default)]
struct SelectionRange {
    #[serde(default)]
    start: SelectionStart,
}

#[derive(Deserialize, Default)]
struct SelectionStart {
    line: Option<u32>,
    #[serde(default)]
    character: u32,
}

/// The shape of a `CallHierarchyItem`, read from `incomingCalls`'s `from`.
#[derive(Deserialize)]
struct CallHierarchyItem {
    name: String,
    uri: String,
    range: LspRange,
    #[serde(rename = "selectionRange")]
    selection_range: LspRange,
}

#[derive(Deserialize)]
struct IncomingCall {
    from: CallHierarchyItem,
}

pub struct LspClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    root: PathBuf,
    open_docs: HashMap<PathBuf, i64>,
}

impl LspClient {
    pub async fn spawn(bin: &str, argv: &[&str], root: &Path) -> Result<Self, LspError> {
        let mut child = Command::new(bin)
            .args(argv)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| LspError::Spawn(bin.to_owned(), err.to_string()))?;
        let root = canonical_root(root);
        let stdin = child.stdin.take().ok_or(LspError::Io("no stdin"))?;
        let stdout = BufReader::new(child.stdout.take().ok_or(LspError::Io("no stdout"))?);
        let mut client = Self {
            _child: child,
            stdin,
            stdout,
            next_id: 0,
            root,
            open_docs: HashMap::new(),
        };
        let root_uri = uri(&client.root);
        client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": root_uri,
                    "workspaceFolders": [{"uri": root_uri, "name": "root"}],
                    "capabilities": {
                        "textDocument": {
                            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                            "references": {},
                            "callHierarchy": {}
                        }
                    }
                }),
            )
            .await?;
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    pub async fn sync_document(&mut self, path: &Path, text: &str) -> Result<(), LspError> {
        let abs = self.root.join(path);
        match self.open_docs.get_mut(&abs) {
            None => {
                self.open_docs.insert(abs.clone(), 1);
                self.notify(
                    "textDocument/didOpen",
                    json!({"textDocument": {
                        "uri": uri(&abs),
                        "languageId": crate::lsp::registry::language_id(path),
                        "version": 1,
                        "text": text,
                    }}),
                )
                .await
            }
            Some(version) => {
                *version += 1;
                let version = *version;
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": uri(&abs), "version": version},
                        "contentChanges": [{"text": text}],
                    }),
                )
                .await
            }
        }
    }

    pub async fn document_symbols(&mut self, path: &Path) -> Result<Vec<Symbol>, LspError> {
        let abs = self.root.join(path);
        let result = self
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": uri(&abs)}}),
            )
            .await?;
        let mut out = Vec::new();
        collect_symbols(result.as_array().unwrap_or(&Vec::new()), &mut out);
        Ok(out)
    }

    pub async fn references(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<RefSite>, LspError> {
        let abs = self.root.join(path);
        let result = self
            .request(
                "textDocument/references",
                json!({
                    "textDocument": {"uri": uri(&abs)},
                    "position": {"line": line, "character": character},
                    "context": {"includeDeclaration": false},
                }),
            )
            .await?;
        Ok(result
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|loc| {
                let loc: Location = serde_json::from_value(loc.clone()).ok()?;
                let path = rel_path(&self.root, &loc.uri)?;
                Some(RefSite {
                    path,
                    line: loc.range.start.line,
                })
            })
            .collect())
    }

    pub async fn incoming_calls(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Vec<Caller>, LspError> {
        let abs = self.root.join(path);
        let items = self
            .request(
                "textDocument/prepareCallHierarchy",
                json!({
                    "textDocument": {"uri": uri(&abs)},
                    "position": {"line": line, "character": character},
                }),
            )
            .await?;
        // The raw item (not a struct rebuilt from it) is forwarded to
        // `incomingCalls` below: servers may carry extra fields (e.g. `data`)
        // on it that must round-trip untouched.
        let Some(item) = items.as_array().and_then(|a| a.first()).cloned() else {
            return Ok(Vec::new());
        };
        let calls = self
            .request("callHierarchy/incomingCalls", json!({"item": item}))
            .await?;
        Ok(calls
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|call| {
                let call: IncomingCall = serde_json::from_value(call.clone()).ok()?;
                let path = rel_path(&self.root, &call.from.uri)?;
                Some(Caller {
                    name: call.from.name,
                    path,
                    line: call.from.range.start.line,
                    select_line: call.from.selection_range.start.line,
                    select_character: call.from.selection_range.start.character,
                })
            })
            .collect())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        tokio::time::timeout(REQUEST_TIMEOUT, self.await_response(id, method))
            .await
            .map_err(|_| LspError::Io("timeout"))?
    }

    async fn await_response(&mut self, id: i64, method: &str) -> Result<Value, LspError> {
        loop {
            let message = self.read_message().await?;
            if message.get("id").and_then(Value::as_i64) == Some(id)
                && message.get("method").is_none()
            {
                if let Some(error) = message.get("error") {
                    return Err(LspError::Server(method.to_owned(), error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if let Some(request_id) = message
                .get("method")
                .and(message.get("id"))
                .and_then(Value::as_i64)
            {
                let result = server_request_result(&message);
                self.send(&json!({"jsonrpc": "2.0", "id": request_id, "result": result}))
                    .await?;
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    async fn send(&mut self, message: &Value) -> Result<(), LspError> {
        let body = message.to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.stdin
            .write_all(framed.as_bytes())
            .await
            .map_err(|_| LspError::Io("write"))?;
        self.stdin.flush().await.map_err(|_| LspError::Io("flush"))
    }

    async fn read_message(&mut self) -> Result<Value, LspError> {
        let mut length: usize = 0;
        loop {
            let mut header = String::new();
            let read = self
                .stdout
                .read_line(&mut header)
                .await
                .map_err(|_| LspError::Io("read header"))?;
            if read == 0 {
                return Err(LspError::Io("server closed"));
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length: ") {
                length = value.parse().map_err(|_| LspError::Io("bad length"))?;
            }
        }
        let mut body = vec![0u8; length];
        self.stdout
            .read_exact(&mut body)
            .await
            .map_err(|_| LspError::Io("read body"))?;
        serde_json::from_slice(&body).map_err(|_| LspError::Io("bad json"))
    }
}

/// Servers hold every response until their pending request is answered, so a
/// reply that never comes would wedge the client (and its pool slot) forever.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// `workspace/configuration` expects one entry per queried item; everything
/// else the client doesn't implement is answered with plain `null`.
fn server_request_result(message: &Value) -> Value {
    if message.get("method").and_then(Value::as_str) == Some("workspace/configuration") {
        let items = message
            .pointer("/params/items")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        return Value::Array(vec![Value::Null; items]);
    }
    Value::Null
}

/// The canonical root for URI building. Windows `canonicalize` returns a
/// verbatim `\\?\C:\…` path that `Url::from_file_path` and servers won't
/// produce, so the prefix comes back off.
fn canonical_root(root: &Path) -> PathBuf {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let text = canonical.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) => PathBuf::from(stripped),
        None => canonical,
    }
}

/// A `file://` URI with the path percent-encoded the way servers emit them.
fn uri(path: &Path) -> String {
    url::Url::from_file_path(path).map_or_else(
        |()| format!("file://{}", path.display()),
        |url| url.to_string(),
    )
}

/// A server-reported URI back to a repo-relative path, or `None` when it
/// points outside `root` (stdlib, dependencies).
fn rel_path(root: &Path, uri: &str) -> Option<String> {
    let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

const FUNCTION_KINDS: &[u32] = &[6, 9, 12];

/// Recurses into `children` unconditionally, before parsing the node's own
/// fields, so a node with a broken `name`/`kind`/`range` still surfaces any
/// valid symbols nested underneath it — matching the old pointer-walk, which
/// never let a node's own validity gate its children.
fn collect_symbols(nodes: &[Value], out: &mut Vec<Symbol>) {
    for node in nodes {
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            collect_symbols(children, out);
        }
        let Ok(core) = serde_json::from_value::<DocumentSymbolCore>(node.clone()) else {
            continue;
        };
        if !FUNCTION_KINDS.contains(&core.kind) {
            continue;
        }
        let selection = node
            .get("selectionRange")
            .and_then(|v| serde_json::from_value::<SelectionRange>(v.clone()).ok())
            .unwrap_or_default();
        out.push(Symbol {
            name: core.name,
            start_line: core.range.start.line,
            end_line: core.range.end.line,
            select_line: selection.start.line.unwrap_or(core.range.start.line),
            select_character: selection.start.character,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn uri_and_rel_path_round_trip_spaces() {
        let root = Path::new("/repo dir");
        let uri = uri(&root.join("src/a file.rs"));
        assert_eq!(uri, "file:///repo%20dir/src/a%20file.rs");
        assert_eq!(rel_path(root, &uri).as_deref(), Some("src/a file.rs"));
    }

    #[test]
    fn rel_path_rejects_locations_outside_the_root() {
        assert_eq!(rel_path(Path::new("/repo"), "file:///elsewhere/x.rs"), None);
        assert_eq!(rel_path(Path::new("/repo"), "not a uri"), None);
    }

    #[test]
    fn configuration_requests_get_one_null_per_item() {
        let msg = serde_json::json!({
            "method": "workspace/configuration",
            "params": {"items": [{}, {}]},
        });
        assert_eq!(
            server_request_result(&msg),
            Value::Array(vec![Value::Null, Value::Null])
        );
        assert_eq!(
            server_request_result(&serde_json::json!({"method": "x"})),
            Value::Null
        );
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> serde_json::Value {
        serde_json::json!({
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        })
    }

    #[test]
    fn collect_symbols_reads_kind_name_and_ranges() {
        let nodes = serde_json::json!([{
            "name": "target",
            "kind": 12,
            "range": range((0, 0), (2, 1)),
            "selectionRange": range((0, 3), (0, 9)),
        }]);
        let mut out = Vec::new();
        collect_symbols(nodes.as_array().unwrap(), &mut out);
        assert_eq!(
            out,
            vec![Symbol {
                name: "target".into(),
                start_line: 0,
                end_line: 2,
                select_line: 0,
                select_character: 3,
            }]
        );
    }

    #[test]
    fn collect_symbols_filters_non_function_kinds() {
        let nodes = serde_json::json!([{
            "name": "Widget",
            "kind": 5, // class, not a function kind
            "range": range((0, 0), (5, 1)),
            "selectionRange": range((0, 6), (0, 12)),
        }]);
        let mut out = Vec::new();
        collect_symbols(nodes.as_array().unwrap(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn collect_symbols_defaults_missing_selection_range_to_the_outer_range_start() {
        let nodes = serde_json::json!([{
            "name": "target",
            "kind": 12,
            "range": range((4, 0), (6, 1)),
        }]);
        let mut out = Vec::new();
        collect_symbols(nodes.as_array().unwrap(), &mut out);
        assert_eq!(out[0].select_line, 4, "falls back to range.start.line");
        assert_eq!(out[0].select_character, 0);
    }

    #[test]
    fn collect_symbols_recurses_into_children_of_an_otherwise_unresolvable_node() {
        let nodes = serde_json::json!([{
            "name": "impl Foo",
            // no "kind"/"range" at all on the parent, but its children are
            // still real, pushable symbols
            "children": [{
                "name": "target",
                "kind": 6,
                "range": range((1, 0), (1, 20)),
                "selectionRange": range((1, 3), (1, 9)),
            }],
        }]);
        let mut out = Vec::new();
        collect_symbols(nodes.as_array().unwrap(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "target");
    }

    #[test]
    fn collect_symbols_skips_a_node_with_no_resolvable_range() {
        let nodes = serde_json::json!([{"name": "target", "kind": 12}]);
        let mut out = Vec::new();
        collect_symbols(nodes.as_array().unwrap(), &mut out);
        assert!(out.is_empty());
    }
}
