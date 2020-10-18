use crate::consts::{FTL_INGEST_RESP_OK, FTL_INGEST_RESP_PING, INGEST_PORT};
use hex::{encode, FromHex};
use rand::random;
use ring::hmac::{verify, Key, HMAC_SHA512};
use staaf_core::rpc::{
    AudioCodec, MediaStream, MediaStreamAudio, MediaStreamContent, MediaStreamVideo,
    RegisterStream, StaafRPC, VideoCodec,
};
use staaf_core::StreamId;
use staaf_janus_client::{JanusConnection, JanusMessageData, JanusPluginOwned, JanusPluginOwner};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::stream::StreamExt;
use tokio::task::spawn;
use tokio::time::{delay_for, Duration};

pub async fn ingest_server() {
    let mut janus_connection = JanusConnection::parse("ws://localhost:8188").unwrap();
    janus_connection.start().await;
    let session = janus_connection
        .create_session()
        .await
        .expect("Failed to create Janus Session");
    let session = Arc::new(session);

    let plugin = session
        .attach_owned("eater.staafmixer")
        .await
        .expect("Failed to attach staafmixer plugin");

    let mut listener =
        TcpListener::bind(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), INGEST_PORT))
            .await
            .unwrap();

    while let Some(connection) = listener.next().await {
        match connection {
            Ok(stream) => {
                spawn(ingest_connection(stream, plugin.clone()));
            }
            Err(err) => println!("Listening failed: {}", err),
        }
    }
}

#[derive(Debug)]
struct ControlConnection {
    addr: SocketAddr,
    tcp_stream: TcpStream,
    shared_secret: Vec<u8>,
    channel_id: Option<u64>,
    authenticated: bool,
    metadata: HashMap<String, String>,
}

impl ControlConnection {
    pub async fn send_port(&mut self, port: u16) {
        self.tcp_stream
            .write(format!("{}. Use UDP port {}\n", FTL_INGEST_RESP_OK, port).as_bytes())
            .await;
    }

    pub fn new(tcp_stream: TcpStream) -> ControlConnection {
        ControlConnection {
            addr: tcp_stream.peer_addr().unwrap(),
            tcp_stream,
            shared_secret: random::<[u8; 8]>().to_vec(),
            channel_id: None,
            authenticated: false,
            metadata: HashMap::new(),
        }
    }

    async fn handshake(&mut self) -> Result<(), ()> {
        let mut hmac_buffer = [0u8; 8];
        self.tcp_stream
            .read_exact(&mut hmac_buffer)
            .await
            .map_err(|_| ())?;
        if &hmac_buffer != b"HMAC\r\n\r\n" {
            return Err(());
        }

        self.tcp_stream
            .write(format!("{} {}\n", FTL_INGEST_RESP_OK, encode(&self.shared_secret)).as_bytes())
            .await
            .map_err(|_| ())?;
        let mut buffer = [0u8; 1024];
        let mut offset = 0;
        while !buffer[..offset].ends_with(b"\r\n\r\n") {
            let len = self
                .tcp_stream
                .read(&mut buffer[offset..])
                .await
                .map_err(|_| ())?;
            if len == 0 {
                return Err(());
            }

            offset += len;

            if offset == 1024 {
                return Err(());
            }
        }

        let line = String::from_utf8(buffer[0..offset].to_vec()).map_err(|_| ())?;
        let mut splitter = line.split(" ");
        splitter.next().filter(|cmd| *cmd == "CONNECT").ok_or(())?;
        let channel_id = splitter
            .next()
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or(())?;
        let verification = splitter
            .next()
            .filter(|hash| hash.starts_with("$") && hash.ends_with("\r\n\r\n"))
            .map(|v| &v[1..v.len() - 4])
            .and_then(|hex| Vec::from_hex(hex).map_err(|err| println!("{}", err)).ok())
            .ok_or(())?;

        println!("Connection from {} with channel {}", self.addr, channel_id);

        // TODO: Fetch key by channel id
        let key = b"staaf";
        let hmac_key = Key::new(HMAC_SHA512, key);

        verify(&hmac_key, &self.shared_secret, &verification).map_err(|_| ())?;
        self.channel_id = Some(channel_id);
        self.authenticated = true;

        println!(
            "Authenticated stream {} for channel {}",
            self.addr, channel_id
        );

        self.tcp_stream
            .write(format!("{}\n", FTL_INGEST_RESP_OK).as_bytes())
            .await
            .map_err(|_| ())?;
        Ok(())
    }

