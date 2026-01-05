use std::time::Instant;
use std::sync::atomic::{AtomicU64, Ordering};

static CONN_ID: AtomicU64 = AtomicU64::new(1);

async fn handle_connection(
    stream: TcpStream,
    client: SocketAddr,
    backend: String,
    instance: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let cid = CONN_ID.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();

    stream.set_nodelay(true)?;

    let ws = accept_async(stream).await?;
    let tcp = TcpStream::connect(&backend).await?;

    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();

    println!(
        "[{}] #{} {} → {}",
        instance,
        cid,
        client.ip(),
        backend
    );

    let (tx, rx) = tokio::sync::oneshot::channel::<CloseReason>();

    // WS → TCP
    let tx1 = tx.clone();
    tokio::spawn(async move {
        loop {
            match ws_rx.next().await {
                Some(Ok(Message::Text(t))) => {
                    let d = if t.ends_with('\n') { t } else { format!("{}\n", t) };
                    if tcp_tx.write_all(d.as_bytes()).await.is_err() {
                        let _ = tx1.send(CloseReason::Error);
                        return;
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    let _ = tcp_tx.shutdown().await;
                    let _ = tx1.send(CloseReason::WsClose);
                    return;
                }
                _ => {}
            }
        }
    });

    // TCP → WS
    tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_PAYLOAD];
        loop {
            match tcp_rx.read(&mut buf).await {
                Ok(0) => {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    let _ = tx.send(CloseReason::TcpClose);
                    return;
                }
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]).to_string();
                    if ws_tx.send(Message::Text(text)).await.is_err() {
                        let _ = tx.send(CloseReason::Error);
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(CloseReason::Error);
                    return;
                }
            }
        }
    });

    let reason = rx.await.unwrap_or(CloseReason::Error);
    let dur = start.elapsed().as_secs();

    println!(
        "[{}] #{} {}s | {:?}",
        instance,
        cid,
        dur,
        reason
    );

    Ok(())
}
