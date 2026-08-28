//! HTTP transport guards that cannot be expressed as request middleware.
//!
//! A response-body timeout is not sufficient for a slow reader: Hyper may be
//! blocked flushing an already-polled frame and never poll the body again.
//! Wrapping the accepted socket makes lack of actual write progress a
//! connection error, which drops the Hyper response body and its admission
//! permit.

use axum::body::Body;
use axum::serve::Listener;
use axum::Router;
use futures_util::task::AtomicWaker;
use hyper::body::Incoming;
use hyper::server::conn::http1::Builder;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tower::ServiceExt;

/// Maximum time a connection may make zero forward progress while Hyper is
/// trying to write response bytes. The response pump uses the same budget, so
/// a backpressured large response first releases its detached serialization
/// buffer and then has its socket closed if the peer remains completely idle.
pub const HTTP_WRITE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Total budget for every HTTP/1 request head. Hyper applies this to each
/// keep-alive request and removes it before a handler, response wait, or
/// WebSocket upgrade owns the connection.
pub const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Absolute accepted-connection lifetime. This is longer than the maximum job
/// and result-wait budgets, while still bounding a peer that makes tiny writes
/// often enough to avoid the zero-progress deadline. Cursor replay makes a
/// reconnect safe when a long queued stream reaches this boundary.
pub const HTTP_CONNECTION_MAX_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Hard global bound on accepted HTTP sockets, including upgraded WebSockets.
pub const HTTP_MAX_ACCEPTED_CONNECTIONS: usize = 256;

pub struct WriteTimeoutListener<L> {
    inner: L,
    write_progress_timeout: Duration,
    connection_max_lifetime: Duration,
    connection_slots: Arc<Semaphore>,
    connection_registry: Arc<ConnectionRegistry>,
    max_connections: u32,
}

#[derive(Default)]
struct ConnectionRegistry {
    connections: Mutex<Vec<Weak<ConnectionForceClose>>>,
}

impl ConnectionRegistry {
    fn register(&self) -> Arc<ConnectionForceClose> {
        let connection = Arc::new(ConnectionForceClose::default());
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connections.retain(|connection| connection.strong_count() > 0);
        connections.push(Arc::downgrade(&connection));
        connection
    }

    fn force_close_all(&self) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connections.retain(|connection| {
            if let Some(connection) = connection.upgrade() {
                connection.force_close();
                true
            } else {
                false
            }
        });
    }
}

#[derive(Default)]
struct ConnectionForceClose {
    forced: StdAtomicBool,
    read_waker: AtomicWaker,
    write_waker: AtomicWaker,
}

impl ConnectionForceClose {
    fn force_close(&self) {
        self.forced.store(true, AtomicOrdering::Release);
        self.read_waker.wake();
        self.write_waker.wake();
    }

    fn check<R>(&self, cx: &mut Context<'_>, waker: &AtomicWaker) -> Option<Poll<io::Result<R>>> {
        if self.forced.load(AtomicOrdering::Acquire) {
            return Some(Poll::Ready(Err(Self::error())));
        }
        waker.register(cx.waker());
        if self.forced.load(AtomicOrdering::Acquire) {
            Some(Poll::Ready(Err(Self::error())))
        } else {
            None
        }
    }

    fn error() -> io::Error {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "HTTP connection was force-closed after its drain deadline",
        )
    }
}

struct ForceCloseOnDrop {
    registry: Arc<ConnectionRegistry>,
    armed: bool,
}

impl ForceCloseOnDrop {
    fn new(registry: Arc<ConnectionRegistry>) -> Self {
        Self {
            registry,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ForceCloseOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.registry.force_close_all();
        }
    }
}

impl<L> WriteTimeoutListener<L> {
    pub fn new(inner: L, write_progress_timeout: Duration) -> Self {
        Self::with_limits(
            inner,
            write_progress_timeout,
            HTTP_CONNECTION_MAX_LIFETIME,
            HTTP_MAX_ACCEPTED_CONNECTIONS,
        )
    }