    async fn read_stream_info(&mut self) -> Result<(), ()> {
        let mut recv_buffer = [0u8; 1024];
        let mut buffer = vec![];
        loop {
            let len = self
                .tcp_stream
                .read(&mut recv_buffer)
                .await
                .map_err(|_| ())?;
            if len == 0 {
                return Err(());
            }

            buffer.extend_from_slice(&recv_buffer[..len]);

            if buffer.ends_with(b".\r\n\r\n") {
                break;
            }
        }

        let headers = String::from_utf8(buffer).map_err(|_| ())?;
        println!("{:?}", headers);
        for line in headers.split("\r\n\r\n") {
            if line == "." {
                break;
            }

            let mut splitter = line.splitn(2, ": ");
            let name = splitter.next().ok_or(())?;
            let value = splitter.next().ok_or(())?;

            self.metadata.insert(name.to_string(), value.to_string());
        }

        Ok(())
    }

    async fn send_ping(&mut self) -> tokio::io::Result<usize> {
        self.tcp_stream
            .write(format!("{}", FTL_INGEST_RESP_PING).as_bytes())
            .await
    }
}

async fn ingest_connection(mut stream: TcpStream, plugin: JanusPluginOwned) -> Result<(), ()> {
    let peer_addr = stream.peer_addr().map_err(|_| ())?.ip();
    let mut connection = ControlConnection::new(stream);
    connection.handshake().await?;
    connection.read_stream_info().await?;

    println!("Received metadata: {:?}", connection.metadata);

    let mut rpc = RegisterStream {
        media_streams: vec![],
        peer_addr,
        stream_id: connection.channel_id.ok_or(())? as StreamId,
    };

    if connection.metadata.get("Video").map(|x| x.as_str()) == Some("true") {
        connection.metadata.get("VideoCodec");
        let media_stream = MediaStream {
            ssrc: connection
                .metadata
                .get("VideoIngestSSRC")
                .ok_or(())?
                .parse()
                .map_err(|_| ())?,
            payload_type: connection
                .metadata
                .get("VideoPayloadType")
                .ok_or(())?
                .parse::<u16>()
                .map_err(|_| ())?,
            content: MediaStreamContent::Video(MediaStreamVideo {
                codec: VideoCodec::H264,
                width: 0,
                height: 0,
            }),
        };

        rpc.media_streams.push(media_stream);
    }

    if connection.metadata.get("Audio").map(|x| x.as_str()) == Some("true") {
        connection.metadata.get("AudioCodec");
        let media_stream = MediaStream {
            ssrc: connection
                .metadata
                .get("AudioIngestSSRC")
                .ok_or(())?
                .parse()
                .map_err(|_| ())?,
            payload_type: connection
                .metadata
                .get("AudioPayloadType")
                .ok_or(())?
                .parse()
                .map_err(|_| ())?,
            content: MediaStreamContent::Audio(MediaStreamAudio {
                codec: AudioCodec::Opus,
            }),
        };

        rpc.media_streams.push(media_stream);
    }

    println!("Sending StaafRPC message to Janus: {:?}", rpc);
    let resp = plugin
        .send(
            "message",
            JanusMessageData::Body {
                body: StaafRPC::RegisterStream(rpc),
            },
        )
        .await
        .expect("Failed to communicate with Janus");

    let data = resp
        .extract::<StaafRPC>()
        .expect("Failed to read message from Janus");

    // Verify metadata and request port @ janus
    if let StaafRPC::StreamRegistered { port } = data {
        connection.send_port(port).await;
    } else {
        panic!("Got wrong response")
    }

    spawn(async move {
        loop {
            let mut x = vec![0u8; 4096];
            let s = connection.tcp_stream.read(&mut x).await.unwrap();
            let x = String::from_utf8_lossy(&x[..s]);
            if "PING" == x.split(" ").next().unwrap() {
                connection
                    .tcp_stream
                    .write(format!("{}\r\n\r\n", FTL_INGEST_RESP_PING).as_bytes())
                    .await;
            }
        }
    });

    Ok(())
}
