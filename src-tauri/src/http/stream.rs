//! A server connection that is either plain TCP or TLS (issue #501).
//!
//! The embedded HTTP server parses requests over one concrete stream type, and
//! every route handler is typed against it. Rather than make all of them
//! generic over `AsyncRead + AsyncWrite`, this enum *is* that one type:
//! loopback connections stay [`MaybeTls::Plain`]; connections accepted on an
//! externally-reachable interface arrive already wrapped as [`MaybeTls::Tls`].
//!
//! Both inner stream types are `Unpin`, so the `AsyncRead`/`AsyncWrite` impls
//! project through `Pin::new` without any `unsafe`.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

pub enum MaybeTls {
    Plain(TcpStream),
    /// Boxed because a `TlsStream` is far larger than a `TcpStream`; boxing
    /// keeps the enum (and every `BufStream` holding one) small.
    Tls(Box<TlsStream<TcpStream>>),
}

impl MaybeTls {
    /// Whether this connection is encrypted. Since the server terminates TLS
    /// itself (loopback arrives `Plain`, externally-reachable interfaces arrive
    /// `Tls`, issue #501), this is the authoritative request scheme — used to
    /// gate the `Secure` cookie attribute (issue #553).
    pub fn is_tls(&self) -> bool {
        matches!(self, MaybeTls::Tls(_))
    }
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
