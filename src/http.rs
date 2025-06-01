//! Http server for serving web-ui as well as static locally available files
use crate::{
    api::{post_connect, post_files, post_shares},
    hdp::SharedState,
    ws::handle_socket,
};
use axum::{
    body::Body,
    extract::{ws::WebSocketUpgrade, ConnectInfo, FromRequest, Path, Request, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::{any, get, post},
    Router,
};
use log::error;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde::de::DeserializeOwned;
use std::net::SocketAddr;
use tower_http::services::ServeDir;

pub async fn http_server(
    shared_state: SharedState,
    addr: SocketAddr,
) -> anyhow::Result<SocketAddr> {
    let app = Router::new()
        .nest_service(
            "/downloads",
            ServeDir::new(shared_state.download_dir.clone()),
        )
        .route("/connect", post(post_connect))
        .route("/files", post(post_files))
        // .route("/download", post(download))
        .route("/shares", post(post_shares))
        // .route("/shares", put(put_shares))
        // .route("/shares", delete(delete_shares))
        // .route("/connect", delete(delete_connect))
        // .route("/requests", get(requests))
        // .route("/request", get(request))
        .route("/ws", any(ws_handler))
        .route("/{path}", get(static_handler))
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            error!("HTTP server error: {err:?}");
        }
    });

    Ok(addr)
}

async fn ws_handler(
    State(shared_state): State<SharedState>,
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(shared_state, socket, addr))
}

/// For statically serving the front end embedded in the binary
#[derive(RustEmbed)]
#[folder = "dist"]
struct WebUi;

/// Statically serve the front end
async fn static_handler(Path(file_path): Path<String>) -> impl IntoResponse {
    match WebUi::get(&file_path) {
        Some(content) => {
            let body = content.data.into_owned();
            let mime = from_path(file_path).first_or_octet_stream();
            Response::builder()
                .header("Content-Type", mime.as_ref())
                .body(body.into())
                .unwrap()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Bincode extractor for axum
pub struct Bincode<T>(pub T);

impl<T, S> FromRequest<S, Body> for Bincode<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, String);

    async fn from_request(req: Request<Body>, _state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
            .await
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    "Failed to read request body".into(),
                )
            })?;

        let obj = bincode::deserialize::<T>(&bytes).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Failed to decode bincode body".into(),
            )
        })?;

        Ok(Bincode(obj))
    }
}
