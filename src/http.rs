use rust_embed::RustEmbed;
use warp::Filter;

#[derive(RustEmbed)]
#[folder = "web-ui/dist"]
struct WebUi;

pub async fn http_server() {
    // TODO we will statically serve the downloads dir
    // as well as the shared directories
    let embed_serve = warp_embed::embed(&WebUi);
    let static_serve = warp::path("shared").and(warp::fs::dir("extra-test-data/alice"));
    println!("Web UI served on http://127.0.0.1:3030");
    warp::serve(embed_serve.or(static_serve))
        .run(([127, 0, 0, 1], 3030))
        .await;
}
