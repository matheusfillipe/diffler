//! Minimal LSP client over a child process's stdio: just enough JSON-RPC to
//! initialize, open documents, and ask for symbols and references. Server-
//! initiated requests are answered with `null` so servers like rust-analyzer
//! don't stall waiting on capability registration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::lsp::{Caller, LspError, RefSite, Symbol};

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
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
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
                        "languageId": language_id(path),
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
        let root_prefix = format!("{}/", uri(&self.root));
        Ok(result
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|loc| {
                let target = loc.get("uri")?.as_str()?;
                let line = loc.pointer("/range/start/line")?.as_u64()? as u32;
                let path = decode_uri_path(target.strip_prefix(&root_prefix)?);
                Some(RefSite { path, line })
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
        let Some(item) = items.as_array().and_then(|a| a.first()).cloned() else {
            return Ok(Vec::new());
        };
        let calls = self
            .request("callHierarchy/incomingCalls", json!({"item": item}))
            .await?;
        let root_prefix = format!("{}/", uri(&self.root));
        Ok(calls
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|call| {
                let from = call.get("from")?;
                let name = from.get("name")?.as_str()?.to_owned();
                let target = from.get("uri")?.as_str()?;
                let path = decode_uri_path(target.strip_prefix(&root_prefix)?);
                let at = |ptr: &str| from.pointer(ptr).and_then(Value::as_u64).map(|v| v as u32);
                Some(Caller {
                    name,
                    path,
                    line: at("/range/start/line")?,
                    select_line: at("/selectionRange/start/line")?,
                    select_character: at("/selectionRange/start/character")?,
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

/// A `file://` URI with the path percent-encoded the way servers emit them,
/// so prefix-matching server URIs back to repo-relative paths stays exact.
fn uri(path: &Path) -> String {
    use std::fmt::Write;
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(char::from(byte));
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Decode `%XX` escapes in a URI path back into the on-disk path.
fn decode_uri_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes.get(i) == Some(&b'%'))
            .then(|| encoded.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(byte) = escaped {
            out.push(byte);
            i += 3;
        } else {
            out.extend(bytes.get(i..=i).unwrap_or_default());
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn language_id(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "rb" => "ruby",
        "sh" | "bash" => "shellscript",
        _ => "plaintext",
    }
}

const FUNCTION_KINDS: &[u64] = &[6, 9, 12];

fn collect_symbols(nodes: &[Value], out: &mut Vec<Symbol>) {
    for node in nodes {
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            collect_symbols(children, out);
        }
        let kind = node.get("kind").and_then(Value::as_u64).unwrap_or(0);
        if !FUNCTION_KINDS.contains(&kind) {
            continue;
        }
        let Some(name) = node.get("name").and_then(Value::as_str) else {
            continue;
        };
        let range = |ptr: &str| node.pointer(ptr).and_then(Value::as_u64).map(|v| v as u32);
        let (Some(start), Some(end)) = (range("/range/start/line"), range("/range/end/line"))
        else {
            continue;
        };
        out.push(Symbol {
            name: name.to_owned(),
            start_line: start,
            end_line: end,
            select_line: range("/selectionRange/start/line").unwrap_or(start),
            select_character: range("/selectionRange/start/character").unwrap_or(0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_percent_encodes_and_decode_reverses_it() {
        let uri = uri(Path::new("/repo dir/src/a file.rs"));
        assert_eq!(uri, "file:///repo%20dir/src/a%20file.rs");
        let path = uri.strip_prefix("file:///repo%20dir/").expect("prefix");
        assert_eq!(decode_uri_path(path), "src/a file.rs");
    }

    #[test]
    fn decode_leaves_malformed_escapes_alone() {
        assert_eq!(decode_uri_path("a%2"), "a%2");
        assert_eq!(decode_uri_path("a%zz"), "a%zz");
        assert_eq!(decode_uri_path("plain"), "plain");
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
}
