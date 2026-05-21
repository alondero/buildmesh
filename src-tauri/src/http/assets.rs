//! Static asset serving for the mobile web app.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

const MOBILE_APP_HTML: &str = include_str!("../mobile_app.html");

pub async fn serve_mobile_root(
    lines: &mut tokio::io::BufStream<TcpStream>,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
        MOBILE_APP_HTML.len(),
        MOBILE_APP_HTML
    );
    lines.get_mut().write_all(response.as_bytes()).await
}
