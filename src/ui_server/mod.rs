//! Http server for serving web-ui as well as static locally available files
pub mod api;
pub mod ws;
pub use harddrive_party_shared::client;

use crate::{
    ui_server::{
        api::{
            delete_shares, get_info, get_request, get_requests, post_close, post_connect,
            post_download, post_files, post_read, post_shares, put_shares,
        },
        ws::handle_socket,
    },
    SharedState,
};
use axum::{
    body::Body,
    extract::{ws::WebSocketUpgrade, FromRequest, Path, Request, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::{any, delete, get, post, put},
    Router,
};
use log::error;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use serde::{de::DeserializeOwned, Serialize};
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
        .nest(
            "/api",
            Router::new()
                .route("/connect", post(post_connect))
                .route("/files", post(post_files))
                .route("/download", post(post_download))
                .route("/shares", post(post_shares))
                .route("/info", get(get_info))
                .route("/read", post(post_read))
                .route("/shares", put(put_shares))
                .route("/shares", delete(delete_shares))
                // .route("/connect", delete(delete_connect))
                .route("/requests", get(get_requests))
                .route("/request", get(get_request))
                // .route("/request", delete(delete_request))
                .route("/close", post(post_close)),
        )
        .route("/ws", any(ws_handler))
        .route("/", get(index))
        .route("/peers", get(index))
        .route("/shares", get(index))
        .route("/transfers", get(index))
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
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(shared_state, socket))
}

/// For statically serving the front end embedded in the binary
#[derive(RustEmbed)]
#[folder = "dist"]
struct WebUi;

async fn index() -> impl IntoResponse {
    static_handler(Path("index.html".to_string())).await
}

/// Statically serve the front end
async fn static_handler(Path(file_path): Path<String>) -> impl IntoResponse {
    match WebUi::get(&file_path) {
        Some(content) => {
            let body = content.data.into_owned();
            let mime = from_path(file_path).first_or_octet_stream();
            match Response::builder()
                .header("Content-Type", mime.as_ref())
                .body(body.into())
            {
                Ok(res) => res,
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
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

impl<T> IntoResponse for Bincode<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response<Body> {
        match bincode::serialize(&self.0) {
            Ok(bytes) => (
                [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                bytes,
            )
                .into_response(),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to serialize response",
            )
                .into_response(),
        }
    }
}
