//! Fake authorization server: discovery, PKCE, call, refresh, and revoke.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use termior_agent_host::*;
use termior_ai::{InMemorySecretStore, SecretStore};

const START: u64 = 1_700_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallbackFault {
    None,
    BadState,
    BadResource,
}

struct FakeState {
    seq: u64,
    access: Option<String>,
    refresh: Option<String>,
    codes: HashMap<String, String>,
    secrets: Vec<String>,
    refresh_count: u32,
    revoke_count: u32,
    register_count: u32,
    mcp_calls: u32,
    mcp_calls_after_revoke: u32,
    revoked: bool,
    break_refresh: bool,
    callback_fault: CallbackFault,
    resource: String,
}

struct FakeServer {
    mcp: String,
    state: Arc<Mutex<FakeState>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn spawn_fake(callback_fault: CallbackFault) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let mcp = format!("{base}/mcp");
    let state = Arc::new(Mutex::new(FakeState {
        seq: 0,
        access: None,
        refresh: None,
        codes: HashMap::new(),
        secrets: Vec::new(),
        refresh_count: 0,
        revoke_count: 0,
        register_count: 0,
        mcp_calls: 0,
        mcp_calls_after_revoke: 0,
        revoked: false,
        break_refresh: false,
        callback_fault,
        resource: mcp.clone(),
    }));
    let shutdown = Arc::new(AtomicBool::new(false));
    let state_thread = Arc::clone(&state);
    let shutdown_thread = Arc::clone(&shutdown);
    let thread = std::thread::spawn(move || {
        serve(listener, state_thread, shutdown_thread);
    });
    FakeServer {
        mcp,
        state,
        shutdown,
        thread: Some(thread),
    }
}

fn serve(listener: TcpListener, state: Arc<Mutex<FakeState>>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                if let Some(request) = read_request(&mut stream) {
                    let response = handle(&request, &state);
                    let _ = stream.write_all(response.as_bytes());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    let mut buf = Vec::new();
    let mut temp = [0u8; 2048];
    let header_end;
    loop {
        if buf.len() > 64 * 1024 {
            return None;
        }
        let count = stream.read(&mut temp).ok()?;
        if count == 0 {
            return None;
        }
        buf.extend_from_slice(&temp[..count]);
        if let Some(pos) = buf.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            header_end = pos;
            break;
        }
    }
    let header_text = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let length = header_text
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or(0);
    while buf.len() < header_end + 4 + length {
        let count = stream.read(&mut temp).ok()?;
        if count == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..count]);
    }
    let body = String::from_utf8_lossy(&buf[header_end + 4..]).into_owned();
    let mut lines = header_text.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let (path, query_text) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Some(HttpRequest {
        method,
        path: path.to_owned(),
        query: parse_pairs(query_text),
        headers: header_text,
        body,
    })
}

fn handle(request: &HttpRequest, state: &Arc<Mutex<FakeState>>) -> String {
    let mut guard = state.lock().expect("fake auth state");
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/mcp") => mcp_response(&mut guard, request),
        ("GET", "/.well-known/oauth-protected-resource") => json_ok(&json!({
            "resource": guard.resource,
            "authorization_servers": [origin(&guard.resource)],
            "bearer_methods_supported": ["header"],
            "scopes_supported": ["mcp"]
        })),
        ("GET", "/.well-known/oauth-authorization-server") => {
            let origin = origin(&guard.resource);
            json_ok(&json!({
                "issuer": origin,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "registration_endpoint": format!("{origin}/register"),
                "revocation_endpoint": format!("{origin}/revoke"),
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "scopes_supported": ["mcp"]
            }))
        }
        ("POST", "/register") => {
            guard.register_count += 1;
            json_ok(&json!({
                "client_id": "termior-public",
                "token_endpoint_auth_method": "none"
            }))
        }
        ("GET", "/authorize") => authorize(&mut guard, request),
        ("POST", "/token") => token(&mut guard, &parse_pairs(&request.body)),
        ("POST", "/revoke") => revoke(&mut guard, &parse_pairs(&request.body)),
        _ => text(404, "Not Found", "", "missing"),
    }
}

