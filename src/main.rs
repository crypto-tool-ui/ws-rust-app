use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{timeout, Duration},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::env;

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
    let backend_host = env::var("BACKEND_HOST")
        .unwrap_or_else(|_| "backend".to_string());
    let backend_port = env::var("BACKEND_PORT")
        .unwrap_or_else(|_| "3333".to_string())
        .parse::<u16>()
        .unwrap_or(3333);
    let instance_id = env::var("INSTANCE_ID").unwrap_or_else(|_| "unknown".to_string());

    let addr = format!("0.0.0.0:{}", ws_port);
    let listener = TcpListener::bind(&addr).await?;
    
    println!("[PROXY] Instance ID: {}", instance_id);
    println!("[PROXY] WebSocket listening on :{}", ws_port);
    println!("[PROXY] Backend TCP: {}:{}", backend_host, backend_port);
    println!("[PROXY] Ready\n");

    loop {
        let (stream, client_addr) = listener.accept().await?;
        let backend = format!("{}:{}", backend_host, backend_port);
        let instance = instance_id.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, client_addr, backend, instance).await {
                let _ = e;
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    client_addr: SocketAddr,
    backend_addr: String,
    instance_id: String,
) -> Result<(), ()> {
    let _ = stream.set_nodelay(true);

    let ws_stream = match timeout(Duration::from_secs(10), accept_async(stream)).await {
        Ok(Ok(ws)) => ws,
        _ => return Err(()),
    };
    println!("[{}][WS] ✓ {}", instance_id, client_addr.ip());

    let tcp_stream = match TcpStream::connect(&backend_addr).await {
        Ok(s) => s,
        Err(_) => return Err(()),
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
                                let data = if text.ends_with('\n') {
                                    text.into_bytes()
                                } else {
                                    format!("{}\n", text).into_bytes()
                                };
                                tcp_write.write_all(&data).await.is_err()
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

    println!("[{}][WS] ✗ {}", instance_id, client_addr.ip());
    Ok(())
}
