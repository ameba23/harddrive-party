use async_std::prelude::*;
use harddrive_party::{messages::request, rpc::Rpc, shares::Shares};
use tempfile::TempDir;

#[async_std::test]
async fn basic_rpc() -> anyhow::Result<()> {
    let storage = TempDir::new().unwrap();
    let mut shares = Shares::new(storage).await.unwrap();
    shares.scan("/home/turnip/Hipax").await.unwrap();
    let mut rpc = Rpc::new(shares);

    let req = request::Msg::Ls(request::Ls {
        path: None,
        searchterm: None,
        recursive: None,
    });

    let mut responses = Box::into_pin(rpc.request(req).await);
    while let Some(res) = responses.next().await {
        println!("response {:?}", res);
    }
    Ok(())
}