fn mcp_response(state: &mut FakeState, request: &HttpRequest) -> String {
    if state.revoked {
        state.mcp_calls_after_revoke += 1;
    }
    let token = bearer(&request.headers);
    let authorized = matches!(
        (state.access.as_deref(), token.as_deref()),
        (Some(expected), Some(presented)) if expected == presented
    );
    if !authorized {
        let origin = origin(&state.resource);
        return text(
            401,
            "Unauthorized",
            &format!(
                "WWW-Authenticate: Bearer realm=\"mcp\", resource_metadata=\"{origin}/.well-known/oauth-protected-resource\"\r\n"
            ),
            "",
        );
    }
    state.mcp_calls += 1;
    let echo = token.unwrap_or_default();
    let body = request.body.as_str();
    if body.contains("notifications/initialized") {
        return text(202, "Accepted", "", "");
    }
    let id = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_u64))
        .unwrap_or(1);
    let result = if body.contains("\"method\":\"initialize\"") {
        json!({"protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {}})
    } else if body.contains("tools/list") {
        json!({"tools": [{"name": "search", "inputSchema": {"type": "object", "properties": {}}}]})
    } else if body.contains("resources/list") {
        json!({"resources": []})
    } else if body.contains("prompts/list") {
        json!({"prompts": []})
    } else {
        json!({"content": [{"type": "text", "text": format!("echo {echo}")}]})
    };
    json_ok(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn authorize(state: &mut FakeState, request: &HttpRequest) -> String {
    let resource = request
        .query
        .get("resource")
        .map(String::as_str)
        .unwrap_or("");
    let method = request
        .query
        .get("code_challenge_method")
        .map(String::as_str)
        .unwrap_or("");
    let challenge = request
        .query
        .get("code_challenge")
        .cloned()
        .unwrap_or_default();
    let redirect = request
        .query
        .get("redirect_uri")
        .cloned()
        .unwrap_or_default();
    let state_param = request.query.get("state").cloned().unwrap_or_default();
    if resource != state.resource
        || method != "S256"
        || challenge.len() < 20
        || !redirect.starts_with("http://127.0.0.1:")
        || state_param.is_empty()
        || request.query.get("response_type").map(String::as_str) != Some("code")
    {
        return text(400, "Bad Request", "", "rejected authorize");
    }
    state.seq += 1;
    let code = format!("termior-test-code-{}", state.seq);
    state.secrets.push(code.clone());
    state.codes.insert(code.clone(), challenge);
    let callback_state = if state.callback_fault == CallbackFault::BadState {
        "wrong-state".to_owned()
    } else {
        state_param
    };
    let mut location = format!("{redirect}?code={code}&state={callback_state}");
    if state.callback_fault == CallbackFault::BadResource {
        location.push_str("&resource=https://evil.example/mcp");
    }
    text(302, "Found", &format!("Location: {location}\r\n"), "")
}

fn token(state: &mut FakeState, form: &HashMap<String, String>) -> String {
    let grant = form.get("grant_type").map(String::as_str).unwrap_or("");
    if form.get("resource").map(String::as_str) != Some(state.resource.as_str()) {
        return json_status(400, &json!({"error": "invalid_target"}));
    }
    if grant == "authorization_code" {
        let code = form.get("code").cloned().unwrap_or_default();
        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
        state.secrets.push(verifier.clone());
        let Some(challenge) = state.codes.remove(&code) else {
            return json_status(400, &json!({"error": "invalid_grant"}));
        };
        if oauth_code_challenge_s256(&verifier) != challenge {
            return json_status(400, &json!({"error": "invalid_grant"}));
        }
        return json_ok(&issue(state));
    }
    if grant == "refresh_token" {
        state.refresh_count += 1;
        let refresh = form.get("refresh_token").cloned().unwrap_or_default();
        if state.break_refresh || state.refresh.as_deref() != Some(refresh.as_str()) {
            return json_status(400, &json!({"error": "invalid_grant"}));
        }
        return json_ok(&issue(state));
    }
    json_status(400, &json!({"error": "unsupported_grant_type"}))
}

fn issue(state: &mut FakeState) -> Value {
    state.seq += 1;
    let access = format!("termior-test-access-{}-secret", state.seq);
    let refresh = format!("termior-test-refresh-{}-secret", state.seq);
    state.secrets.push(access.clone());
    state.secrets.push(refresh.clone());
    state.access = Some(access.clone());
    state.refresh = Some(refresh.clone());
    json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 60,
        "scope": "mcp"
    })
}

fn revoke(state: &mut FakeState, form: &HashMap<String, String>) -> String {
    state.revoke_count += 1;
    let token = form.get("token").cloned().unwrap_or_default();
    if state.access.as_deref() == Some(token.as_str())
        || state.refresh.as_deref() == Some(token.as_str())
    {
        state.access = None;
        state.refresh = None;
        state.revoked = true;
    }
    text(200, "OK", "", "")
}

fn origin(resource: &str) -> String {
    resource.trim_end_matches("/mcp").to_owned()
}

fn bearer(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("authorization") {
            let value = value.trim();
            return value
                .get(7..)
                .filter(|_| value.len() > 7 && value[..7].eq_ignore_ascii_case("bearer "))
                .map(str::to_owned);
        }
    }
    None
}

