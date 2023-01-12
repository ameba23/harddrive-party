mod _duplex;
use _duplex::Duplex;
use harddrive_party::{messages::response::ls::Entry, run::Run};
use tempfile::TempDir;

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

    peer_a
        .handle_peer(Box::new(Duplex::new(br, bw)), true)
        .await;

    peer_b
        .handle_peer(Box::new(Duplex::new(ar, aw)), false)
        .await;

    async_std::task::spawn(async move {
        peer_a.run().await;
    });
    let entries = peer_b.request_all().await;
    assert_eq!(
        entries,
        vec![
            Entry {
                name: "".to_string(),
                size: 17,
                is_dir: true
            },
            Entry {
                name: "test-data".to_string(),
                size: 17,
                is_dir: true
            },
            Entry {
                name: "test-data/subdir".to_string(),
                size: 12,
                is_dir: true
            },
            Entry {
                name: "test-data/subdir/subsubdir".to_string(),
                size: 6,
                is_dir: true
            },
            Entry {
                name: "test-data/somefile".to_string(),
                size: 5,
                is_dir: false
            },
            Entry {
                name: "test-data/subdir/anotherfile".to_string(),
                size: 6,
                is_dir: false
            },
            Entry {
                name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
                size: 6,
                is_dir: false
            }
        ]
    )
}
