use futures::StreamExt;
use harddrive_party::{
    ui_messages::{DownloadInfo, FilesQuery, PeerPath, UiEvent, UiServerError},
    ui_server::{client::Client, http_server},
    wire_messages::{Entry, IndexQuery, LsResponse},
    Hdp, SharedState,
};
use harddrive_party_shared::client::ClientError;
use std::collections::HashSet;
use tempfile::TempDir;

async fn setup_peer(share_dirs: Vec<String>) -> (SharedState, reqwest::Url) {
    let storage = TempDir::new().unwrap();
    let downloads = storage.path().to_path_buf();
    let mut hdp = Hdp::new(storage, share_dirs, downloads, true)
        .await
        .unwrap();

    let shared_state = hdp.shared_state.clone();

    tokio::spawn(async move {
        hdp.run().await;
    });

    let http_server_addr = http_server(shared_state.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let url = format!("http://{}", http_server_addr).parse().unwrap();
    (shared_state, url)
}

#[tokio::test]
async fn basic() {
    let (alice, alice_url) = setup_peer(vec!["tests/test-data".to_string()]).await;

    let (bob, bob_url) = setup_peer(vec![]).await;
    let mut bob_rx = bob.event_broadcaster.subscribe();

    // Wait until they connect to each other using mDNS
    while let Ok(event) = bob_rx.recv().await {
        if let UiEvent::PeerConnected { name } = event {
            if name == alice.name {
                break;
            }
        }
    }

    // Create clients
    let alice_client = Client::new(alice_url);
    let bob_client = Client::new(bob_url);

    // Do a share query on alice
    let mut response_stream = alice_client
        .shares(IndexQuery {
            recursive: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut response_entries = HashSet::new();
    while let Some(item) = response_stream.next().await {
        if let LsResponse::Success(entries) = item.unwrap() {
            for entry in entries {
                response_entries.insert(entry);
            }
        }
    }
    assert_eq!(response_entries, create_test_entries());

    // Bob does a files query on alice
    let query = FilesQuery {
        peer_name: None,
        query: IndexQuery {
            recursive: true,
            ..Default::default()
        },
    };
    let mut response_stream = bob_client.files(query).await.unwrap();

    let mut response_entries = HashSet::new();
    while let Some(item) = response_stream.next().await {
        if let (LsResponse::Success(entries), peer_name) = item.unwrap() {
            if peer_name == alice.name {
                for entry in entries {
                    response_entries.insert(entry);
                }
            }
        }
    }
    assert_eq!(response_entries, create_test_entries());

    // Download a file
    let request_id = bob_client
        .download(&PeerPath {
            path: "test-data/somefile".to_string(),
            peer_name: alice.name,
        })
        .await
        .unwrap();

    let mut bob_events = bob_client.event_stream().await.unwrap();
    // Check it dowloads - using the download events
    while let Some(event) = bob_events.next().await {
        if let Ok(UiEvent::Download(download_event)) = event {
            if let DownloadInfo::Completed(_) = download_event.download_info {
                break;
            }
        }
    }

    // Check we can query the request with its ID
    let mut requested_files = bob_client.requested_files(request_id).await.unwrap();
    let requested_file = requested_files.next().await.unwrap().unwrap();
    assert_eq!(requested_file[0].path, "test-data/somefile");

    // TODO download a directory
    // TODO read
    // TODO close connection
}

#[tokio::test]
async fn add_share_dir() {
    let (_alice, alice_url) = setup_peer(Vec::new()).await;

    // Create client
    let alice_client = Client::new(alice_url);

    let num_files_added = alice_client
        .add_share("tests/test-data".to_string())
        .await
        .unwrap();

    assert_eq!(num_files_added, 3);

    // Do a share query on alice
    let mut response_stream = alice_client
        .shares(IndexQuery {
            recursive: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut response_entries = HashSet::new();
    while let Some(item) = response_stream.next().await {
        if let LsResponse::Success(entries) = item.unwrap() {
            for entry in entries {
                response_entries.insert(entry);
            }
        }
    }
    assert_eq!(response_entries, create_test_entries());

    // Now remove it - note that we have to give the 'share name' and not the dir
    alice_client
        .remove_share("test-data".to_string())
        .await
        .unwrap();

    // Do a share query on alice
    let mut response_stream = alice_client
        .shares(IndexQuery {
            recursive: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut response_entries = HashSet::new();
    while let Some(item) = response_stream.next().await {
        if let LsResponse::Success(entries) = item.unwrap() {
            for entry in entries {
                response_entries.insert(entry);
            }
        }
    }

    assert_eq!(
        response_entries,
        HashSet::from([Entry {
            name: String::new(),
            size: 0,
            is_dir: true
        }])
    );

    // Try to remove it again - we should get an error
    assert_eq!(
        alice_client.remove_share("test-data".to_string()).await,
        Err(ClientError::ServerError(UiServerError::AddShare(
            "Share dir does not exist in DB".to_string()
        )))
    );
}

fn create_test_entries() -> HashSet<Entry> {
    HashSet::from([
        Entry {
            name: "".to_string(),
            size: 17,
            is_dir: true,
        },
        Entry {
            name: "test-data".to_string(),
            size: 17,
            is_dir: true,
        },
        Entry {
            name: "test-data/subdir".to_string(),
            size: 12,
            is_dir: true,
        },
        Entry {
            name: "test-data/subdir/subsubdir".to_string(),
            size: 6,
            is_dir: true,
        },
        Entry {
            name: "test-data/somefile".to_string(),
            size: 5,
            is_dir: false,
        },
        Entry {
            name: "test-data/subdir/anotherfile".to_string(),
            size: 6,
            is_dir: false,
        },
        Entry {
            name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
            size: 6,
            is_dir: false,
        },
    ])
}