fn json_ok(body: &Value) -> String {
    json_status(200, body)
}

fn json_status(status: u16, body: &Value) -> String {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let text = body.to_string();
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )
}

fn text(status: u16, reason: &str, extra_headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn parse_pairs(input: &str) -> HashMap<String, String> {
    let mut pairs = HashMap::new();
    for part in input.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        pairs.insert(percent_decode(key), percent_decode(value));
    }
    pairs
}

fn percent_decode(input: &str) -> String {
    let input = input.replace('+', " ");
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn clock(start: Arc<AtomicU64>) -> McpClock {
    Arc::new(move || UNIX_EPOCH + Duration::from_secs(start.load(Ordering::SeqCst)))
}

fn binding(
    server_id: &str,
    store: Arc<InMemorySecretStore>,
    dir: &Path,
    now: McpClock,
) -> McpOAuthBinding {
    McpOAuthBinding {
        server_id: server_id.to_owned(),
        store,
        catalog_path: dir.join("mcp-servers.json"),
        now,
        diagnostic_log: Some(dir.join("mcp.log")),
    }
}

fn browser(expected_resource: &str, url: &str) -> Result<(), String> {
    let query = url.split_once('?').map(|(_, query)| query).unwrap_or("");
    let pairs = parse_pairs(query);
    if pairs.get("response_type").map(String::as_str) != Some("code") {
        return Err("response type".into());
    }
    if pairs.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return Err("pkce".into());
    }
    if pairs.get("resource").map(String::as_str) != Some(expected_resource) {
        return Err("resource".into());
    }
    if pairs.get("state").map(String::is_empty).unwrap_or(true) {
        return Err("state".into());
    }
    let (status, location) = exchange(url)?;
    if status != 302 {
        return Err("authorize did not redirect".into());
    }
    let location = location.ok_or_else(|| "missing redirect".to_owned())?;
    if !location.starts_with("http://127.0.0.1:") {
        return Err("redirect left loopback".into());
    }
    let (status, _) = exchange(&location)?;
    if status != 200 {
        return Err("callback was rejected".into());
    }
    Ok(())
}

fn exchange(url: &str) -> Result<(u16, Option<String>), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "url".to_owned())?;
    let (authority, target) = rest.split_once('/').ok_or_else(|| "url".to_owned())?;
    let (host, port) = authority.split_once(':').ok_or_else(|| "url".to_owned())?;
    let port: u16 = port.parse().map_err(|_| "port".to_owned())?;
    let mut stream = TcpStream::connect((host, port)).map_err(|_| "connect".to_owned())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "timeout".to_owned())?;
    let request =
        format!("GET /{target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "write".to_owned())?;
    let mut buf = Vec::new();
    let mut temp = [0u8; 2048];
    loop {
        match stream.read(&mut temp) {
            Ok(0) => break,
            Ok(count) => buf.extend_from_slice(&temp[..count]),
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| "status".to_owned())?;
    let location = text.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .starts_with("location:")
            .then(|| {
                line.split_once(':')
                    .map(|(_, value)| value.trim().to_owned())
            })
            .flatten()
    });
    Ok((status, location))
}

