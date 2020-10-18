use async_trait::async_trait;
use futures::SinkExt;
use serde::de::DeserializeOwned;
use serde::export::Formatter;
use serde::Deserialize;
use serde::Serialize;
use serde_json::error::Error as JsonError;
use serde_json::{Error, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::stream::StreamExt;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::oneshot::channel as one_shot_channel;
use tokio::sync::oneshot::error::RecvError as OneShotRecvError;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::sync::{Mutex, RwLock};
use tokio::task::spawn;
use tokio::time::{delay_for, Duration};
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::uri::InvalidUri;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::WebSocketStream;

async fn keepalive() {
    delay_for(Duration::from_secs(20)).await;
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JanusMessage<T: Clone + Serialize = ()> {
    #[serde(rename = "janus")]
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<u64>,
    #[serde(
        flatten,
        skip_serializing_if = "JanusMessageData::is_empty",
        bound(deserialize = "JanusMessageData<T>: Deserialize<'de>")
    )]
    pub data: JanusMessageData<T>,
}

impl<T: Serialize + Clone> JanusMessage<T> {
    pub fn extract(self) -> Result<T, JanusError> {
        self.data.extract()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JanusBareMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction: Option<String>,
    #[serde(flatten)]
    data: Value,
}

impl JanusBareMessage {
    pub fn data<T: DeserializeOwned>(self) -> Result<T, JanusError> {
        serde_json::from_value(self.data).map_err(Into::into)
    }

