use futures::StreamExt;
use harddrive_party::{
    client::Client,
    hdp::{Hdp, SharedState},
    http::http_server,
    ui_messages::{FilesQuery, UiEvent, UiResponse},
    wire_messages::{Entry, IndexQuery, LsResponse},
};
use std::{collections::HashSet, net::SocketAddr};
use tempfile::TempDir;

async fn setup_peer(share_dirs: Vec<String>) -> (SharedState, SocketAddr) {
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

    (shared_state, http_server_addr)
}

#[tokio::test]
async fn basic() {
    let (alice, alice_socket_address) = setup_peer(vec!["tests/test-data".to_string()]).await;

    let (bob, bob_socket_address) = setup_peer(vec![]).await;
    let mut bob_rx = bob.event_broadcaster.subscribe();

    // Wait until they connect to each other using mDNS
    while let Ok(event) = bob_rx.recv().await {
        if let UiEvent::PeerConnected { name, peer_type: _ } = event {
            if name == alice.name {
                break;
            }
        }
    }

    // Create clients
    let alice_client = Client::new(alice_socket_address);
    let bob_client = Client::new(bob_socket_address);

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
        if let UiResponse::Shares(LsResponse::Success(entries)) = item.unwrap() {
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
        if let UiResponse::Ls(LsResponse::Success(entries), peer_name) = item.unwrap() {
            if peer_name == alice.name {
                for entry in entries {
                    response_entries.insert(entry);
                }
            }
        }
    }
    assert_eq!(response_entries, create_test_entries());

    // TODO read
    // TODO download - and check requests state
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
