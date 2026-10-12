//! Client for the loopback sync socket.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::{port_file, token_file};

pub struct SyncClient {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl SyncClient {
    /// Connect and authenticate against the running sync server.
    pub async fn connect() -> Result<Self, String> {
        let port: u16 = std::fs::read_to_string(port_file())
            .map_err(|_| "no sync server is running (missing sync.port file)".to_string())?
            .trim()
            .parse()
            .map_err(|_| "invalid sync.port file".to_string())?;
        let token = std::fs::read_to_string(token_file())
            .map_err(|_| "no sync server token (missing sync.token file)".to_string())?
            .trim()
            .to_string();

        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|err| format!("could not connect to sync server on port {port}: {err}"))?;
        let (read_half, mut write_half) = stream.into_split();
        write_half
            .write_all(format!("{{\"token\":\"{token}\"}}\n").as_bytes())
            .await
            .map_err(|err| err.to_string())?;

        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|err| err.to_string())?;
        let welcome: Value = serde_json::from_str(&line).map_err(|err| err.to_string())?;
        if welcome.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(welcome
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("authentication failed")
                .to_string());
        }

        Ok(Self {
            reader,
            writer: write_half,
            next_id: 1,
        })
    }

    /// Send a command and wait for its matching response.
    pub async fn request(&mut self, cmd: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({ "id": id, "cmd": cmd, "params": params }).to_string();
        message.push('\n');
        self.writer
            .write_all(message.as_bytes())
            .await
            .map_err(|err| err.to_string())?;

        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|err| err.to_string())?;
            if read == 0 {
                return Err("sync server closed the connection".to_string());
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if value.get("ok").and_then(Value::as_bool) == Some(true) {
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                return Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed")
                    .to_string());
            }
        }
    }

    /// Read the next pushed event (used after `subscribe`).
    pub async fn next_event(&mut self) -> Result<Option<Value>, String> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|err| err.to_string())?;
        if read == 0 {
            return Ok(None);
        }
        Ok(serde_json::from_str(&line).ok())
    }
}
