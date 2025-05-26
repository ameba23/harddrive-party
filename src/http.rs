//! Http server for serving web-ui as well as static locally available files
use crate::{hdp::SharedState, ws::handle_socket};
use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, State,
    },
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use log::error;
use mime_guess::from_path;
use rust_embed::RustEmbed;
use std::{net::SocketAddr, path::PathBuf};
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppState {}

#[derive(RustEmbed)]
#[folder = "dist"]
struct WebUi;

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

// pub async fn http_server(ws_addr: SocketAddr, download_dir: PathBuf) {
//     let http_sockect_addr = SocketAddr::new(ws_addr.ip(), 3030);
//     let embed_serve = warp_embed::embed(&WebUi);
//
//     // This lets the web ui be able to serve files which have been downloaded
//     let static_serve_downloads = warp::path("downloads").and(warp::fs::dir(download_dir));
//     // TODO this should be the actual share directory - the problem is we dont know it when this
//     // function is called
//     let static_serve_shares = warp::path("shared").and(warp::fs::dir("extra-test-data/alice"));
//
//     println!("Web UI served on http://{}", http_sockect_addr);
//     warp::serve(
//         embed_serve
//             .or(static_serve_shares)
//             .or(static_serve_downloads),
//     )
//     .run(http_sockect_addr)
//     .await;
// }
pub async fn http_server(
    shared_state: SharedState,
    addr: SocketAddr,
    download_dir: PathBuf,
) -> anyhow::Result<()> {
    let app = Router::new()
        .nest_service("/downloads", ServeDir::new(download_dir))
        // .route("/peers", get(peers))
        // .route("/files", get(files))
        // .route("/download", post(download))
        // .route("/shares", get(shares))
        // .route("/shares", put(put_shares))
        // .route("/shares", delete(delete_shares))
        // .route("/connect", post(connect))
        // .route("/connect", delete(delete_connect))
        // .route("/requests", get(requests))
        // .route("/request", get(request))
        .route("/ws", any(ws_handler))
        .route("/{path}", get(static_handler))
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            error!("HTTP server error: {err:?}");
        }
    });

    println!("Web UI served on http://{}", addr);
    Ok(())
}

async fn ws_handler(
    State(shared_state): State<SharedState>,
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(shared_state, socket, addr))
}