    pub fn with_limits(
        inner: L,
        write_progress_timeout: Duration,
        connection_max_lifetime: Duration,
        max_connections: usize,
    ) -> Self {
        assert!(
            !write_progress_timeout.is_zero(),
            "write progress timeout must be positive"
        );
        assert!(
            !connection_max_lifetime.is_zero(),
            "connection lifetime must be positive"
        );
        assert!(max_connections > 0, "connection capacity must be positive");
        let max_connections = u32::try_from(max_connections)
            .expect("connection capacity must fit Hyper's semaphore accounting");
        Self {
            inner,
            write_progress_timeout,
            connection_max_lifetime,
            connection_slots: Arc::new(Semaphore::new(max_connections as usize)),
            connection_registry: Arc::new(ConnectionRegistry::default()),
            max_connections,
        }
    }
}

impl<L> Listener for WriteTimeoutListener<L>
where
    L: Listener,
    L::Io: Unpin,
{
    type Io = WriteTimeoutIo<L::Io>;
    type Addr = L::Addr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        // Reserve capacity before accepting. When full, the kernel listen
        // backlog provides bounded backpressure and Coop never creates an
        // extra accepted FD or Hyper task. Cancellation during shutdown drops
        // the reservation before the listener itself is dropped.
        let permit = Arc::clone(&self.connection_slots)
            .acquire_owned()
            .await
            .expect("HTTP connection semaphore is never closed");
        let (io, address) = self.inner.accept().await;
        let force_close = self.connection_registry.register();
        (
            WriteTimeoutIo::with_connection_guard(
                io,
                self.write_progress_timeout,
                self.connection_max_lifetime,
                permit,
                force_close,
            ),
            address,
        )
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

pub struct WriteTimeoutIo<T> {
    inner: T,
    timeout: Duration,
    stalled_since: Option<Pin<Box<tokio::time::Sleep>>>,
    timed_out: bool,
    connection_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    connection_expired: bool,
    _connection_permit: Option<OwnedSemaphorePermit>,
    force_close: Option<Arc<ConnectionForceClose>>,
}

impl<T> WriteTimeoutIo<T> {
    pub fn new(inner: T, timeout: Duration) -> Self {
        assert!(
            !timeout.is_zero(),
            "write progress timeout must be positive"
        );
        Self {
            inner,
            timeout,
            stalled_since: None,
            timed_out: false,
            connection_deadline: None,
            connection_expired: false,
            _connection_permit: None,
            force_close: None,
        }
    }

    fn with_connection_guard(
        inner: T,
        timeout: Duration,
        connection_max_lifetime: Duration,
        connection_permit: OwnedSemaphorePermit,
        force_close: Arc<ConnectionForceClose>,
    ) -> Self {
        let mut io = Self::new(inner, timeout);
        io.connection_deadline = Some(Box::pin(tokio::time::sleep(connection_max_lifetime)));
        io._connection_permit = Some(connection_permit);
        io.force_close = Some(force_close);
        io
    }

    fn timeout_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "HTTP peer made no socket write progress before the deadline",
        )
    }

    fn finish_write_poll(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<io::Result<usize>>,
        attempted_bytes: bool,
    ) -> Poll<io::Result<usize>> {
        match result {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.stalled_since = None;
                    Poll::Ready(Ok(written))
                } else if !attempted_bytes {
                    // An empty-buffer probe is not progress and must neither
                    // arm nor reset the timer. Preserve the AsyncWrite result.
                    Poll::Ready(Ok(0))
                } else {
                    self.stalled_since = None;
                    // AsyncWrite defines zero for a non-empty buffer as an
                    // inability to accept more bytes. Surface that terminal
                    // state instead of repeatedly resetting the deadline.
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "HTTP socket accepted zero response bytes",
                    )))
                }
            }
            Poll::Ready(Err(error)) => {
                self.stalled_since = None;
                Poll::Ready(Err(error))
            }
            Poll::Pending if attempted_bytes || self.stalled_since.is_some() => {
                let timeout = self.timeout;
                let stalled_since = self
                    .stalled_since
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                if stalled_since.as_mut().poll(cx).is_ready() {
                    self.timed_out = true;
                    self.stalled_since = None;
                    Poll::Ready(Err(Self::timeout_error()))
                } else {
                    Poll::Pending
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn check_write_timeout<R>(&mut self, cx: &mut Context<'_>) -> Option<Poll<io::Result<R>>> {
        if self.timed_out {
            return Some(Poll::Ready(Err(Self::timeout_error())));
        }
        if self
            .stalled_since
            .as_mut()
            .is_some_and(|deadline| deadline.as_mut().poll(cx).is_ready())
        {
            self.timed_out = true;
            self.stalled_since = None;
            return Some(Poll::Ready(Err(Self::timeout_error())));
        }
        None
    }

    fn connection_lifetime_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "HTTP connection reached its absolute lifetime",
        )
    }

    fn check_connection_lifetime<R>(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Option<Poll<io::Result<R>>> {
        if self.connection_expired {
            return Some(Poll::Ready(Err(Self::connection_lifetime_error())));
        }
        if self
            .connection_deadline
            .as_mut()
            .is_some_and(|deadline| deadline.as_mut().poll(cx).is_ready())
        {
            self.connection_expired = true;
            self.connection_deadline = None;
            return Some(Poll::Ready(Err(Self::connection_lifetime_error())));
        }
        None
    }

    fn check_read_force_close<R>(&self, cx: &mut Context<'_>) -> Option<Poll<io::Result<R>>> {
        self.force_close
            .as_ref()
            .and_then(|force_close| force_close.check(cx, &force_close.read_waker))
    }

    fn check_write_force_close<R>(&self, cx: &mut Context<'_>) -> Option<Poll<io::Result<R>>> {
        self.force_close
            .as_ref()
            .and_then(|force_close| force_close.check(cx, &force_close.write_waker))
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for WriteTimeoutIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(result) = self.check_read_force_close(cx) {
            return result;
        }
        if let Some(result) = self.check_connection_lifetime(cx) {
            return result;
        }
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteTimeoutIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Some(result) = self.check_write_force_close(cx) {
            return result;
        }
        if let Some(result) = self.check_connection_lifetime(cx) {
            return result;
        }
        if let Some(result) = self.check_write_timeout(cx) {
            return result;
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, buffer);
        self.finish_write_poll(cx, result, !buffer.is_empty())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(result) = self.check_write_force_close(cx) {
            return result;
        }
        if let Some(result) = self.check_connection_lifetime(cx) {
            return result;
        }
        if let Some(result) = self.check_write_timeout(cx) {
            return result;
        }
        // A ready flush is not evidence that the peer accepted any bytes and
        // therefore must not reset an outstanding write-progress deadline.
        // Tokio TcpStream has no userspace flush buffer, so its flush itself
        // cannot become the slow-reader wait point.
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(result) = self.check_write_force_close(cx) {
            return result;
        }
        if let Some(result) = self.check_connection_lifetime(cx) {
            return result;
        }
        if let Some(result) = self.check_write_timeout(cx) {
            return result;
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if let Some(result) = self.check_write_force_close(cx) {
            return result;
        }
        if let Some(result) = self.check_connection_lifetime(cx) {
            return result;
        }
        if let Some(result) = self.check_write_timeout(cx) {
            return result;
        }
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, buffers);
        self.finish_write_poll(cx, result, buffers.iter().any(|buffer| !buffer.is_empty()))
    }
}

