mod _duplex;
use _duplex::Duplex;
use _testdata::create_test_entries;
use harddrive_party::run::Run;
use tempfile::TempDir;
mod _testdata;
// use async_std::prelude::FutureExt;
use futures::future::join_all;

#[async_std::test]
async fn run() {
    env_logger::init();
    let storage_a = TempDir::new().unwrap();
    let mut peer_a = Run::new(storage_a).await.unwrap();
    peer_a.rpc.shares.scan("tests/test-data").await.unwrap();

    let storage_b = TempDir::new().unwrap();
    let mut peer_b = Run::new(storage_b).await.unwrap();

    let (ar, bw) = sluice::pipe::pipe();
    let (br, aw) = sluice::pipe::pipe();

    // wait for peers to handshake
    join_all(vec![
        peer_a.handle_peer(Box::new(Duplex::new(br, bw)), true),
        peer_b.handle_peer(Box::new(Duplex::new(ar, aw)), false),
    ])
    .await;

    async_std::task::spawn(async move {
        peer_a.run().await;
    });
    let entries = peer_b.request_all().await;
    assert_eq!(entries, create_test_entries());
    let file_contents = peer_b.read_file("test-data/somefile").await;
    assert_eq!(file_contents, "boop\n");
}
