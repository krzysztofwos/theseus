//! The `http-server` binary: Theseus's operations over HTTP — the Http inbound
//! adapter (L4).
//!
//! One route serves every operation, `POST /{operation}` with a JSON body. The
//! generated handlers parse the body into the operation's request, run it against
//! the standalone composition root, and map the outcome onto the status line:
//! 200 a result, 400 a body that does not parse, 404 an unknown operation, 501
//! an operation with no authored handler, 403 a write the gate refused, 500 any
//! other error. Writes are refused unless the server is launched with
//! `--allow-writes`.

use std::sync::Arc;

use theseus::Standalone;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let allow_writes = std::env::args().skip(1).any(|arg| arg == "--allow-writes");
    let listen = std::env::args()
        .skip(1)
        .find(|arg| arg != "--allow-writes")
        .unwrap_or_else(|| "127.0.0.1:4870".to_string());
    let router = theseus_http::router(Arc::new(Standalone::new(allow_writes)));
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("listening on http://{listen}");
    axum::serve(listener, router).await?;
    Ok(())
}