fn assert_dir_has_no_secrets(dir: &Path, secrets: &[String]) {
    let mut leaked = false;
    visit(dir, &mut |text| {
        if secrets
            .iter()
            .any(|secret| secret.len() >= 8 && text.contains(secret))
        {
            leaked = true;
        }
    });
    assert!(!leaked, "credential material was written to disk");
}

fn visit(dir: &Path, inspect: &mut dyn FnMut(&str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, inspect);
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            inspect(&text);
        }
    }
}

fn remembered_secrets(
    server: &FakeServer,
    store: &InMemorySecretStore,
    server_id: &str,
) -> Vec<String> {
    let mut secrets = server.state.lock().expect("state").secrets.clone();
    if let Ok(Some(raw)) = store.get(&mcp_oauth_key(server_id)) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            for key in ["access_token", "refresh_token"] {
                if let Some(token) = value.get(key).and_then(Value::as_str) {
                    secrets.push(token.to_owned());
                }
            }
        }
    }
    secrets
}

fn login(server: &FakeServer, oauth: &McpOAuthBinding) {
    let resource = server.mcp.clone();
    login_with_browser(
        &server.mcp,
        &PkceLoginOptions {
            binding: oauth.clone(),
            client_id: None,
            scopes: vec!["mcp".into()],
            timeout: Duration::from_secs(5),
        },
        &move |url| browser(&resource, url),
    )
    .expect("pkce login");
}

#[test]
fn oauth_lifecycle_discovers_authorizes_refreshes_and_revokes_without_persisting_tokens() {
    let fake = spawn_fake(CallbackFault::None);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(InMemorySecretStore::new());
    let ticks = Arc::new(AtomicU64::new(START));
    let oauth = binding(
        "docs",
        Arc::clone(&store),
        dir.path(),
        clock(Arc::clone(&ticks)),
    );
    McpServerCatalog {
        servers: vec![McpRemoteServer {
            id: "docs".into(),
            url: fake.mcp.clone(),
            oauth_key_ref: Some(mcp_oauth_key("docs")),
            client_id: None,
            scopes: vec!["mcp".into()],
            auth_status: McpLoginStatus::LoggedOut,
            auth_detail: String::new(),
        }],
    }
    .save(&oauth.catalog_path, &[])
    .unwrap();

    let discovered = discover_authorization(&fake.mcp).expect("discovery");
    assert_eq!(discovered.resource, fake.mcp);
    assert!(discovered.authorization_endpoint.ends_with("/authorize"));
    assert!(discovered.token_endpoint.ends_with("/token"));
    assert!(discovered
        .revocation_endpoint
        .as_deref()
        .unwrap()
        .ends_with("/revoke"));
    assert_eq!(discovered.scopes_supported, vec!["mcp".to_owned()]);

    login(&fake, &oauth);
    assert_eq!(fake.state.lock().unwrap().register_count, 1);
    let mut client = McpHttpClient::with_oauth(&fake.mcp, oauth.clone()).unwrap();
    client.initialize().unwrap();
    let discovery = client.discover().unwrap();
    let tool =
        QualifiedMcpTool::from_declaration("docs", discovery.tools[0].clone(), &Default::default());
    let first = client.call_tool(&tool, json!({})).unwrap();
    assert_eq!(first["content"][0]["text"], "echo [REDACTED]");
    let mut secrets = remembered_secrets(&fake, &store, "docs");
    std::fs::write(dir.path().join("journal.jsonl"), first.to_string()).unwrap();

    ticks.store(START + 120, Ordering::SeqCst);
    let second = client.call_tool(&tool, json!({})).unwrap();
    assert_eq!(second["content"][0]["text"], "echo [REDACTED]");
    assert_eq!(fake.state.lock().unwrap().refresh_count, 1);
    secrets.extend(remembered_secrets(&fake, &store, "docs"));
    std::fs::write(dir.path().join("journal-refresh.jsonl"), second.to_string()).unwrap();

    let report = logout_mcp_server(&oauth).unwrap();
    assert!(report.remote_revoked);
    assert_eq!(fake.state.lock().unwrap().revoke_count, 1);
    assert!(store.get(&mcp_oauth_key("docs")).unwrap().is_none());
    let catalog = McpServerCatalog::load(&oauth.catalog_path).unwrap();
    assert_eq!(catalog.servers[0].auth_status, McpLoginStatus::LoggedOut);
    assert!(catalog.servers[0].auth_detail.is_empty());

    let error = client.call_tool(&tool, json!({})).unwrap_err();
    assert!(error.to_string().contains("sign-in"));
    assert_eq!(fake.state.lock().unwrap().mcp_calls_after_revoke, 0);
    secrets.retain(|secret| secret.len() >= 8);
    secrets.sort();
    secrets.dedup();
    assert!(secrets.iter().any(|secret| secret.contains("access")));
    assert!(secrets.iter().any(|secret| secret.contains("refresh")));
    assert!(secrets
        .iter()
        .any(|secret| secret.contains("code-verifier") || secret.len() >= 40));
    for secret in &secrets {
        assert!(!error.to_string().contains(secret));
        assert!(!first.to_string().contains(secret));
        assert!(!second.to_string().contains(secret));
    }
    assert_dir_has_no_secrets(dir.path(), &secrets);
}

