//! Http server for serving web-ui as well as static locally available files
use rust_embed::RustEmbed;
use std::{net::SocketAddr, path::PathBuf};
use warp::Filter;

#[derive(RustEmbed)]
#[folder = "web-ui/dist"]
struct WebUi;

pub async fn http_server(ws_addr: SocketAddr, download_dir: PathBuf) {
    let http_sockect_addr = SocketAddr::new(ws_addr.ip(), 3030);
    let embed_serve = warp_embed::embed(&WebUi);

    // This lets the web ui be able to serve files which have been downloaded
    let static_serve_downloads = warp::path("downloads").and(warp::fs::dir(download_dir));
    // TODO this should be the actual share directory - the problem is we dont know it when this
    // function is called
    let static_serve_shares = warp::path("shared").and(warp::fs::dir("extra-test-data/alice"));

    println!("Web UI served on http://{}", http_sockect_addr);
    warp::serve(
        embed_serve
            .or(static_serve_shares)
            .or(static_serve_downloads),
    )
    .run(http_sockect_addr)
    .await;
}
