use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;

const WS_PORT: u16 = 8080;
const TCP_BACKEND_HOST: &str = "127.0.0.1"; // hoặc tên container trong Docker network
const TCP_BACKEND_PORT: u16 = 3333;
const MAX_PAYLOAD: usize = 100 * 1024; // 100KB

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("0.0.0.0:{}", WS_PORT);
    let listener = TcpListener::bind(&addr).await?;
    
    println!("[PROXY] WebSocket listening on port: {}", WS_PORT);
    println!("[PROXY] Backend TCP: {}:{}", TCP_BACKEND_HOST, TCP_BACKEND_PORT);
    println!("[PROXY] Ready to accept connections...\n");

    loop {
        let (stream, client_addr) = listener.accept().await?;
        tokio::spawn(handle_connection(stream, client_addr));
    }
}

async fn handle_connection(stream: TcpStream, client_addr: SocketAddr) {
    println!("[WS] New connection from {}", client_addr.ip());

    // Accept WebSocket connection
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[ERROR] WebSocket handshake failed from {}: {}", client_addr.ip(), e);
            return;
        }
    };

    println!("[WS] WebSocket established from {}", client_addr.ip());

    // Connect to backend TCP
    let backend_addr = format!("{}:{}", TCP_BACKEND_HOST, TCP_BACKEND_PORT);
    let tcp_stream = match TcpStream::connect(&backend_addr).await {
        Ok(stream) => {
            println!(
                "[TCP] Connected from {} -> {}",
                client_addr.ip(),
                backend_addr
            );
            stream
        }
        Err(e) => {
            eprintln!("[TCP ERROR] Failed to connect to {}: {}", backend_addr, e);
            return;
        }
    };

    if let Err(e) = tcp_stream.set_nodelay(true) {
        eprintln!("[WARN] Failed to set TCP_NODELAY: {}", e);
    }

    // Split streams
    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // WS → TCP
    let ws_to_tcp = async {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let data = if text.ends_with('\n') {
                        text
                    } else {
                        format!("{}\n", text)
                    };
                    if let Err(e) = tcp_write.write_all(data.as_bytes()).await {
                        eprintln!("[ERROR] WS→TCP failed: {}", e);
                        break;
                    }
                }
                Ok(Message::Binary(data)) => {
                    if let Err(e) = tcp_write.write_all(&data).await {
                        eprintln!("[ERROR] WS→TCP failed: {}", e);
                        break;
                    }
                }
                Ok(Message::Close(_)) => {
                    println!("[WS] Connection closed from {}", client_addr.ip());
                    break;
                }
                Err(e) => {
                    eprintln!("[WS ERROR] {}", e);
                    break;
                }
                _ => {}
            }
        }
    };

    // TCP → WS
    let tcp_to_ws = async {
        let mut buffer = vec![0u8; MAX_PAYLOAD];
        loop {
            match tcp_read.read(&mut buffer).await {
                Ok(0) => {
                    println!("[TCP] Backend closed connection");
                    break;
                }
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buffer[..n]).to_string();
                    if let Err(e) = ws_write.send(Message::Text(text)).await {
                        eprintln!("[ERROR] TCP→WS: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[TCP ERROR] {}", e);
                    break;
                }
            }
        }
    };

    // Run both directions concurrently
    select! {
        _ = ws_to_tcp => {},
        _ = tcp_to_ws => {},
    }

    println!("[PROXY] Connection closed for {}", client_addr.ip());
}