#[test]
fn refresh_rejection_requires_sign_in_again_and_drops_the_keyring_entry() {
    let fake = spawn_fake(CallbackFault::None);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(InMemorySecretStore::new());
    let ticks = Arc::new(AtomicU64::new(START));
    let oauth = binding(
        "docs",
        Arc::clone(&store),
        dir.path(),
        clock(Arc::clone(&ticks)),
    );
    login(&fake, &oauth);
    let mut secrets = remembered_secrets(&fake, &store, "docs");
    fake.state.lock().unwrap().break_refresh = true;
    ticks.store(START + 120, Ordering::SeqCst);
    let mut client = McpHttpClient::with_oauth(&fake.mcp, oauth.clone()).unwrap();
    let error = client.initialize().unwrap_err();
    assert!(error.to_string().contains("sign-in"));
    assert!(store.get(&mcp_oauth_key("docs")).unwrap().is_none());
    let catalog = McpServerCatalog::load(&oauth.catalog_path).unwrap();
    assert_eq!(
        catalog.servers[0].auth_status,
        McpLoginStatus::ReLoginRequired
    );
    assert_eq!(
        catalog.servers[0].auth_detail,
        MCP_AUTH_DETAIL_REFRESH_REJECTED
    );
    secrets.retain(|secret| secret.len() >= 8);
    for secret in &secrets {
        assert!(!error.to_string().contains(secret));
    }
    assert_dir_has_no_secrets(dir.path(), &secrets);
}

#[test]
fn callback_state_and_resource_mismatch_store_nothing() {
    for fault in [CallbackFault::BadState, CallbackFault::BadResource] {
        let fake = spawn_fake(fault);
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemorySecretStore::new());
        let oauth = binding(
            "docs",
            Arc::clone(&store),
            dir.path(),
            clock(Arc::new(AtomicU64::new(START))),
        );
        let resource = fake.mcp.clone();
        let error = login_with_browser(
            &fake.mcp,
            &PkceLoginOptions {
                binding: oauth.clone(),
                client_id: None,
                scopes: vec!["mcp".into()],
                timeout: Duration::from_secs(5),
            },
            &move |url| browser(&resource, url),
        )
        .unwrap_err();
        assert!(store.get(&mcp_oauth_key("docs")).unwrap().is_none());
        assert_eq!(fake.state.lock().unwrap().refresh_count, 0);
        let catalog = McpServerCatalog::load(&oauth.catalog_path).unwrap();
        assert_eq!(catalog.servers[0].auth_status, McpLoginStatus::LoggedOut);
        let secrets = remembered_secrets(&fake, &store, "docs");
        assert!(!error.to_string().contains("termior-test-code"));
        assert_dir_has_no_secrets(dir.path(), &secrets);
    }
}

#[test]
fn system_time_is_not_required_to_format_a_binding() {
    let rendered = format!(
        "{:?}",
        McpOAuthBinding {
            server_id: "docs".into(),
            store: Arc::new(InMemorySecretStore::new()),
            catalog_path: Path::new("mcp-servers.json").into(),
            now: system_clock(),
            diagnostic_log: None,
        }
    );
    assert!(rendered.contains("docs"));
    assert!(!rendered.contains("access_token"));
}
