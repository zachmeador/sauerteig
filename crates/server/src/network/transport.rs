//! Observe real TCP writes under WebSocket framing; a slow progressing frame
//! must not be confused with a stalled socket.
use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
};
#[derive(Clone)]
pub(super) struct Progress(Arc<Writes>);
struct Writes {
    start: Instant,
    micros: AtomicU64,
}
impl Progress {
    pub(super) fn new() -> Self {
        Self(Arc::new(Writes {
            start: Instant::now(),
            micros: AtomicU64::new(0),
        }))
    }
    pub(super) fn wrote(&self) {
        self.0.micros.store(
            self.0.start.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Release,
        );
    }
    pub fn last_write(&self) -> Instant {
        self.0.start + Duration::from_micros(self.0.micros.load(Ordering::Acquire))
    }
}
pub(super) struct ProgressIo {
    socket: TcpStream,
    progress: Progress,
}
impl AsyncRead for ProgressIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}
impl AsyncWrite for ProgressIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.socket).poll_write(cx, buf);
        if matches!(result,Poll::Ready(Ok(n)) if n>0) {
            self.progress.wrote();
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
pub(super) struct ProgressListener(pub TcpListener);
impl Listener for ProgressListener {
    type Io = ProgressIo;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((socket, address)) => {
                    if let Err(error) = socket.set_nodelay(true) {
                        eprintln!("Could not enable TCP_NODELAY: {error}");
                    }
                    return (
                        ProgressIo {
                            socket,
                            progress: Progress::new(),
                        },
                        address,
                    );
                }
                Err(error) => {
                    eprintln!("Accept failed: {error}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}
impl Connected<IncomingStream<'_, ProgressListener>> for Progress {
    fn connect_info(stream: IncomingStream<'_, ProgressListener>) -> Self {
        stream.io().progress.clone()
    }
}
