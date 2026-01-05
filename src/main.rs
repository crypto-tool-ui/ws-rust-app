use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    time::{timeout, Duration},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::env;

const MAX_PAYLOAD: usize = 512 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ws_port = env::var("WS_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse::<u16>()
        .unwrap_or(8080);
    let backend_host = env::var("BACKEND_HOST")
        .unwrap_or_else(|_| "127.0.0.1".to_string());
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

    // Shared close signal
    let (close_tx, mut close_rx) = tokio::sync::broadcast::channel::<()>(2);

    // WS → TCP
    let ws_to_tcp = {
        let close_tx = close_tx.clone();
        async move {
            loop {
                let msg = match timeout(CONNECTION_TIMEOUT, ws_read.next()).await {
                    Ok(Some(Ok(m))) => m,
                    _ => break,
                };

                match msg {
                    Message::Text(text) => {
                        let data = if text.ends_with('\n') {
                            text.into_bytes()
                        } else {
                            format!("{}\n", text).into_bytes()
                        };
                        if tcp_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Message::Binary(data) => {
                        if tcp_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(data) => {
                        // Respond to ping to keep connection alive
                        continue;
                    }
                    Message::Pong(_) => {}
                    _ => {}
                }
            }
            
            // Flush và shutdown TCP write
            let _ = tcp_write.flush().await;
            let _ = tcp_write.shutdown().await;
            let _ = close_tx.send(());
        }
    };

    // TCP → WS
    let tcp_to_ws = {
        let close_tx = close_tx.clone();
        async move {
            let mut buffer = vec![0u8; MAX_PAYLOAD];
            loop {
                let n = match timeout(CONNECTION_TIMEOUT, tcp_read.read(&mut buffer)).await {
                    Ok(Ok(0)) => break, // TCP EOF
                    Ok(Ok(n)) => n,
                    _ => break,
                };

                let text = String::from_utf8_lossy(&buffer[..n]).to_string();
                if ws_write.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            
            // Flush WS buffer
            let _ = ws_write.flush().await;
            let _ = close_tx.send(());
        }
    };

    // Wait for either direction to close
    select! {
        _ = ws_to_tcp => {},
        _ = tcp_to_ws => {},
    }

    // Graceful close sequence
    let _ = timeout(CLOSE_TIMEOUT, async {
        // Wait a bit for any pending data
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // Send WebSocket close frame
        let _ = ws_write.send(Message::Close(None)).await;
        let _ = ws_write.flush().await;
        
        // Wait for close acknowledgment or timeout
        while let Ok(Some(Ok(msg))) = timeout(Duration::from_secs(1), ws_read.next()).await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    }).await;

    println!("[{}][WS] ✗ {}", instance_id, client_addr.ip());
    Ok(())
}
