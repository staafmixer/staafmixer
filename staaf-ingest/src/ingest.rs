use crate::consts::INGEST_PORT;
use hex::{encode_to_slice, FromHex};
use rand::random;
use ring::hmac::{verify, Key, HMAC_SHA512};
use std::net::{Ipv6Addr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::stream::StreamExt;
use tokio::task::spawn;

pub async fn ingest_server() {
    let mut listener =
        TcpListener::bind(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), INGEST_PORT))
            .await
            .unwrap();

    while let Some(connection) = listener.next().await {
        match connection {
            Ok(stream) => {
                spawn(ingest_connection(stream));
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
}

impl ControlConnection {
    pub fn new(tcp_stream: TcpStream) -> ControlConnection {
        ControlConnection {
            addr: tcp_stream.peer_addr().unwrap(),
            tcp_stream,
            shared_secret: random::<[u8; 8]>().to_vec(),
            channel_id: None,
            authenticated: false,
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

        let mut resp: [u8; 21] = *b"200 \0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\n";
        encode_to_slice(&self.shared_secret, &mut resp[4..20]).map_err(|_| ())?;
        self.tcp_stream.write(&resp).await.map_err(|_| ())?;
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

        // Fetch key by channel id
        let key = b"staaf";
        let hmac_key = Key::new(HMAC_SHA512, key);

        verify(&hmac_key, &self.shared_secret, &verification).map_err(|_| ())?;
        self.channel_id = Some(channel_id);
        self.authenticated = true;

        println!(
            "Authenticated stream {} for channel {}",
            self.addr, channel_id
        );

        Ok(())
    }
}

async fn ingest_connection(stream: TcpStream) -> Result<(), ()> {
    let mut connection = ControlConnection::new(stream);
    connection.handshake().await?;

    Ok(())
}