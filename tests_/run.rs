mod _duplex;
use _duplex::Duplex;
use _testdata::create_test_entries;
use harddrive_party::{messages::response::ls::Entry, run::Run};
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

    let peer_a_name = peer_a.name.clone();

    async_std::task::spawn(async move {
        assert!(peer_a.run().await.is_ok());
    });

    // Top level recursive query
    let entries = peer_b.ls(None, None, true).await;
    let test_entries: Vec<Entry> = create_test_entries()
        .iter()
        .map(|e| Entry {
            name: format!("{}/{}", peer_a_name, e.name.clone()),
            size: e.size,
            is_dir: e.is_dir,
        })
        .collect();

    assert_eq!(entries, test_entries);

    // Top level non-recursive query
    let entries = peer_b.ls(None, None, false).await;

    assert_eq!(
        entries,
        vec![Entry {
            name: format!("{}/", peer_a_name),
            size: 17,
            is_dir: true,
        }]
    );

    let file_contents = peer_b
        .read_file(&format!("{}/test-data/somefile", peer_a_name))
        .await;
    assert_eq!(file_contents, "boop\n");
}
