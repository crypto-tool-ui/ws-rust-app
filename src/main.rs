use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{timeout, Duration},
};
use tokio_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    Message,
};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::env;
use base64::{Engine as _, engine::general_purpose};

const MAX_PAYLOAD: usize = 512 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

enum CloseReason {
    WsToTcp,
    TcpToWs,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws_port = env::var("WS_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .unwrap_or(8080);
    let instance_id = env::var("INSTANCE_ID").unwrap_or_else(|_| "unknown".to_string());

    let addr = format!("0.0.0.0:{}", ws_port);
    let listener = TcpListener::bind(&addr).await?;
    
    println!("[PROXY] Instance ID: {}", instance_id);
    println!("[PROXY] WebSocket listening on :{}", ws_port);
    println!("[PROXY] URL format: ws://ip:port/BASE64_ENCODED_ADDRESS");
    println!("[PROXY] Ready\n");

    loop {
        let (stream, client_addr) = listener.accept().await?;
        let instance = instance_id.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, client_addr, instance).await {
                let _ = e;
            }
        });
    }
}

fn extract_backend_from_path(path: &str) -> Option<String> {
    // Path format: /BASE64_ENCODED_ADDRESS
    // Example: /MTI3LjAuMC4xOjMzMzM= decodes to "127.0.0.1:3333"
    
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    
    // Remove query string if present
    let path = path.split('?').next().unwrap_or(path);
    
    match general_purpose::STANDARD.decode(path) {
        Ok(decoded) => {
            match String::from_utf8(decoded) {
                Ok(addr) => {
                    // Validate format: should contain host:port
                    if addr.contains(':') {
                        Some(addr)
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

async fn handle_connection(
    stream: TcpStream,
    client_addr: SocketAddr,
    instance_id: String,
) -> Result<(), ()> {
    let _ = stream.set_nodelay(true);

    // Capture backend address from WebSocket handshake
    let backend_addr = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let backend_addr_clone = backend_addr.clone();

    let ws_stream = match timeout(Duration::from_secs(10), async {
        tokio_tungstenite::accept_hdr_async(stream, |req: &Request, res: Response| {
            let path = req.uri().path();
            
            if let Some(addr) = extract_backend_from_path(path) {
                *backend_addr_clone.lock().unwrap() = Some(addr.clone());
                println!("[{}][WS] Path: {} → Backend: {}", instance_id, path, addr);
                Ok(res)
            } else {
                println!("[{}][WS] Invalid path: {}", instance_id, path);
                Err(tokio_tungstenite::tungstenite::Error::Protocol(
                    std::borrow::Cow::from("Invalid backend address in path")
                ))
            }
        })
        .await
    })
    .await
    {
        Ok(Ok(ws)) => ws,
        _ => {
            println!("[{}][WS] ✗ {} (handshake failed)", instance_id, client_addr.ip());
            return Err(());
        }
    };

    let backend_addr = match backend_addr.lock().unwrap().clone() {
        Some(addr) => addr,
        None => {
            println!("[{}][WS] ✗ {} (no backend address)", instance_id, client_addr.ip());
            return Err(());
        }
    };

    println!("[{}][WS] ✓ {} → {}", instance_id, client_addr.ip(), backend_addr);

    let tcp_stream = match TcpStream::connect(&backend_addr).await {
        Ok(s) => {
            println!("[{}][TCP] ✓ Connected to {}", instance_id, backend_addr);
            s
        }
        Err(e) => {
            println!("[{}][TCP] ✗ Failed to connect to {}: {}", instance_id, backend_addr, e);
            return Err(());
        }
    };
    let _ = tcp_stream.set_nodelay(true);

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    // Proxy bidirectional with manual loop control
    let mut buffer = vec![0u8; MAX_PAYLOAD];
    let close_reason = loop {
        tokio::select! {
            // WS → TCP
            msg_result = timeout(CONNECTION_TIMEOUT, ws_read.next()) => {
                match msg_result {
                    Ok(Some(Ok(msg))) => {
                        let should_close = match msg {
                            Message::Text(text) => {
                                tcp_write.write_all(text.as_bytes()).await.is_err()
                            }
                            Message::Binary(data) => {
                                tcp_write.write_all(&data).await.is_err()
                            }
                            Message::Close(_) => true,
                            Message::Ping(_) | Message::Pong(_) => false,
                            _ => false,
                        };
                        
                        if should_close {
                            break CloseReason::WsToTcp;
                        }
                    }
                    _ => break CloseReason::WsToTcp,
                }
            }
            
            // TCP → WS
            read_result = timeout(CONNECTION_TIMEOUT, tcp_read.read(&mut buffer)) => {
                match read_result {
                    Ok(Ok(0)) => break CloseReason::TcpToWs, // EOF
                    Ok(Ok(n)) => {
                        let text = String::from_utf8_lossy(&buffer[..n]).to_string();
                        if ws_write.send(Message::Text(text)).await.is_err() {
                            break CloseReason::TcpToWs;
                        }
                    }
                    _ => break CloseReason::TcpToWs,
                }
            }
        }
    };

    // Flush pending data based on which side closed
    match close_reason {
        CloseReason::WsToTcp => {
            let _ = tcp_write.flush().await;
            let _ = tcp_write.shutdown().await;
        }
        CloseReason::TcpToWs => {
            let _ = ws_write.flush().await;
        }
    }

    // Graceful WebSocket close
    let _ = timeout(CLOSE_TIMEOUT, async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // Send close frame
        let _ = ws_write.send(Message::Close(None)).await;
        let _ = ws_write.flush().await;
        
        // Wait for close ack
        while let Ok(Some(Ok(msg))) = timeout(Duration::from_secs(1), ws_read.next()).await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    }).await;

    println!("[{}][WS] ✗ {} → {}", instance_id, client_addr.ip(), backend_addr);
    Ok(())
}