    pub fn extract<T: DeserializeOwned + Clone + Serialize>(self) -> Result<T, JanusError> {
        self.data::<JanusMessageData<T>>()?.extract()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum JanusMessageData<T: Clone + Serialize = ()> {
    Empty,
    Body {
        body: T,
    },
    Data {
        data: T,
    },
    Plugin {
        plugin: String,
    },
    Error {
        error: ErrorData,
    },
    PluginData {
        sender: u64,
        #[serde(rename = "plugindata")]
        plugin_data: PluginData<T>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorData {
    code: usize,
    reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PluginData<T: Clone + Serialize> {
    plugin: String,
    data: T,
}

impl<T: Clone + Serialize> JanusMessageData<T> {
    fn extract(self) -> Result<T, JanusError> {
        match self {
            JanusMessageData::Body { body } => Ok(body),
            JanusMessageData::Data { data } => Ok(data),
            JanusMessageData::PluginData {
                plugin_data: PluginData { data, .. },
                ..
            } => Ok(data),
            JanusMessageData::Error { error } => Err(JanusError::JanusError(error)),
            _ => Err(JanusError::IncorrectResponse),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct JanusIdMessage {
    id: u64,
}

impl<T: Clone + Serialize> JanusMessageData<T> {
    pub fn is_empty(&self) -> bool {
        if let JanusMessageData::Empty = &self {
            return true;
        }

        false
    }
}

#[derive(Clone, Debug)]
pub struct JanusConnection {
    url: Uri,
    websocket_channel: Arc<Mutex<Option<Sender<JanusQueuedMessage>>>>,
    transaction_counter: Arc<AtomicUsize>,
    transactions: Arc<RwLock<HashMap<String, OneShotSender<JanusBareMessage>>>>,
    sessions: Arc<RwLock<HashSet<u64>>>,
}

#[derive(Debug)]
pub enum JanusError {
    Serde(JsonError),
    WebSocket(WebSocketError),
    OneShotChannelError(OneShotRecvError),
    Closed,
    IncorrectResponse,
    MissingTransactionId,
    JanusError(ErrorData),
}

impl fmt::Display for JanusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            JanusError::Serde(error) => write!(f, "Failure with JSON: {}", error),
            JanusError::WebSocket(error) => write!(f, "Failure with WebSocket: {}", error),
            JanusError::Closed => write!(f, "Janus connection is closed"),
            JanusError::IncorrectResponse => {
                write!(f, "Janus responded with something different than expected")
            }
            JanusError::OneShotChannelError(error) => write!(f, "Failure with callback: {}", error),
            JanusError::MissingTransactionId => {
                write!(f, "Tried to send message without transaction id")
            }
            JanusError::JanusError(error) => {
                write!(f, "Janus returned error: ({}) {}", error.code, error.reason)
            }
        }
    }
}

impl From<JsonError> for JanusError {
    fn from(error: Error) -> Self {
        JanusError::Serde(error)
    }
}

impl From<WebSocketError> for JanusError {
    fn from(error: WebSocketError) -> Self {
        JanusError::WebSocket(error)
    }
}

impl From<OneShotRecvError> for JanusError {
    fn from(error: OneShotRecvError) -> Self {
        JanusError::OneShotChannelError(error)
    }
}

async fn send_message<T: AsyncRead + AsyncWrite + Unpin>(
    msg: &JanusBareMessage,
    websocket: &mut WebSocketStream<T>,
) -> Result<(), JanusError> {
    let json = serde_json::to_string(msg)?;
    println!("Janus <- {}", json);
    websocket
        .send(Message::Text(json))
        .await
        .map_err(Into::into)
}

#[derive(Debug)]
struct JanusQueuedMessage(
    JanusBareMessage,
    Option<OneShotSender<Result<(), JanusError>>>,
);

impl JanusQueuedMessage {
    async fn send<T: AsyncWrite + AsyncRead + Unpin>(self, websocket: &mut WebSocketStream<T>) {
        let JanusQueuedMessage(msg, sender) = self;
        let msg = send_message(&msg, websocket).await;
        if let Some(sender) = sender {
            let _ = sender.send(msg);
        }
    }
}

#[derive(Debug)]
pub struct JanusSession {
    connection: JanusConnection,
    session_id: u64,
}

impl Drop for JanusSession {
    fn drop(&mut self) {
        let conn = self.connection.clone();
        let session_id = self.session_id;
        spawn(async move {
            conn.remove_session(session_id).await;
        });
    }
}

impl JanusSession {
    pub async fn attach(&self, plugin: &str) -> Result<JanusPlugin<'_>, JanusError> {
        let res = self
            .send(
                "attach",
                JanusMessageData::<()>::Plugin {
                    plugin: plugin.to_string(),
                },
            )
            .await?;
        let id = res
            .data::<JanusMessageData<JanusIdMessage>>()?
            .extract()?
            .id;

        Ok(JanusPlugin {
            connection: self.connection.clone(),
            session: &self,
            handle_id: id,
        })
    }

    pub async fn attach_owned(
        self: &Arc<Self>,
        plugin: &str,
    ) -> Result<JanusPluginOwned, JanusError> {
        let res = self
            .send(
                "attach",
                JanusMessageData::<()>::Plugin {
                    plugin: plugin.to_string(),
                },
            )
            .await?;
        let id = res
            .data::<JanusMessageData<JanusIdMessage>>()?
            .extract()?
            .id;

        Ok(JanusPluginOwned {
            connection: self.connection.clone(),
            session: Arc::clone(&self),
            handle_id: id,
        })
    }

    pub async fn send<T: Clone + Serialize>(
        &self,
        action: &str,
        data: JanusMessageData<T>,
    ) -> Result<JanusBareMessage, JanusError> {
        self.connection
            .send(&JanusMessage {
                action: action.to_string(),
                session_id: Some(self.session_id.clone()),
                handle_id: None,
                data,
            })
            .await
    }
}

#[async_trait]
pub trait JanusPluginOwner {
    fn handle_id(&self) -> u64;
    fn connection(&self) -> &JanusConnection;
    fn session(&self) -> &JanusSession;

    async fn send<T: Clone + Serialize + Send + Sync>(
        &self,
        action: &str,
        data: JanusMessageData<T>,
    ) -> Result<JanusBareMessage, JanusError> {
        self.connection()
            .send(&JanusMessage {
                action: action.to_string(),
                session_id: Some(self.session().session_id),
                handle_id: Some(self.handle_id()),
                data,
            })
            .await
    }
}

#[derive(Debug)]
pub struct JanusPlugin<'a> {
    connection: JanusConnection,
    session: &'a JanusSession,
    handle_id: u64,
}

impl JanusPluginOwner for JanusPlugin<'_> {
    fn handle_id(&self) -> u64 {
        self.handle_id
    }

    fn connection(&self) -> &JanusConnection {
        &self.connection
    }

    fn session(&self) -> &JanusSession {
        &self.session
    }
}

#[derive(Debug, Clone)]
pub struct JanusPluginOwned {
    connection: JanusConnection,
    session: Arc<JanusSession>,
    handle_id: u64,
}

impl JanusPluginOwner for JanusPluginOwned {
    fn handle_id(&self) -> u64 {
        self.handle_id
    }

    fn connection(&self) -> &JanusConnection {
        &self.connection
    }

    fn session(&self) -> &JanusSession {
        self.session.as_ref()
    }
}

impl JanusConnection {
    pub fn new<T: Into<Uri>>(url: T) -> JanusConnection {
        JanusConnection {
            url: url.into(),
            websocket_channel: Arc::new(Mutex::new(None)),
            transaction_counter: Default::default(),
            transactions: Default::default(),
            sessions: Arc::new(Default::default()),
        }
    }

    pub fn parse(url: &str) -> Result<JanusConnection, InvalidUri> {
        Ok(Self::new(url.parse::<Uri>()?))
    }

    pub async fn start(&self) {
        if self.websocket_channel.lock().await.is_some() {
            return;
        }

        let janus_conn = self.clone();
        let (sender, receiver) = channel::<JanusQueuedMessage>(5);
        self.websocket_channel.lock().await.replace(sender);
        spawn(async move {
            janus_conn.consume(receiver).await;
        });
    }

    async fn consume(&self, mut receiver: Receiver<JanusQueuedMessage>) {
        let req = Request::get(&self.url)
            .header("Sec-WebSocket-Protocol", "janus-protocol")
            .body(())
            .unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

        println!("Starting consumption loop");
        loop {
            tokio::select! {
                res = ws.next() => {
                    println!("Received message: {:?}", res);
                    match res {
                        Some(Ok(frame)) => { self.handle_message(frame).await.unwrap(); },
                        _ => {
                            // ????
                            break;
                        }
                    }
                }

                queued = receiver.recv() => {
                    println!("Sending message: {:?}", queued);
                    if let Some(queued) = queued {
                        queued.send(&mut ws).await;
                    } else {
                        break;
                    }
                }

                _ = keepalive() => {
                    println!("Sending keepalive");
                    let session_ids = self.sessions.read().await.iter().copied().collect::<Vec<_>>();
                    let mut msg: JanusMessage<()> = JanusMessage {
                        action: "keepalive".to_string(),
                        session_id: None,
                        handle_id: None,
                        data: JanusMessageData::Empty,
                    };

                    for session_id in session_ids {
                        msg.session_id = Some(session_id);
                        let _ = self.send_without_response(&msg).await;
                    }
                }
            }
        }

        self.websocket_channel.lock().await.take();
    }

    pub async fn remove_session(&self, session_id: u64) {
        self.sessions.write().await.remove(&session_id);
    }

    pub async fn create_session(&self) -> Result<JanusSession, JanusError> {
        let item: JanusMessage<JanusIdMessage> = self
            .send(&JanusMessage {
                action: "create".to_string(),
                session_id: None,
                handle_id: None,
                data: JanusMessageData::<()>::Empty,
            })
            .await?
            .data()?;

        let session_id = item.extract()?.id;
        self.sessions.write().await.insert(session_id);
        Ok(JanusSession {
            connection: self.clone(),
            session_id,
        })
    }

    async fn push_queue(&self, queue_item: JanusQueuedMessage) -> Result<(), JanusError> {
        if let Some(channel) = self.websocket_channel.lock().await.as_mut() {
            let _ = channel.send(queue_item).await;
        } else {
            return Err(JanusError::Closed);
        }

        Ok(())
    }

    async fn queue_message(&self, message: JanusBareMessage) -> Result<(), JanusError> {
        let (sender, receiver) = one_shot_channel();
        let queue = JanusQueuedMessage(message, Some(sender));
        self.push_queue(queue).await?;
        receiver.await.map_err(Into::into).and_then(|x| x)
    }

    pub async fn send_bare_without_response(
        &self,
        message: JanusBareMessage,
    ) -> Result<(), JanusError> {
        self.push_queue(JanusQueuedMessage(message, None)).await
    }

    pub async fn send_without_response<T: Clone + Serialize>(
        &self,
        message: &JanusMessage<T>,
    ) -> Result<(), JanusError> {
        self.ensure_open_session(message).await?;
        self.send_bare_without_response(self.create_bare_message(message)?)
            .await
    }

    async fn ensure_open_session<T: Clone + Serialize>(
        &self,
        message: &JanusMessage<T>,
    ) -> Result<(), JanusError> {
        if let Some(session_id) = message.session_id {
            if !self.sessions.read().await.contains(&session_id) {
                return Err(JanusError::Closed);
            }
        }
        Ok(())
    }

    pub async fn send_bare(
        &self,
        message: JanusBareMessage,
    ) -> Result<JanusBareMessage, JanusError> {
        println!("Sending {:?}", message);
        if let Some(transaction_id) = &message.transaction {
            let (sender, receiver) = one_shot_channel::<JanusBareMessage>();
            {
                self.transactions
                    .write()
                    .await
                    .insert(transaction_id.clone(), sender);
            }
            self.queue_message(message).await?;
            receiver.await.map_err(Into::into)
        } else {
            return Err(JanusError::MissingTransactionId);
        }
    }

    fn create_bare_message<T: Clone + Serialize>(
        &self,
        message: &JanusMessage<T>,
    ) -> Result<JanusBareMessage, JanusError> {
        let id = self.transaction_counter.fetch_add(1, Ordering::Relaxed);
        let transaction_id = format!("{:x}", id);
        Ok(JanusBareMessage {
            transaction: Some(transaction_id.clone()),
            data: serde_json::to_value(message)?,
        })
    }

    pub async fn send<T: Clone + Serialize>(
        &self,
        message: &JanusMessage<T>,
    ) -> Result<JanusBareMessage, JanusError> {
        self.ensure_open_session(message).await?;
        self.send_bare(self.create_bare_message(message)?).await
    }

    async fn handle_message(&self, message: Message) -> Result<(), JanusError> {
        match message {
            Message::Text(payload) => {
                println!("Janus -> {}", payload);
                let message: JanusBareMessage = serde_json::from_str(&payload)?;
                let action = message.data.get("janus").and_then(Value::as_str);

                if action == Some("ack") {
                    return Ok(());
                }

                if action == Some("timeout") {
                    if let Some(session_id) = message.data.get("session_id").and_then(Value::as_u64)
                    {
                        self.sessions.write().await.remove(&session_id);
                    }
                    return Ok(());
                }

                if let Some(transaction_id) = &message.transaction {
                    if let Some(sender) = self.transactions.write().await.remove(transaction_id) {
                        let _ = sender.send(message);
                    }
                }
            }
            _ => {
                // wtf bro
            }
        }

        Ok(())
    }
}
