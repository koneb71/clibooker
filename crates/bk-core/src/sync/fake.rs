//! A tiny in-process stand-in for the Telegram Bot API, for tests: the endpoints sync uses,
//! one private chat, pins, file uploads and downloads. Point `BK_TELEGRAM_API` at `url()`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

pub const TOKEN: &str = "123456:TEST-token";
pub const CHAT_ID: i64 = 4242;
pub const USER_ID: i64 = 4242;

#[derive(Default)]
struct World {
    next_message: i64,
    next_update: i64,
    /// message id → (caption, file id)
    messages: BTreeMap<i64, (String, Option<String>)>,
    files: BTreeMap<String, Vec<u8>>,
    pinned: Option<i64>,
    /// Pending `/start` updates (id, sent?)
    updates: Vec<i64>,
    texts: Vec<String>,
}

pub struct FakeTelegram {
    port: u16,
    world: Arc<Mutex<World>>,
}

impl FakeTelegram {
    pub fn start() -> FakeTelegram {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake telegram");
        let port = listener.local_addr().expect("addr").port();
        let world = Arc::new(Mutex::new(World { next_message: 1, next_update: 100, ..Default::default() }));
        let w = world.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let w = w.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, &w);
                });
            }
        });
        FakeTelegram { port, world }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The user presses Start in the bot's chat.
    pub fn press_start(&self) {
        let mut w = self.world.lock().unwrap();
        let id = w.next_update;
        w.next_update += 1;
        w.updates.push(id);
    }

    pub fn pinned_message(&self) -> Option<i64> {
        self.world.lock().unwrap().pinned
    }

    /// Number of snapshot files still in the chat.
    pub fn documents(&self) -> usize {
        self.world.lock().unwrap().messages.values().filter(|(_, f)| f.is_some()).count()
    }

    /// The content of the pinned file (to check it's encrypted).
    pub fn pinned_bytes(&self) -> Option<Vec<u8>> {
        let w = self.world.lock().unwrap();
        let file = w.messages.get(&w.pinned?)?.1.clone()?;
        w.files.get(&file).cloned()
    }

    pub fn texts(&self) -> Vec<String> {
        self.world.lock().unwrap().texts.clone()
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn ok(stream: &mut TcpStream, result: Value) -> std::io::Result<()> {
    respond(stream, 200, "application/json", json!({ "ok": true, "result": result }).to_string().as_bytes())
}

fn fail(stream: &mut TcpStream, code: u16, description: &str) -> std::io::Result<()> {
    let body = json!({ "ok": false, "error_code": code, "description": description }).to_string();
    respond(stream, code, "application/json", body.as_bytes())
}

/// Multipart form → name → (filename, bytes).
fn multipart(content_type: &str, body: &[u8]) -> BTreeMap<String, (Option<String>, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let Some(boundary) = content_type.split("boundary=").nth(1) else { return out };
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    while let Some(i) = find(&body[start..], &delim) {
        parts.push(start + i);
        start += i + delim.len();
    }
    for w in parts.windows(2) {
        let part = &body[w[0] + delim.len()..w[1]];
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let part = part.strip_suffix(b"\r\n").unwrap_or(part);
        let Some(split) = find(part, b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&part[..split]).to_string();
        let data = part[split + 4..].to_vec();
        let attr =
            |key: &str| head.split(&format!("{key}=\"")).nth(1).and_then(|r| r.split('"').next()).map(str::to_string);
        if let Some(name) = attr("name") {
            out.insert(name, (attr("filename"), data));
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn handle(mut stream: TcpStream, world: &Arc<Mutex<World>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (mut len, mut content_type) = (0usize, String::new());
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "content-type" => content_type = v.trim().to_string(),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;

    if let Some(rest) = path.strip_prefix(&format!("/file/bot{TOKEN}/documents/")) {
        let data = world.lock().unwrap().files.get(rest).cloned();
        return match data {
            Some(d) => respond(&mut stream, 200, "application/octet-stream", &d),
            None => fail(&mut stream, 404, "Not Found"),
        };
    }
    let Some(method) = path.strip_prefix(&format!("/bot{TOKEN}/")) else {
        return fail(&mut stream, 401, "Unauthorized");
    };
    let params: Value = if content_type.starts_with("application/json") {
        serde_json::from_slice(&body).unwrap_or(json!({}))
    } else {
        json!({})
    };
    let mut w = world.lock().unwrap();
    match method {
        "getMe" => {
            ok(&mut stream, json!({ "id": 1, "is_bot": true, "first_name": "Sync", "username": "clibooker_test_bot" }))
        }
        "getUpdates" => {
            let offset = params["offset"].as_i64().unwrap_or(0);
            w.updates.retain(|id| *id >= offset);
            let result: Vec<Value> = w
                .updates
                .iter()
                .map(|id| {
                    json!({ "update_id": id, "message": {
                        "message_id": 1, "date": 0, "text": "/start",
                        "from": { "id": USER_ID, "is_bot": false, "first_name": "Tester" },
                        "chat": { "id": CHAT_ID, "type": "private" } } })
                })
                .collect();
            ok(&mut stream, Value::Array(result))
        }
        "getChat" => {
            let mut chat = json!({ "id": CHAT_ID, "type": "private" });
            if let Some(id) = w.pinned
                && let Some((caption, file)) = w.messages.get(&id)
            {
                let mut m = json!({ "message_id": id, "date": 0, "caption": caption });
                if let Some(f) = file {
                    m["document"] = json!({ "file_id": f, "file_name": "clibooker-sync.bin" });
                }
                chat["pinned_message"] = m;
            }
            ok(&mut stream, chat)
        }
        "getFile" => {
            let id = params["file_id"].as_str().unwrap_or_default().to_string();
            ok(&mut stream, json!({ "file_id": id, "file_path": format!("documents/{id}") }))
        }
        "sendDocument" => {
            let form = multipart(&content_type, &body);
            if form.get("chat_id").map(|(_, v)| v.as_slice()) != Some(CHAT_ID.to_string().as_bytes()) {
                return fail(&mut stream, 400, "Bad Request: chat not found");
            }
            let caption = form.get("caption").map(|(_, v)| String::from_utf8_lossy(v).to_string()).unwrap_or_default();
            let Some((_, data)) = form.get("document") else { return fail(&mut stream, 400, "no document") };
            let id = w.next_message;
            w.next_message += 1;
            let file_id = format!("file{id}");
            w.files.insert(file_id.clone(), data.clone());
            w.messages.insert(id, (caption, Some(file_id)));
            ok(&mut stream, json!({ "message_id": id, "date": 0 }))
        }
        "pinChatMessage" => {
            let id = params["message_id"].as_i64().unwrap_or(0);
            if !w.messages.contains_key(&id) {
                return fail(&mut stream, 400, "Bad Request: message to pin not found");
            }
            w.pinned = Some(id);
            ok(&mut stream, json!(true))
        }
        "unpinChatMessage" => {
            let id = params["message_id"].as_i64();
            if w.pinned == id {
                w.pinned = None;
            }
            ok(&mut stream, json!(true))
        }
        "deleteMessage" => {
            let id = params["message_id"].as_i64().unwrap_or(0);
            if let Some((_, Some(f))) = w.messages.remove(&id) {
                w.files.remove(&f);
            }
            ok(&mut stream, json!(true))
        }
        "sendMessage" => {
            let text = params["text"].as_str().unwrap_or_default().to_string();
            w.texts.push(text.clone());
            let id = w.next_message;
            w.next_message += 1;
            w.messages.insert(id, (text, None));
            ok(&mut stream, json!({ "message_id": id, "date": 0 }))
        }
        _ => fail(&mut stream, 404, "Not Found: method not found"),
    }
}
