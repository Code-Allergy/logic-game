//! The server over HTTP on this machine: how the running game is reached
//! (`docs/mcp-server.md`, "Safety").
//!
//! [`serve`] binds a free port on 127.0.0.1 and serves the tools at
//! `/mcp` (MCP's Streamable HTTP transport) on a thread of its own. Every
//! request must carry `Authorization: Bearer <token>`, a token made for
//! this run. The address and the token are written to `mcp.json` in the
//! game's data folder, for clients (and their owners) to read.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};

use super::server::Server;

/// The file in the data folder that says where the server is.
pub const ENDPOINT_FILE: &str = "mcp.json";

/// Where a running server is reached.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// `http://127.0.0.1:<port>/mcp`.
    pub url: String,
    pub token: String,
    /// The file the two were written to, if there is a data folder.
    pub file: Option<PathBuf>,
}

impl Endpoint {
    /// The contents of [`ENDPOINT_FILE`].
    pub fn json(&self) -> String {
        format!(
            "{{\n  \"url\": \"{}\",\n  \"token\": \"{}\",\n  \"pid\": {}\n}}\n",
            self.url,
            self.token,
            std::process::id()
        )
    }
}

/// Serve `server` on a free local port until the process ends. With
/// `folder`, the endpoint is written to [`ENDPOINT_FILE`] in it.
pub fn serve(server: Server, folder: Option<PathBuf>) -> Result<Endpoint, String> {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let expected = format!("Bearer {token}");
    let (started, port) = std::sync::mpsc::channel::<Result<u16, String>>();
    std::thread::Builder::new()
        .name("gatequest-mcp".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = started.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        let _ = started.send(Err(format!("could not open a local port: {e}")));
                        return;
                    }
                };
                let port = match listener.local_addr() {
                    Ok(addr) => addr.port(),
                    Err(e) => {
                        let _ = started.send(Err(e.to_string()));
                        return;
                    }
                };
                let service = StreamableHttpService::new(
                    move || Ok(server.clone()),
                    Arc::new(LocalSessionManager::default()),
                    StreamableHttpServerConfig::default(),
                );
                let router = axum::Router::new().nest_service("/mcp", service).layer(
                    axum::middleware::from_fn(move |request: Request, next: Next| {
                        let expected = expected.clone();
                        async move {
                            let given = request
                                .headers()
                                .get(header::AUTHORIZATION)
                                .and_then(|v| v.to_str().ok());
                            if given == Some(expected.as_str()) {
                                next.run(request).await
                            } else {
                                unauthorized()
                            }
                        }
                    }),
                );
                let _ = started.send(Ok(port));
                if let Err(e) = axum::serve(listener, router).await {
                    eprintln!("gatequest-mcp: the server stopped: {e}");
                }
            });
        })
        .map_err(|e| format!("could not start the server thread: {e}"))?;
    let port = port
        .recv()
        .map_err(|_| "the server thread ended before it started".to_string())??;
    let mut endpoint = Endpoint {
        url: format!("http://127.0.0.1:{port}/mcp"),
        token,
        file: None,
    };
    if let Some(folder) = folder {
        let path = folder.join(ENDPOINT_FILE);
        let written = std::fs::create_dir_all(&folder)
            .and_then(|()| write_private(&path, endpoint.json().as_bytes()));
        match written {
            Ok(()) => endpoint.file = Some(path),
            Err(e) => eprintln!("gatequest-mcp: could not write {}: {e}", path.display()),
        }
    }
    Ok(endpoint)
}

/// Write `contents` to `path` for its owner only. The file holds the
/// token, and whoever has the token can use every tool (which read and
/// write files with the game's rights): on a machine with other accounts
/// it must not be readable by them. On Unix the file is made, or an
/// existing one changed to, mode 0600 before the token is written; on
/// other systems the folder's own protection applies.
fn write_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // The mode above is for a new file: one left by an earlier run
        // keeps the mode it had.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn the_endpoint_file_is_for_its_owner_only() {
        let dir = std::env::temp_dir().join(format!(
            "gatequest-mcp-endpoint-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(super::ENDPOINT_FILE);
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        super::write_private(&path, b"first").unwrap();
        assert_eq!(mode(&path), 0o600);
        // A file an earlier version left readable by others is closed too.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::write_private(&path, b"second").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        "this server needs \"Authorization: Bearer <token>\"; the token is in mcp.json in the \
         game's data folder\n",
    )
        .into_response()
}
