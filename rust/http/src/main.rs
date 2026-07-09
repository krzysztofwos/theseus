//! The `http-server` binary: Theseus's operations over HTTP — the Http inbound
//! adapter (L4).
//!
//! One route serves every operation, `POST /{operation}` with a JSON body. The
//! generated handlers parse the body into the operation's request, run it against
//! a serialized stateful session, and map the outcome onto the status line:
//! 200 a result, 400 a body that does not parse, 404 an unknown operation, 501
//! an operation with no authored handler, 403 a write the gate refused, 500 any
//! other error. Writes are refused unless the server is launched with
//! `--allow-writes`.

use std::sync::Arc;

use theseus::StatefulSession;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut allow_writes = false;
    let mut address = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            flag if flag.starts_with("--") => {
                anyhow::bail!(
                    "unknown flag `{flag}`; usage: http-server [--allow-writes] [address]"
                )
            }
            _ => address = Some(arg),
        }
    }
    let listen = address.unwrap_or_else(|| "127.0.0.1:4870".to_string());
    let router = theseus_http::router(Arc::new(StatefulSession::at_repo_root(allow_writes)));
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("listening on http://{listen}");
    axum::serve(listener, router).await?;
    Ok(())
}
