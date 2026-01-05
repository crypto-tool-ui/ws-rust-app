use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{timeout, Duration},
};
use tokio_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    Message, Error as WsError,
    http::StatusCode,
};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::env;
use std::sync::{Arc, Mutex};
use base64::{Engine as _, engine::general_purpose};

const MAX_PAYLOAD: usize = 512 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
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
    println!("[PROXY] Example: ws://localhost:8080/{}", 
             general_purpose::STANDARD.encode("127.0.0.1:3333"));
    println!("[PROXY] Ready\n");

    loop {
        let (stream, client_addr) = listener.accept().await?;
        let instance = instance_id.clone();
        
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, client_addr, instance).await {
                eprintln!("[ERROR] Connection failed: {:?}", e);
            }
        });
    }
}

fn extract_backend_from_path(path: &str) -> Result<String, String> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Err("Empty path".to_string());
    }
    
    let path = path.split('?').next().unwrap_or(path);
    
    if !path.chars().all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '=') {
        return Err("Invalid base64 characters".to_string());
    }
    
    match general_purpose::STANDARD.decode(path) {
        Ok(decoded) => {
            match String::from_utf8(decoded) {
                Ok(addr) => {
                    if let Some(colon_pos) = addr.rfind(':') {
                        let host = &addr[..colon_pos];
                        let port_str = &addr[colon_pos + 1..];
                        
                        if let Ok(port) = port_str.parse::<u16>() {
                            if port > 0 && !host.is_empty() {
                                Ok(addr)
                            } else {
                                Err("Invalid host or port".to_string())
                            }
                        } else {
                            Err("Port is not a valid number".to_string())
                        }
                    } else {
                        Err("Missing port separator ':'".to_string())
                    }
                }
                Err(e) => Err(format!("UTF-8 decode error: {}", e)),
            }
        }
        Err(e) => Err(format!("Base64 decode error: {}", e)),
    }
}

// FIXED: Response không có generic
fn handshake_callback(
    req: &Request, 
    backend_addr: &Arc<Mutex<Option<String>>>,
    instance_id: &str
) -> Result<Response, Response> {
    let path = req.uri().path();
    let client_ip = req.headers()
        .get("x-forwarded-for")
        .or_else(|| req.headers().get("x-real-ip"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown");

    println!("[{}][HANDSHAKE] Client: {}, Path: {}", instance_id, client_ip, path);
    
    println!("[{}][HANDSHAKE] Headers:", instance_id);
    for (name, value) in req.headers() {
        if let Ok(val_str) = value.to_str() {
            println!("  {}: {}", name, val_str);
        }
    }

    match extract_backend_from_path(path) {
        Ok(addr) => {
            *backend_addr.lock().unwrap() = Some(addr.clone());
            println!("[{}][HANDSHAKE] ✓ Backend: {}", instance_id, addr);
            
            Response::builder()
                .status(StatusCode::SWITCHING_PROTOCOLS)
                .header("Server", "ws-tcp-proxy")
                .body(())
                .map_err(|e| {
                    println!("[{}][HANDSHAKE] ✗ Response build error: {}", instance_id, e);
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(())
                        .unwrap()
                })
        }
        Err(error) => {
            println!("[{}][HANDSHAKE] ✗ Path validation failed: {}", instance_id, error);
            Err(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "text/plain")
                .body(())
                .unwrap())
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    client_addr: SocketAddr,
    instance_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(e) = stream.set_nodelay(true) {
        println!("[{}][SOCKET] Warning: Failed to set nodelay: {}", instance_id, e);
    }

    let backend_addr = Arc::new(Mutex::new(None::<String>));
    let backend_addr_clone = backend_addr.clone();
    let instance_clone = instance_id.clone();

    println!("[{}][CONNECTION] New connection from {}", instance_id, client_addr.ip());

    let ws_stream = match timeout(HANDSHAKE_TIMEOUT, async {
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, _res: Response| {
            handshake_callback(req, &backend_addr_clone, &instance_clone)
        }).await
    }).await {
        Ok(Ok(ws)) => {
            println!("[{}][WS] ✓ Handshake successful with {}", instance_id, client_addr.ip());
            ws
        },
        Ok(Err(WsError::Http(response))) => {
            println!("[{}][WS] ✗ HTTP handshake failed with {}: {:?}", instance_id, client_addr.ip(), response);
            return Err("HTTP handshake failed".into());
        },
        Ok(Err(e)) => {
            println!("[{}][WS] ✗ Handshake error with {}: {}", instance_id, client_addr.ip(), e);
            return Err(format!("Handshake error: {}", e).into());
        },
        Err(_) => {
            println!("[{}][WS] ✗ Handshake timeout with {}", instance_id, client_addr.ip());
            return Err("Handshake timeout".into());
        }
    };

    let backend_addr = match backend_addr.lock().unwrap().clone() {
        Some(addr) => addr,
        None => {
            println!("[{}][WS] ✗ No backend address extracted", instance_id);
            return Err("No backend address".into());
        }
    };

    println!("[{}][PROXY] Starting proxy: {} → {}", instance_id, client_addr.ip(), backend_addr);

    let tcp_stream = match timeout(Duration::from_secs(10), TcpStream::connect(&backend_addr)).await {
        Ok(Ok(s)) => {
            println!("[{}][TCP] ✓ Connected to {}", instance_id, backend_addr);
            s
        }
        Ok(Err(e)) => {
            println!("[{}][TCP] ✗ Failed to connect to {}: {}", instance_id, backend_addr, e);
            return Err(format!("TCP connection failed: {}", e).into());
        }
        Err(_) => {
            println!("[{}][TCP] ✗ Connection timeout to {}", instance_id, backend_addr);
            return Err("TCP connection timeout".into());
        }
    };
    
    if let Err(e) = tcp_stream.set_nodelay(true) {
        println!("[{}][TCP] Warning: Failed to set nodelay: {}", instance_id, e);
    }

    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();

    let mut buffer = vec![0u8; MAX_PAYLOAD];
    let close_reason = loop {
        tokio::select! {
            msg_result = timeout(CONNECTION_TIMEOUT, ws_read.next()) => {
                match msg_result {
                    Ok(Some(Ok(msg))) => {
                        let should_close = match msg {
                            Message::Text(text) => tcp_write.write_all(text.as_bytes()).await.is_err(),
                            Message::Binary(data) => tcp_write.write_all(&data).await.is_err(),
                            Message::Close(_) => true,
                            Message::Ping(data) => { let _ = ws_write.send(Message::Pong(data)).await; false }
                            Message::Pong(_) => false,
                            _ => false,
                        };
                        if should_close { break CloseReason::WsToTcp; }
                    }
                    _ => break CloseReason::WsToTcp,
                }
            }
            read_result = timeout(CONNECTION_TIMEOUT, tcp_read.read(&mut buffer)) => {
                match read_result {
                    Ok(Ok(0)) => break CloseReason::TcpToWs,
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

    match close_reason {
        CloseReason::WsToTcp => { let _ = tcp_write.flush().await; let _ = tcp_write.shutdown().await; }
        CloseReason::TcpToWs => { let _ = ws_write.flush().await; }
    }

    let _ = timeout(CLOSE_TIMEOUT, async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = ws_write.send(Message::Close(None)).await;
        let _ = ws_write.flush().await;
        while let Ok(Some(Ok(msg))) = timeout(Duration::from_secs(1), ws_read.next()).await {
            if matches!(msg, Message::Close(_)) { break; }
        }
    }).await;

    println!("[{}][PROXY] ✗ Connection closed: {} → {}", instance_id, client_addr.ip(), backend_addr);
    Ok(())
}
