//! The few Telegram Bot API calls sync needs.
//!
//! A bot never receives its own messages, and only one client may poll `getUpdates`, so
//! devices don't talk through messages: the latest snapshot is a document pinned in the chat
//! with the bot, read with `getChat`. `getUpdates` is only used during setup.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_API: &str = "https://api.telegram.org";

#[derive(Debug, Clone, Deserialize)]
pub struct BotInfo {
    pub id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub first_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub document: Option<Document>,
}

/// Who pressed Start in the bot's chat during setup.
#[derive(Debug, Clone)]
pub struct Contact {
    pub chat_id: i64,
    pub user_id: i64,
    pub name: String,
}

pub struct Bot {
    token: String,
    base: String,
    agent: ureq::Agent,
}

impl Bot {
    /// `BK_TELEGRAM_API` replaces `https://api.telegram.org` (tests, self-hosted Bot API servers).
    pub fn new(token: &str) -> Bot {
        let base = std::env::var("BK_TELEGRAM_API")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_API.to_string())
            .trim_end_matches('/')
            .to_string();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(90)))
            .http_status_as_error(false)
            .build();
        Bot { token: token.trim().to_string(), base, agent: config.into() }
    }

    /// Errors must never show the token (it's in every URL).
    fn scrub(&self, e: impl std::fmt::Display) -> anyhow::Error {
        anyhow::anyhow!("{}", e.to_string().replace(&self.token, "<token>"))
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    fn result(&self, v: Value) -> Result<Value> {
        if v["ok"] == true {
            return Ok(v["result"].clone());
        }
        let desc = v["description"].as_str().unwrap_or("unknown error");
        match v["error_code"].as_i64() {
            Some(401) | Some(404) => bail!("Telegram doesn't accept this bot token ({desc})"),
            _ => bail!("Telegram: {desc}"),
        }
    }

    fn read_json(&self, resp: &mut ureq::http::Response<ureq::Body>, what: &str) -> Result<Value> {
        let text = resp.body_mut().read_to_string().map_err(|e| self.scrub(format!("{what}: {e}")))?;
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{what}: unexpected reply ({e})"))
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut resp = self
            .agent
            .post(&self.url(method))
            .header("Content-Type", "application/json")
            .send(params.to_string().as_bytes())
            .map_err(|e| self.scrub(format!("can't reach Telegram: {e}")))?;
        let v = self.read_json(&mut resp, &format!("Telegram {method}"))?;
        self.result(v)
    }

    pub fn get_me(&self) -> Result<BotInfo> {
        Ok(serde_json::from_value(self.call("getMe", json!({}))?)?)
    }

    /// Wait up to `timeout` for someone to message the bot privately (pressing Start counts).
    /// Pending updates are acknowledged so they aren't seen again.
    pub fn wait_for_contact(&self, timeout: Duration) -> Result<Contact> {
        let deadline = std::time::Instant::now() + timeout;
        let mut offset: Option<i64> = None;
        while std::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs().clamp(1, 25);
            let mut params = json!({ "timeout": left, "allowed_updates": ["message"] });
            if let Some(o) = offset {
                params["offset"] = o.into();
            }
            let updates = self.call("getUpdates", params)?;
            for u in updates.as_array().into_iter().flatten() {
                if let Some(id) = u["update_id"].as_i64() {
                    offset = Some(id + 1);
                }
                let m = &u["message"];
                if m["chat"]["type"] == "private"
                    && let (Some(chat_id), Some(user_id)) = (m["chat"]["id"].as_i64(), m["from"]["id"].as_i64())
                {
                    let name = m["from"]["first_name"].as_str().unwrap_or_default().to_string();
                    // Confirm what we read.
                    let _ = self.call("getUpdates", json!({ "offset": offset, "timeout": 0 }));
                    return Ok(Contact { chat_id, user_id, name });
                }
            }
        }
        bail!("nobody messaged the bot in time; open the bot in Telegram, press Start, and try again")
    }

    /// The message pinned in the chat, if any.
    pub fn pinned(&self, chat_id: i64) -> Result<Option<Message>> {
        let chat = self.call("getChat", json!({ "chat_id": chat_id }))?;
        match chat.get("pinned_message") {
            Some(m) if !m.is_null() => Ok(Some(serde_json::from_value(m.clone())?)),
            _ => Ok(None),
        }
    }

    pub fn download(&self, file_id: &str) -> Result<Vec<u8>> {
        let f = self.call("getFile", json!({ "file_id": file_id }))?;
        let path = f["file_path"].as_str().context("Telegram gave no file path")?;
        let url = format!("{}/file/bot{}/{path}", self.base, self.token);
        let mut resp =
            self.agent.get(&url).call().map_err(|e| self.scrub(format!("downloading from Telegram: {e}")))?;
        if resp.status() != 200 {
            bail!("downloading from Telegram failed ({})", resp.status());
        }
        resp.body_mut()
            .with_config()
            .limit(25 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| self.scrub(format!("downloading from Telegram: {e}")))
    }

    /// Upload a file to the chat; returns the new message's id.
    pub fn send_document(&self, chat_id: i64, file_name: &str, caption: &str, bytes: &[u8]) -> Result<i64> {
        let boundary = format!("clibooker{:016x}", u64::from_le_bytes(rand_bytes()));
        let mut body = Vec::with_capacity(bytes.len() + 512);
        for (name, value) in [
            ("chat_id", chat_id.to_string()),
            ("caption", caption.to_string()),
            ("disable_notification", "true".into()),
        ] {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"{file_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut resp = self
            .agent
            .post(&self.url("sendDocument"))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..])
            .map_err(|e| self.scrub(format!("uploading to Telegram: {e}")))?;
        let v = self.read_json(&mut resp, "uploading to Telegram")?;
        self.result(v)?["message_id"].as_i64().context("Telegram returned no message id")
    }

    pub fn pin(&self, chat_id: i64, message_id: i64) -> Result<()> {
        self.call(
            "pinChatMessage",
            json!({ "chat_id": chat_id, "message_id": message_id, "disable_notification": true }),
        )?;
        Ok(())
    }

    /// Best effort: tidy up an older snapshot (bots may only delete messages under 48h old).
    pub fn retire(&self, chat_id: i64, message_id: i64) {
        let _ = self.call("unpinChatMessage", json!({ "chat_id": chat_id, "message_id": message_id }));
        let _ = self.call("deleteMessage", json!({ "chat_id": chat_id, "message_id": message_id }));
    }

    pub fn send_message(&self, chat_id: i64, text: &str) -> Result<()> {
        self.call("sendMessage", json!({ "chat_id": chat_id, "text": text, "disable_notification": true }))?;
        Ok(())
    }
}

fn rand_bytes() -> [u8; 8] {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b
}
