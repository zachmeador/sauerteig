//! Headless WebSocket clients that drive the server's acceptance tests.
use futures_util::{SinkExt, StreamExt};
use sauerteig_core::{
    Cell, CellFragment, ClientMessage, FineBox, RootEdit, ServerMessage, WorldBounds,
};
use std::time::Duration;
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};

pub fn largest_blueprint(bounds: WorldBounds, material: u8) -> Vec<RootEdit> {
    // Maximum request: 16,384 roots and 65,536 fragments, four columns of
    // distinct materials per root so canonicalization cannot collapse the
    // load. Each half-metre column is two cubes in the tree, so the world
    // stays within its piece guard.
    let start = [bounds.min[0] + 2, bounds.max[1] - 18, bounds.min[2] + 2];
    (0..16_384)
        .map(|i| {
            let pos = [
                start[0] + i % 32,
                start[1] + i / 1024,
                start[2] + i / 32 % 32,
            ];
            let root = FineBox::root(pos);
            let fragments = (0..4)
                .map(|column| {
                    let (x, z) = (column % 2 * 4, column / 2 * 4);
                    CellFragment {
                        bounds: FineBox {
                            min: [root.min[0] + x, root.min[1], root.min[2] + z],
                            max: [root.min[0] + x + 4, root.max[1], root.min[2] + z + 4],
                        },
                        origin: root.min,
                        size: 8,
                        cell: Cell::solid((material + column as u8) % 8),
                    }
                })
                .collect();
            RootEdit { pos, fragments }
        })
        .collect()
}

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
/// A small protocol client for acceptance tests.
pub struct Bot {
    pub socket: Socket,
    pub id: u64,
    pub revision: u64,
    pub bounds: WorldBounds,
}
impl Bot {
    pub async fn join(endpoint: &str) -> Result<Self, String> {
        let (socket, _) = connect_async_with_config(
            endpoint,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_frame_size(Some(64 * 1024 * 1024)),
            ),
            true,
        )
        .await
        .map_err(|e| e.to_string())?;
        let mut bot = Self {
            socket,
            id: 0,
            revision: 0,
            bounds: WorldBounds::default(),
        };
        match bot.receive().await? {
            ServerMessage::Welcome {
                id,
                revision,
                bounds,
                ..
            } => {
                bot.id = id;
                bot.revision = revision;
                bot.bounds = bounds;
                Ok(bot)
            }
            other => Err(format!("Expected welcome, received {other:?}")),
        }
    }
    pub async fn send(&mut self, message: ClientMessage) -> Result<(), String> {
        self.socket
            .send(Message::Text(
                serde_json::to_string(&message)
                    .map_err(|e| e.to_string())?
                    .into(),
            ))
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn receive(&mut self) -> Result<ServerMessage, String> {
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let message = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    match &message {
                        ServerMessage::WorldPatch { revision, .. }
                        | ServerMessage::RootPatch { revision, .. }
                        | ServerMessage::TransactionPatch { revision, .. }
                        | ServerMessage::Welcome { revision, .. } => self.revision = *revision,
                        _ => {}
                    }
                    return Ok(message);
                }
                Some(Ok(Message::Ping(bytes))) => self
                    .socket
                    .send(Message::Pong(bytes))
                    .await
                    .map_err(|e| e.to_string())?,
                Some(Ok(Message::Close(_))) | None => return Err("Connection closed".into()),
                Some(Err(error)) => return Err(error.to_string()),
                _ => {}
            }
        }
    }
    pub async fn until(
        &mut self,
        predicate: impl Fn(&ServerMessage) -> bool,
    ) -> Result<ServerMessage, String> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let message = self.receive().await?;
                if predicate(&message) {
                    return Ok(message);
                }
            }
        })
        .await
        .map_err(|_| "Timed out waiting for server message".to_string())?
    }
    pub async fn edit_mode(&mut self) -> Result<(), String> {
        self.send(ClientMessage::SetMode {
            request: 1,
            editing: true,
        })
        .await?;
        self.until(|m| matches!(m, ServerMessage::ModeChanged { editing: true, .. }))
            .await?;
        Ok(())
    }
}
