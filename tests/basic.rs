use futures::StreamExt;
use harddrive_party::{
    ui_messages::{DownloadInfo, FilesQuery, PeerPath, UiEvent},
    ui_server::{client::Client, http_server},
    wire_messages::{Entry, IndexQuery, LsResponse},
    Hdp, SharedState,
};
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

//
//     // Do a read request
//     let req = ReadQuery {
//         path: "test-data/somefile".to_string(),
//         start: None,
//         end: None,
//     };
//     bob_command_tx
//         .send(UiClientMessage {
//             id: 2,
//             command: Command::Read(req, alice_name.clone()),
//         })
//         .await
//         .unwrap();
//
//     while let Some(res) = bob_rx.recv().await {
//         if let UiServerMessage::Response { id: _, response } = res {
//             assert_eq!(Ok(UiResponse::Read(b"boop\n".to_vec())), response);
//             break;
//         }
//     }
//
//     // // Do an Ls query
//     let req = IndexQuery {
//         path: None,
//         searchterm: None,
//         recursive: true,
//     };
//     bob_command_tx
//         .send(UiClientMessage {
//             id: 3,
//             command: Command::Ls(req, Some(alice_name)),
//         })
//         .await
//         .unwrap();
//
//     let mut entries = Vec::new();
//     while let Some(res) = bob_rx.recv().await {
//         if let UiServerMessage::Response { id, response } = res {
//             match response.unwrap() {
//                 UiResponse::Ls(LsResponse::Success(some_entries), _name) => {
//                     for entry in some_entries {
//                         entries.push(entry);
//                     }
//                 }
//                 UiResponse::EndResponse => {
//                     if id == 3 {
//                         break;
//                     }
//                 }
//                 _ => {}
//             }
//         }
//     }
//     let test_entries = create_test_entries();
//     assert_eq!(test_entries, entries);
//
//     // Close the connection
//     alice_command_tx
//         .send(UiClientMessage {
//             id: 3,
//             command: Command::Close,
//         })
//         .await
//         .unwrap();