/// Serve the Coop router with the transport invariants that `axum::serve`
/// does not currently expose. In particular, Hyper's default HTTP/1 header
/// timeout is inert unless a runtime timer is installed explicitly.
pub async fn serve<L, F>(
    listener: WriteTimeoutListener<L>,
    app: Router,
    shutdown: F,
) -> io::Result<()>
where
    L: Listener,
    L::Addr: Debug,
    F: Future<Output = ()> + Send,
{
    serve_with_header_timeout(listener, app, shutdown, HTTP_HEADER_READ_TIMEOUT).await
}

async fn serve_with_header_timeout<L, F>(
    mut listener: WriteTimeoutListener<L>,
    app: Router,
    shutdown: F,
    header_read_timeout: Duration,
) -> io::Result<()>
where
    L: Listener,
    L::Addr: Debug,
    F: Future<Output = ()> + Send,
{
    assert!(
        !header_read_timeout.is_zero(),
        "header read timeout must be positive"
    );
    let connection_slots = Arc::clone(&listener.connection_slots);
    let mut force_close_on_drop = ForceCloseOnDrop::new(Arc::clone(&listener.connection_registry));
    let max_connections = listener.max_connections;
    let (connection_shutdown, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            completion = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completion {
                    tracing::error!(%error, "HTTP connection task failed");
                }
            }
            (io, address) = listener.accept() => {
                let app = app.clone();
                let shutdown = connection_shutdown.subscribe();
                connections.spawn(async move {
                    serve_connection(io, address, app, shutdown, header_read_timeout).await;
                });
            }
        }
    }

    drop(listener);
    connection_shutdown.send_replace(true);
    while let Some(completion) = connections.join_next().await {
        if let Err(error) = completion {
            tracing::error!(%error, "HTTP connection task failed during shutdown");
        }
    }
    // HTTP upgrade transfers the guarded IO (and therefore its permit) into
    // Axum's WebSocket task. Waiting for every slot makes graceful shutdown
    // include upgraded sockets as well; main's outer HTTP drain deadline is
    // still the forced-abort bound if an application task fails to close.
    let all_connections = connection_slots
        .acquire_many_owned(max_connections)
        .await
        .expect("HTTP connection semaphore is never closed");
    drop(all_connections);
    force_close_on_drop.disarm();
    Ok(())
}

