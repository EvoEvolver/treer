//! Fixed-origin Bot API client. Never expose reqwest errors (URLs contain tokens).
use crate::telegram::{Error, Incoming, Target, Transport, Update};
use reqwest::{blocking::Client, redirect::Policy};
use serde_json::{json, Value};
use std::{io::Read, time::Duration};
const RESPONSE_LIMIT: u64 = 1024 * 1024;

pub struct BotClient {
    client: Client,
    token: String,
    bot_id: i64,
    account_id: String,
    verified: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    test_origin: Option<String>,
}
impl BotClient {
    pub fn new(account_id: String, token: String, bot_id: i64) -> Result<Self, Error> {
        if !crate::store::valid_id(&account_id) || !valid_token(&token, bot_id) {
            return Err(Error::Invalid);
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .user_agent("Treer-Message-Hub/0.1")
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            client,
            token,
            bot_id,
            account_id,
            verified: false.into(),
            #[cfg(test)]
            test_origin: None,
        })
    }
    fn request(&self, operation: &str, body: Value) -> Result<Value, Error> {
        // Caller cannot supply an operation or URL through an HTTP/API parameter.
        if !matches!(operation, "getMe" | "getUpdates" | "sendMessage") {
            return Err(Error::Invalid);
        }
        #[cfg(test)]
        let origin = self
            .test_origin
            .as_deref()
            .unwrap_or("https://api.telegram.org");
        #[cfg(not(test))]
        let origin = "https://api.telegram.org";
        let url = format!("{origin}/bot{}/{operation}", self.token);
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .map_err(|_| Error::Transport)?;
        let status = response.status();
        if matches!(status.as_u16(), 400 | 401 | 403 | 404) {
            return Err(Error::Rejected);
        }
        if status.as_u16() == 429 {
            let mut bytes = Vec::new();
            let read = response.take(RESPONSE_LIMIT + 1).read_to_end(&mut bytes);
            let delay = if read.is_ok() && bytes.len() as u64 <= RESPONSE_LIMIT {
                serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|v| v.get("parameters")?.get("retry_after")?.as_u64())
                    .map(|n| n.clamp(1, 86400) as u32)
                    .unwrap_or(60)
            } else {
                60
            };
            return Err(Error::RateLimited(delay));
        }
        if !status.is_success() {
            return Err(Error::Transport);
        }
        if response
            .content_length()
            .is_some_and(|size| size > RESPONSE_LIMIT)
        {
            return Err(Error::Transport);
        }
        let mut bytes = Vec::new();
        response
            .take(RESPONSE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Transport)?;
        if bytes.len() as u64 > RESPONSE_LIMIT {
            return Err(Error::Transport);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Transport)?;
        if value.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::Transport);
        }
        value.get("result").cloned().ok_or(Error::Transport)
    }
    /// Must succeed before polling/sending; verifies the configured bot identity.
    pub fn verify(&self) -> Result<(), Error> {
        self.verified
            .store(false, std::sync::atomic::Ordering::Release);
        let me = self.request("getMe", json!({}))?;
        if me.get("id").and_then(Value::as_i64) != Some(self.bot_id)
            || me.get("is_bot") != Some(&Value::Bool(true))
        {
            return Err(Error::Invalid);
        }
        self.verified
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
    /// Runtime supplies ONLY the durable account offset, never a browser value.
    pub fn poll(&self, stored_offset: i64) -> Result<Vec<Update>, Error> {
        if stored_offset < 0 || !self.verified.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::Invalid);
        }
        let result = self.request(
            "getUpdates",
            json!({"offset":stored_offset,"limit":100,"timeout":5,"allowed_updates":["message"]}),
        )?;
        normalize_updates(&result)
    }
}
impl Transport for BotClient {
    fn send(&self, target: &Target, text: &str) -> Result<i64, Error> {
        if !self.verified.load(std::sync::atomic::Ordering::Acquire)
            || target.account_id != self.account_id
            || target.chat_id == 0
            || target.topic_id.is_some_and(|n| n <= 0)
            || text.trim().is_empty()
            || text.encode_utf16().count() > 3500
        {
            return Err(Error::Invalid);
        }
        let mut body = json!({"chat_id":target.chat_id,"text":text,"link_preview_options":{"is_disabled":true}});
        if let Some(topic) = target.topic_id {
            body["message_thread_id"] = topic.into();
        }
        let result = self.request("sendMessage", body)?;
        if result
            .get("chat")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_i64)
            != Some(target.chat_id)
        {
            return Err(Error::Transport);
        }
        let id = result
            .get("message_id")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or(Error::Transport)?;
        Ok(id)
    }
}
fn valid_token(token: &str, bot_id: i64) -> bool {
    token.len() <= 256
        && bot_id > 0
        && token.split_once(':').is_some_and(|(prefix, secret)| {
            prefix.parse::<i64>().ok() == Some(bot_id)
                && !prefix.is_empty()
                && prefix.bytes().all(|b| b.is_ascii_digit())
                && secret.len() >= 20
                && secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}
fn normalize_updates(result: &Value) -> Result<Vec<Update>, Error> {
    let rows = result
        .as_array()
        .filter(|r| r.len() <= 100)
        .ok_or(Error::Invalid)?;
    let mut updates = Vec::with_capacity(rows.len());
    let mut previous = None;
    for row in rows {
        let id = row
            .get("update_id")
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0 && *n < i64::MAX)
            .ok_or(Error::Invalid)?;
        if previous.is_some_and(|old| old >= id) {
            return Err(Error::Invalid);
        }
        previous = Some(id);
        let message = row.get("message").and_then(normalize_message);
        updates.push(Update {
            update_id: id,
            message,
        });
    }
    Ok(updates)
}
fn normalize_message(message: &Value) -> Option<Incoming> {
    // Strict text-only first release. An anonymous admin is bot-authored and/or
    // carries sender_chat. Forwarded origin fields never provide identity.
    if [
        "sender_chat",
        "forward_origin",
        "forward_from",
        "forward_from_chat",
    ]
    .iter()
    .any(|key| message.get(key).is_some())
    {
        return None;
    }
    let from = message.get("from")?;
    if from.get("is_bot") != Some(&Value::Bool(false)) {
        return None;
    }
    let sender_id = from.get("id")?.as_i64().filter(|n| *n > 0)?;
    let chat_id = message
        .get("chat")?
        .get("id")?
        .as_i64()
        .filter(|n| *n != 0)?;
    let message_id = message.get("message_id")?.as_i64().filter(|n| *n > 0)?;
    let topic_id = match message.get("message_thread_id") {
        None => None,
        Some(value) => Some(value.as_i64().filter(|n| *n > 0)?),
    };
    let text = message
        .get("text")?
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= crate::protocol::MAX_BODY)?
        .into();
    Some(Incoming {
        message_id,
        chat_id,
        topic_id,
        sender_id,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
    };
    const TOKEN: &str = "12345:synthetic_abcdefghijklmnopqrst";
    struct Server {
        origin: String,
        requests: Arc<Mutex<Vec<String>>>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Server {
        fn start(responses: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let capture = requests.clone();
            let redirect_origin = origin.clone();
            let thread = thread::spawn(move || {
                for (status, body) in responses {
                    let until = std::time::Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((s, _)) => break s,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    std::time::Instant::now() < until,
                                    "synthetic request deadline"
                                );
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => panic!("synthetic accept"),
                        }
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let request = read_request(&mut stream);
                    capture.lock().unwrap().push(request);
                    let length = if status == 299 {
                        String::new()
                    } else {
                        format!("Content-Length: {}\r\n", body.len())
                    };
                    let location = if status == 302 {
                        format!("Location: {redirect_origin}/should-not-follow\r\n")
                    } else {
                        String::new()
                    };
                    let response=format!("HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Type: application/json\r\n{length}{location}\r\n{body}");
                    let _ = stream.write_all(response.as_bytes());
                    drop(stream);
                    if status == 302 {
                        let deadline = std::time::Instant::now() + Duration::from_millis(200);
                        while std::time::Instant::now() < deadline {
                            assert!(
                                matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
                                "client followed redirect"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
            });
            Self {
                origin,
                requests,
                thread: Some(thread),
            }
        }
        fn client(&self) -> BotClient {
            let mut client = BotClient::new("test".into(), TOKEN.into(), 12345).unwrap();
            client.test_origin = Some(self.origin.clone());
            client
        }
        fn finish(mut self) -> Vec<String> {
            self.thread.take().unwrap().join().unwrap();
            self.requests.lock().unwrap().clone()
        }
    }
    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut block = [0; 4096];
        loop {
            let n = stream.read(&mut block).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&block[..n]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    return String::from_utf8(bytes).unwrap();
                }
            }
        }
    }
    fn me() -> (u16, String) {
        (
            200,
            json!({"ok":true,"result":{"id":12345,"is_bot":true}}).to_string(),
        )
    }
    fn message() -> Value {
        json!({"message_id":7,"chat":{"id":-100},"from":{"id":42,"is_bot":false},"message_thread_id":9,"text":"<script> is plain text"})
    }
    #[test]
    fn fixed_operations_use_verified_bot_stored_offset_and_plain_text() {
        let server = Server::start(vec![
            me(),
            (
                200,
                json!({"ok":true,"result":[{"update_id":99,"message":message()}]}).to_string(),
            ),
            (
                200,
                json!({"ok":true,"result":{"message_id":8,"chat":{"id":-100}}}).to_string(),
            ),
        ]);
        let client = server.client();
        assert!(client.poll(99).is_err());
        client.verify().unwrap();
        let updates = client.poll(99).unwrap();
        assert_eq!(updates[0].message.as_ref().unwrap().sender_id, 42);
        let target = Target {
            account_id: "test".into(),
            chat_id: -100,
            topic_id: Some(9),
            allowed_senders: vec![42],
        };
        let mut wrong = target.clone();
        wrong.account_id = "other".into();
        assert!(client.send(&wrong, "body").is_err());
        assert_eq!(client.send(&target, "<b>plain</b>").unwrap(), 8);
        let requests = server.finish();
        assert!(requests[0].starts_with(&format!("POST /bot{TOKEN}/getMe ")));
        let body: Value =
            serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["offset"], 99);
        assert_eq!(body["allowed_updates"], json!(["message"]));
        let body: Value =
            serde_json::from_str(requests[2].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["text"], "<b>plain</b>");
        assert_eq!(body["message_thread_id"], 9);
        assert!(body.get("parse_mode").is_none());
    }
    #[test]
    fn redirect_malformed_oversized_and_wrong_bot_results_are_fixed_errors() {
        for response in [
            (302, "".into()),
            (200, "not-json".into()),
            (200, "x".repeat(RESPONSE_LIMIT as usize + 1)),
            (299, "x".repeat(RESPONSE_LIMIT as usize + 1)),
            (
                200,
                json!({"ok":true,"result":{"id":999,"is_bot":true}}).to_string(),
            ),
            (401, json!({"ok":false,"description":TOKEN}).to_string()),
        ] {
            let server = Server::start(vec![response]);
            let client = server.client();
            let error = client.verify().unwrap_err().to_string();
            assert!(!error.contains(TOKEN));
            assert!(!error.contains("http"));
            assert_eq!(server.finish().len(), 1);
        }
    }
    #[test]
    fn rate_limits_and_permanent_statuses_have_fixed_typed_errors() {
        for (status, body, delay) in [
            (
                429,
                json!({"ok":false,"parameters":{"retry_after":600},"description":TOKEN})
                    .to_string(),
                Some(600),
            ),
            (429, "not-json".into(), Some(60)),
            (403, TOKEN.into(), None),
        ] {
            let server = Server::start(vec![(status, body)]);
            let error = server.client().verify().unwrap_err();
            match (error, delay) {
                (Error::RateLimited(actual), Some(expected)) => assert_eq!(actual, expected),
                (Error::Rejected, None) => {}
                _ => panic!("wrong fixed error class"),
            }
            assert_eq!(server.finish().len(), 1);
        }
    }
    #[test]
    fn normalizer_rejects_spoofed_anonymous_bot_forwarded_and_nonmessage_origins() {
        for key in [
            "sender_chat",
            "forward_origin",
            "forward_from",
            "forward_from_chat",
        ] {
            let mut message = message();
            message[key] = json!({"id":42});
            assert!(normalize_message(&message).is_none());
        }
        for from in [
            json!({"id":42,"is_bot":true}),
            json!({"id":1087968824,"is_bot":true}),
            json!({"id":42}),
            json!({"id":-42,"is_bot":false}),
        ] {
            let mut message = message();
            message["from"] = from;
            assert!(normalize_message(&message).is_none());
        }
        let rows = json!([{"update_id":1,"edited_message":message()},{"update_id":2,"channel_post":message()},{"update_id":3,"callback_query":{"from":{"id":42}}}]);
        assert!(normalize_updates(&rows)
            .unwrap()
            .iter()
            .all(|u| u.message.is_none()));
    }
    #[test]
    fn update_and_token_bounds_do_not_accept_url_or_identity_injection() {
        for token in [
            "12345:abc",
            "999:synthetic_abcdefghijklmnopqrst",
            "12345:synthetic_abcdefghij/../../getMe",
            "12345:synthetic_abcdefghij?token=x",
        ] {
            assert!(BotClient::new("test".into(), token.into(), 12345).is_err());
        }
        for rows in [
            json!([{"update_id":1},{"update_id":1}]),
            json!([{"update_id":2},{"update_id":1}]),
            json!([{"update_id":-1}]),
            json!([{"update_id":9223372036854775807_i64}]),
        ] {
            assert!(normalize_updates(&rows).is_err());
        }
        let mut main = message();
        main.as_object_mut().unwrap().remove("message_thread_id");
        assert!(normalize_message(&main).unwrap().topic_id.is_none());
        main["message_thread_id"] = 0.into();
        assert!(normalize_message(&main).is_none());
    }
}
