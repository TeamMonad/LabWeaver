//! A test-only `PostgreSQL` wire relay. It delays one exact simple-query response.
//!
//! The relay forwards the real query to `PostgreSQL`, waits for `ReadyForQuery`, then
//! holds that response until the test confirms cancellation. It logs no packets.

use std::{io, sync::Arc};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc, oneshot},
};
use tokio_util::task::AbortOnDropHandle;

type Signal = (oneshot::Sender<u8>, oneshot::Receiver<()>);
type Armed = Arc<Mutex<Option<(String, Signal)>>>;

/// Relays one fixture pool and gates an explicitly armed simple-query response.
pub struct PgResponseGate {
    /// Loopback port used only by the writer pool.
    pub port: u16,
    armed: Armed,
    _task: AbortOnDropHandle<io::Result<()>>,
}

impl PgResponseGate {
    /// Starts a relay to the isolated container, without logging packets.
    pub async fn start(upstream_port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let armed = Arc::new(Mutex::new(None));
        let state = Arc::clone(&armed);
        let task = tokio::spawn(async move {
            loop {
                let (client, _) = listener.accept().await?;
                let server = TcpStream::connect(("127.0.0.1", upstream_port)).await?;
                client.set_nodelay(true)?;
                server.set_nodelay(true)?;
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let _ = relay(client, server, state).await;
                });
            }
        });
        Ok(Self {
            port,
            armed,
            _task: AbortOnDropHandle::new(task),
        })
    }

    /// Holds this response after the real server finishes, until the test releases it.
    pub async fn arm(&self, query: &str) -> (oneshot::Receiver<u8>, oneshot::Sender<()>) {
        let (observed_tx, observed_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let previous = self
            .armed
            .lock()
            .await
            .replace((query.to_owned(), (observed_tx, release_rx)));
        assert!(previous.is_none(), "a response gate is already armed");
        (observed_rx, release_tx)
    }
}

async fn packet(reader: &mut (impl tokio::io::AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let tag = reader.read_u8().await?;
    let length = reader.read_u32().await?;
    if !(4..=1_048_576).contains(&length) {
        return Err(io::Error::other("unexpected PostgreSQL packet length"));
    }
    let mut bytes = vec![0; length as usize + 1];
    bytes[0] = tag;
    bytes[1..5].copy_from_slice(&length.to_be_bytes());
    reader.read_exact(&mut bytes[5..]).await?;
    Ok(bytes)
}

async fn relay(mut client: TcpStream, mut server: TcpStream, armed: Armed) -> io::Result<()> {
    // The fixture explicitly disables database TLS. The production HTTP server uses TLS.
    let length = client.read_u32().await?;
    if !(8..=65_536).contains(&length) {
        return Err(io::Error::other("unexpected PostgreSQL startup length"));
    }
    let mut startup = vec![0; length as usize];
    startup[..4].copy_from_slice(&length.to_be_bytes());
    client.read_exact(&mut startup[4..]).await?;
    server.write_all(&startup).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let forward = async {
        loop {
            let bytes = packet(&mut client_read).await?;
            if bytes[0] == b'Q' {
                let mut state = armed.lock().await;
                if state
                    .as_ref()
                    .is_some_and(|(query, _)| bytes[5..] == [query.as_bytes(), &[0]].concat())
                    && let Some((_, signal)) = state.take()
                {
                    signal_tx
                        .send(signal)
                        .map_err(|_| io::Error::other("response relay stopped"))?;
                }
            }
            server_write.write_all(&bytes).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), io::Error>(())
    };
    let backward = async {
        let mut held = Vec::new();
        let mut signal: Option<Signal> = None;
        loop {
            let bytes = packet(&mut server_read).await?;
            if signal.is_none() {
                signal = signal_rx.try_recv().ok();
            }
            if signal.is_some() {
                held.extend_from_slice(&bytes);
                if held.len() > 1_048_576 {
                    return Err(io::Error::other("gated response exceeds fixture bound"));
                }
                if bytes[0] == b'Z' {
                    if let Some((observed, release)) = signal.take() {
                        let _ = observed.send(bytes[5]);
                        let _ = release.await;
                    }
                    client_write.write_all(&held).await?;
                    held.clear();
                }
            } else {
                client_write.write_all(&bytes).await?;
            }
        }
    };
    tokio::select! {
        result = forward => result,
        result = backward => result,
    }
}