async fn serve_connection<I, A>(
    io: I,
    address: A,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
    header_read_timeout: Duration,
) where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    A: Debug,
{
    let tower_service = app.map_request(|request: hyper::Request<Incoming>| request.map(Body::new));
    let hyper_service = TowerToHyperService::new(tower_service);
    let mut builder = Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    let connection = builder
        .serve_connection(TokioIo::new(io), hyper_service)
        .with_upgrades();
    tokio::pin!(connection);

    tokio::select! {
        result = &mut connection => {
            if let Err(error) = result {
                tracing::debug!(?address, %error, "HTTP connection closed with an error");
            }
        }
        result = shutdown.changed() => {
            let stopping = result.is_ok() && *shutdown.borrow();
            if stopping {
                connection.as_mut().graceful_shutdown();
                if let Err(error) = connection.await {
                    tracing::debug!(?address, %error, "HTTP connection failed while draining");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LifetimeAdmission, TryLifetimeError};
    use axum::body::{Body, Bytes};
    use axum::extract::ws::WebSocketUpgrade;
    use axum::response::Response;
    use axum::routing::get;
    use axum::Router;
    use std::convert::Infallible;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;

    async fn read_http_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1_024];
        loop {
            let count = stream.read(&mut chunk).await?;
            if count == 0 || received.windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(received);
            }
            received.extend_from_slice(&chunk[..count]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(received);
            }
            if received.len() > 16 * 1_024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "test response head exceeded 16 KiB",
                ));
            }
        }
    }

    async fn wait_for_peer_close(stream: &mut TcpStream) -> io::Result<()> {
        let mut buffer = [0_u8; 1_024];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error),
            }
        }
    }

    struct ZeroProgressIo {
        request: &'static [u8],
        request_offset: usize,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for ZeroProgressIo {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl AsyncRead for ZeroProgressIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.request_offset == self.request.len() {
                return Poll::Pending;
            }
            let count = buffer
                .remaining()
                .min(self.request.len() - self.request_offset);
            let end = self.request_offset + count;
            buffer.put_slice(&self.request[self.request_offset..end]);
            self.request_offset = end;
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for ZeroProgressIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    struct OneShotListener {
        io: Option<ZeroProgressIo>,
        address: SocketAddr,
    }

    struct FlushReadyWithoutWriteProgress;

    struct FlushPendingWithoutWriteProgress;

    impl AsyncWrite for FlushReadyWithoutWriteProgress {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for FlushPendingWithoutWriteProgress {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl Listener for OneShotListener {
        type Io = ZeroProgressIo;
        type Addr = SocketAddr;

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            match self.io.take() {
                Some(io) => (io, self.address),
                None => std::future::pending().await,
            }
        }

        fn local_addr(&self) -> io::Result<Self::Addr> {
            Ok(self.address)
        }
    }

    #[tokio::test]
    async fn stalled_socket_write_becomes_a_terminal_timeout() {
        let (server, _unread_client) = tokio::io::duplex(64);
        let mut io = WriteTimeoutIo::new(server, Duration::from_millis(10));
        let error = io
            .write_all(&vec![0_u8; 4_096])
            .await
            .expect_err("unread socket must time out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        let error = io
            .write_all(b"again")
            .await
            .expect_err("timed-out transport stays failed");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn write_progress_resets_deadline() {
        let (server, mut client) = tokio::io::duplex(64);
        let mut io = WriteTimeoutIo::new(server, Duration::from_millis(30));
        let reader = tokio::spawn(async move {
            let mut total = 0;
            let mut buffer = [0_u8; 16];
            while total < 512 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                total += client.read(&mut buffer).await.expect("read progress");
            }
        });
        tokio::time::timeout(Duration::from_secs(1), io.write_all(&vec![1_u8; 512]))
            .await
            .expect("overall test deadline")
            .expect("periodic progress keeps connection alive");
        reader.await.expect("reader task");
    }

    #[tokio::test]
    async fn ready_flush_cannot_masquerade_as_socket_write_progress() {
        let mut io = WriteTimeoutIo::new(FlushReadyWithoutWriteProgress, Duration::from_millis(10));
        let error = std::future::poll_fn(|cx| {
            let result = Pin::new(&mut io).poll_write(cx, b"response");
            if result.is_pending() {
                assert!(Pin::new(&mut io).poll_flush(cx).is_ready());
            }
            result
        })
        .await
        .expect_err("flush readiness must not reset the write-progress clock");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn armed_write_deadline_is_enforced_while_only_flush_is_polled() {
        let mut io =
            WriteTimeoutIo::new(FlushPendingWithoutWriteProgress, Duration::from_millis(10));
        std::future::poll_fn(|cx| {
            assert!(Pin::new(&mut io).poll_write(cx, b"response").is_pending());
            Poll::Ready(())
        })
        .await;
        let error = std::future::poll_fn(|cx| Pin::new(&mut io).poll_flush(cx))
            .await
            .expect_err("an armed write deadline must survive operation changes");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn read_only_idle_time_is_not_a_write_timeout() {
        let (server, mut client) = tokio::io::duplex(64);
        let mut io = WriteTimeoutIo::new(server, Duration::from_millis(10));
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            client.write_all(b"ping").await.expect("client write");
        });
        let mut received = [0_u8; 4];
        io.read_exact(&mut received)
            .await
            .expect("idle reads remain valid");
        assert_eq!(&received, b"ping");
        writer.await.expect("writer task");
    }

    #[tokio::test]
    async fn header_deadline_closes_silent_partial_preface_and_keepalive_slowloris() {
        let handled = Arc::new(AtomicUsize::new(0));
        let handler_count = Arc::clone(&handled);
        let app = Router::new().route(
            "/",
            get(move || {
                let handled = Arc::clone(&handler_count);
                async move {
                    handled.fetch_add(1, Ordering::AcqRel);
                    "ok"
                }
            }),
        );
        let raw_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test listener");
        let address = raw_listener.local_addr().expect("test listener address");
        let listener = WriteTimeoutListener::with_limits(
            raw_listener,
            Duration::from_secs(1),
            Duration::from_secs(5),
            1,
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            serve_with_header_timeout(
                listener,
                app,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_millis(150),
            )
            .await
        });

        // A peer that sends no bytes is still covered by Hyper's H1 head
        // deadline; no application auth/admission is required to reclaim it.
        let mut silent = TcpStream::connect(address).await.expect("silent peer");
        tokio::time::timeout(Duration::from_secs(1), wait_for_peer_close(&mut silent))
            .await
            .expect("silent peer is closed at the header deadline")
            .expect("observe silent peer close");

        // Direct HTTP/1 serving avoids hyper-util's untimed H2-preface sniff.
        // An exact partial preface is therefore bounded by the same deadline.
        let mut partial_preface = TcpStream::connect(address)
            .await
            .expect("partial-preface peer");
        partial_preface
            .write_all(b"PRI * HTTP/2.0\r\n\r")
            .await
            .expect("write partial H2 preface");
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_peer_close(&mut partial_preface),
        )
        .await
        .expect("partial H2 preface is closed at the header deadline")
        .expect("observe partial-preface close");

        // The timer is reset for every keep-alive request, not only the first
        // request on a connection.
        let mut keep_alive = TcpStream::connect(address).await.expect("keep-alive peer");
        keep_alive
            .write_all(b"GET / HTTP/1.1\r\nHost: coop.test\r\nConnection: keep-alive\r\n\r\n")
            .await
            .expect("write first complete request");
        let first = tokio::time::timeout(Duration::from_secs(1), read_http_head(&mut keep_alive))
            .await
            .expect("first response deadline")
            .expect("first response");
        assert!(
            String::from_utf8_lossy(&first).starts_with("HTTP/1.1 200"),
            "unexpected first response: {}",
            String::from_utf8_lossy(&first)
        );
        keep_alive
            .write_all(b"GET / HTTP/1.1\r\nHost:")
            .await
            .expect("write incomplete second head");
        tokio::time::timeout(Duration::from_secs(1), wait_for_peer_close(&mut keep_alive))
            .await
            .expect("second request head is independently timed")
            .expect("observe keep-alive slowloris close");
        assert_eq!(handled.load(Ordering::Acquire), 1);

        // A subsequent valid peer proves that each timed-out connection
        // returned its sole global capacity permit.
        let mut reclaimed = TcpStream::connect(address)
            .await
            .expect("connection after timeout");
        reclaimed
            .write_all(b"GET / HTTP/1.1\r\nHost: coop.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("write request after reclaim");
        let response = tokio::time::timeout(Duration::from_secs(1), read_http_head(&mut reclaimed))
            .await
            .expect("reclaimed response deadline")
            .expect("reclaimed response");
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        assert_eq!(handled.load(Ordering::Acquire), 2);
        drop(reclaimed);

        let _ = shutdown_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("header test server honors shutdown")
            .expect("header test server task did not panic")
            .expect("header test server stopped cleanly");
    }

    #[tokio::test]
    async fn connection_cap_survives_websocket_upgrade_until_absolute_lifetime() {
        let app = Router::new()
            .route(
                "/ws",
                get(|upgrade: WebSocketUpgrade| async move {
                    upgrade.on_upgrade(|mut socket| async move {
                        while socket.recv().await.is_some() {}
                    })
                }),
            )
            .route("/", get(|| async { "ok" }));
        let raw_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind WebSocket test listener");
        let address = raw_listener.local_addr().expect("test listener address");
        let listener = WriteTimeoutListener::with_limits(
            raw_listener,
            Duration::from_secs(1),
            Duration::from_millis(500),
            1,
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            serve_with_header_timeout(
                listener,
                app,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_millis(100),
            )
            .await
        });

        let mut websocket = TcpStream::connect(address).await.expect("WebSocket peer");
        websocket
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: coop.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            )
            .await
            .expect("write WebSocket handshake");
        let switching =
            tokio::time::timeout(Duration::from_secs(1), read_http_head(&mut websocket))
                .await
                .expect("WebSocket handshake deadline")
                .expect("WebSocket handshake response");
        assert!(
            String::from_utf8_lossy(&switching).starts_with("HTTP/1.1 101"),
            "unexpected upgrade response: {}",
            String::from_utf8_lossy(&switching)
        );

        // Wait beyond the request-head deadline. The upgrade must remain
        // alive and retain the only accepted-connection slot.
        tokio::time::sleep(Duration::from_millis(175)).await;
        let mut queued = TcpStream::connect(address)
            .await
            .expect("backpressured second peer");
        queued
            .write_all(b"GET / HTTP/1.1\r\nHost: coop.test\r\nConnection: close\r\n\r\n")
            .await
            .expect("write queued request");
        let mut probe = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), queued.read(&mut probe))
                .await
                .is_err(),
            "second peer must remain backpressured while upgraded IO owns capacity"
        );

        // The absolute lifetime bounds a peer that makes enough tiny writes
        // to evade a pure zero-progress timeout. Once it expires, the queued
        // connection is admitted and receives its response.
        let queued_response =
            tokio::time::timeout(Duration::from_secs(1), read_http_head(&mut queued))
                .await
                .expect("queued request admitted after WebSocket lifetime")
                .expect("queued response");
        assert!(String::from_utf8_lossy(&queued_response).starts_with("HTTP/1.1 200"));
        wait_for_peer_close(&mut websocket)
            .await
            .expect("absolute lifetime closes upgraded socket");
        drop(queued);

        let _ = shutdown_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("WebSocket test server honors shutdown")
            .expect("WebSocket test server task did not panic")
            .expect("WebSocket test server stopped cleanly");
    }

    #[tokio::test]
    async fn dropping_server_force_closes_upgraded_socket_and_reclaims_capacity() {
        let upgrade_started = Arc::new(Notify::new());
        let handler_started = Arc::clone(&upgrade_started);
        let app = Router::new().route(
            "/ws",
            get(move |upgrade: WebSocketUpgrade| {
                let handler_started = Arc::clone(&handler_started);
                async move {
                    upgrade.on_upgrade(move |mut socket| async move {
                        handler_started.notify_one();
                        let _ = socket.recv().await;
                    })
                }
            }),
        );
        let raw_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind forced-drain test listener");
        let address = raw_listener.local_addr().expect("test listener address");
        let listener = WriteTimeoutListener::with_limits(
            raw_listener,
            Duration::from_secs(5),
            Duration::from_secs(5),
            1,
        );
        let connection_slots = Arc::clone(&listener.connection_slots);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            serve_with_header_timeout(
                listener,
                app,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_secs(1),
            )
            .await
        });

        let mut websocket = TcpStream::connect(address)
            .await
            .expect("forced-drain WebSocket peer");
        websocket
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: coop.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            )
            .await
            .expect("write forced-drain WebSocket handshake");
        let switching =
            tokio::time::timeout(Duration::from_secs(1), read_http_head(&mut websocket))
                .await
                .expect("forced-drain handshake deadline")
                .expect("forced-drain handshake response");
        assert!(String::from_utf8_lossy(&switching).starts_with("HTTP/1.1 101"));
        tokio::time::timeout(Duration::from_secs(1), upgrade_started.notified())
            .await
            .expect("upgrade handler started");

        let _ = shutdown_tx.send(());
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            !server.is_finished(),
            "graceful drain must wait while upgraded IO owns its permit"
        );

        // Main drops the server future after its bounded graceful-drain wait.
        // Aborting this task exercises the same future-drop path: the
        // transport registry wakes both read and write halves with a terminal
        // error, including IO already moved into Hyper's detached upgrade.
        server.abort();
        let _ = server.await;
        tokio::time::timeout(Duration::from_secs(1), wait_for_peer_close(&mut websocket))
            .await
            .expect("forced drain closes upgraded peer")
            .expect("observe forced upgraded-peer close");
        let permit = tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&connection_slots).acquire_owned(),
        )
        .await
        .expect("forced drain reclaims accepted-connection capacity")
        .expect("connection semaphore remains open");
        drop(permit);
    }

    #[tokio::test]
    async fn stalled_http_reader_closes_connection_and_reclaims_body_admission() {
        let admission = LifetimeAdmission::new(1, 1);
        let handler_admission = admission.clone();
        let acquired = Arc::new(Notify::new());
        let handler_acquired = Arc::clone(&acquired);
        let app = Router::new().route(
            "/",
            get(move || {
                let admission = handler_admission.clone();
                let acquired = Arc::clone(&handler_acquired);
                async move {
                    let permit = admission
                        .try_acquire("tenant-a")
                        .expect("first response owns admission");
                    acquired.notify_one();
                    // This body deliberately never reaches EOF. The only way
                    // to reclaim its permit is for Hyper to drop the response
                    // after the transport reports a zero-progress timeout.
                    let stream = futures_util::stream::poll_fn(move |_cx| {
                        let _keep_permit_alive = &permit;
                        Poll::<Option<Result<Bytes, Infallible>>>::Pending
                    });
                    Response::new(Body::from_stream(stream))
                }
            }),
        );

        let dropped = Arc::new(AtomicBool::new(false));
        let listener = OneShotListener {
            io: Some(ZeroProgressIo {
                request: b"GET / HTTP/1.1\r\nHost: coop.test\r\n\r\n",
                request_offset: 0,
                dropped: Arc::clone(&dropped),
            }),
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, 7300)),
        };
        let listener = WriteTimeoutListener::new(listener, Duration::from_millis(25));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            serve_with_header_timeout(
                listener,
                app,
                async move {
                    let _ = shutdown_rx.await;
                },
                Duration::from_secs(1),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), acquired.notified())
            .await
            .expect("server constructed the guarded response");
        assert_eq!(
            admission.try_acquire("tenant-a").err(),
            Some(TryLifetimeError::GlobalFull),
            "response body must keep admission while the socket is stalled"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if dropped.load(Ordering::Acquire) {
                    if let Ok(reclaimed) = admission.try_acquire("tenant-a") {
                        drop(reclaimed);
                        break;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("write timeout closes the connection and drops its response body");

        let _ = shutdown_tx.send(());
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("server honors graceful shutdown")
            .expect("server task did not panic")
            .expect("server stopped cleanly");
    }
}
