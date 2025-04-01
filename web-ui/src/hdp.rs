pub use harddrive_party_shared::ui_messages;
use harddrive_party_shared::ui_messages::{PeerRemoteOrSelf, UiServerError};
pub use harddrive_party_shared::wire_messages;

use crate::{
    components::header::HdpHeader,
    file::{DownloadStatus, File},
    peer::{Peer, Peers},
    requests::Requests,
    shares::Shares,
    topics::Topics,
    transfers::Transfers,
    ui_messages::{Command, DownloadInfo, UiEvent},
    wire_messages::IndexQuery,
    ws::{Requester, WebsocketService},
};
use futures::StreamExt;
use leptos::prelude::*;
use leptos_meta::Style;
use leptos_router::{
    components::{Route, Routes},
    hooks::use_navigate,
    path,
};
use log::{debug, info, warn};
use pretty_bytes_rust::pretty_bytes;
use std::collections::{BTreeMap, HashMap, HashSet};
use thaw::*;
use ui_messages::{UiDownloadRequest, UiResponse, UiServerMessage, UiTopic};
use wasm_bindgen_futures::spawn_local;
pub use wire_messages::{Entry, LsResponse};

#[derive(Copy, Clone)]
pub struct RequesterSetter(pub WriteSignal<Requester>);

#[derive(Clone)]
pub struct FilesReadSignal(pub ReadSignal<BTreeMap<PeerPath, File>>);

/// Represents a remote file
#[derive(Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerPath {
    /// The name of the peer who holds the file
    pub peer_name: String,
    /// The path to the remote file
    pub path: String,
}

#[component]
pub fn HdpUi() -> impl IntoView {
    // Use document.location as hostname for ws server to connect to
    let location = match document().location() {
        Some(loc) => loc.hostname().unwrap(),
        None => "No location".to_string(),
    };
    info!("location {}", location);

    let (error_message, set_error_message) = signal(HashSet::<AppError>::new());

    let ws_url = format!("ws://{}:4001", location);
    let (ws_service, mut ws_rx) = WebsocketService::new(&ws_url, set_error_message).unwrap();

    // Setup signals
    let (requester, set_requester) = signal(Requester::new(ws_service));
    let (peers, set_peers) = signal(HashMap::<String, Peer>::new());
    let (shares, set_shares) = signal(Option::<Peer>::None);
    let (add_or_remove_share_message, set_add_or_remove_share_message) =
        signal(Option::<Result<String, String>>::None);
    let (topics, set_topics) = signal(Vec::<UiTopic>::new());

    let (requests, set_requests) = signal(Requests::new());

    let (files, set_files) = signal(BTreeMap::<PeerPath, File>::new());
    let (home_dir, set_home_dir) = signal(Option::<String>::None);

    provide_context(RequesterSetter(set_requester));
    // provide_context(Requested(requested));
    provide_context(FilesReadSignal(files));

    spawn_local(async move {
        let remove_request = |id: &u32| {
            set_requester.update(|requester| {
                requester.remove_request(id);
            });
        };

        // Loop over messages from server
        while let Some(msg) = ws_rx.next().await {
            match msg {
                UiServerMessage::Response { id, response } => {
                    // Get the associated request
                    let r = requester.get_untracked();
                    let request = { r.get_request(&id) };

                    match response {
                        Ok(UiResponse::Ls(ls_response, peer_name)) => match ls_response {
                            LsResponse::Success(entries) => {
                                debug!("processing entrys");
                                let entries_clone = entries.clone();
                                set_peers.update(|peers| {
                                    let peer = peers
                                        .entry(peer_name.clone())
                                        .or_insert(Peer::new(peer_name.clone(), false));
                                    for entry in entries_clone {
                                        peer.files.insert(
                                            entry.name.clone(),
                                            // File::new(entry),
                                        );
                                    }
                                    debug!("peers {:?}", peers);
                                });
                                set_files.update(|files| {
                                    for entry in entries {
                                        let peer_path = PeerPath {
                                            peer_name: peer_name.clone(),
                                            path: entry.name.clone(),
                                        };
                                        files
                                            .entry(peer_path)
                                            .and_modify(|file| {
                                                file.size = Some(entry.size);
                                                file.is_dir = Some(entry.is_dir);
                                            })
                                            .or_insert(File::from_entry(entry, peer_name.clone()));
                                    }
                                });
                            }
                            LsResponse::Err(err) => {
                                warn!("Peer responded to ls request with err {:?}", err);
                                remove_request(&id);
                            }
                        },
                        Ok(UiResponse::Read(read_response)) => {
                            // This is not currently used but could be used for previewing a
                            // portion of a file without downloading it
                            debug!("Got read response {:?}", read_response);
                        }
                        Ok(UiResponse::Download(download_response)) => {
                            debug!("Got download response {:?}", download_response);
                            // TODO check if we already have the associated request
                            match download_response.download_info {
                                DownloadInfo::Requested(timestamp) => {
                                    let peer_path = PeerPath {
                                        peer_name: download_response.peer_name.clone(),
                                        path: download_response.path.clone(),
                                    };
                                    let total_size = files
                                        .get()
                                        .get(&peer_path)
                                        .map_or(0, |file| file.size.unwrap_or_default());
                                    let request = UiDownloadRequest {
                                        path: download_response.path.clone(),
                                        peer_name: download_response.peer_name.clone(),
                                        progress: 0,
                                        total_size,
                                        request_id: id,
                                        timestamp,
                                    };
                                    set_requests.update(|requests| {
                                        if requests.get_by_id(id).is_none() {
                                            requests.insert(&request);
                                        }
                                    });
                                    set_files.update(|files| {
                                        files
                                            .entry(peer_path.clone())
                                            .and_modify(|file| {
                                                file.request.set(Some(request.clone()));
                                            })
                                            .or_insert(File {
                                                name: request.path.clone(),
                                                peer_name: request.peer_name.clone(),
                                                size: None,
                                                download_status: RwSignal::new(
                                                    DownloadStatus::Requested(id),
                                                ),
                                                request: RwSignal::new(Some(request.clone())),
                                                is_dir: None,
                                            });
                                        // Mark all files below this one in the dir heirarchy as
                                        // requested
                                        let mut upper_bound = download_response.path.clone();
                                        upper_bound.push_str("~");
                                        for (_, file) in files.range_mut(
                                            PeerPath {
                                                peer_name: download_response.peer_name.clone(),
                                                path: download_response.path.clone(),
                                            }
                                                ..PeerPath {
                                                    peer_name: download_response.peer_name.clone(),
                                                    path: upper_bound,
                                                },
                                        ) {
                                            file.download_status.set(DownloadStatus::Requested(id));
                                        }
                                    })
                                }
                                DownloadInfo::Downloading {
                                    path,
                                    bytes_read,
                                    total_bytes_read,
                                    speed: _,
                                } => {
                                    set_files.update(|files| {
                                        files
                                            .entry(PeerPath {
                                                peer_name: download_response.peer_name.clone(),
                                                path: path.clone(),
                                            })
                                            .and_modify(|file| {
                                                let download_status = if bytes_read
                                                    == file.size.unwrap_or_default()
                                                {
                                                    DownloadStatus::Downloaded(id)
                                                } else {
                                                    DownloadStatus::Downloading {
                                                        bytes_read,
                                                        request_id: id,
                                                    }
                                                };
                                                file.download_status.set(download_status);
                                            })
                                            .or_insert(File::from_downloading_file(
                                                path,
                                                download_response.peer_name.clone(),
                                                DownloadStatus::Downloading {
                                                    bytes_read,
                                                    request_id: id,
                                                },
                                            ));
                                    });

                                    set_requests.update(|requests| {
                                        // TODO Find request with this request id
                                        // update the total_bytes_read
                                    });
                                }
                                DownloadInfo::Completed(timestamp) => {
                                    // TODO Mark all files below this one in the dir heirarchy as
                                    // completed
                                    // TODO update requests to have progress = total_size
                                    set_files.update(|files| {
                                        files
                                            .entry(PeerPath {
                                                peer_name: download_response.peer_name.clone(),
                                                path: download_response.path.clone(),
                                            })
                                            .and_modify(|file| {
                                                file.download_status
                                                    .set(DownloadStatus::Downloaded(id));
                                            });
                                        // TODO do we need or_insert?
                                    })
                                }
                            }
                        }
                        Ok(UiResponse::EndResponse) => {
                            remove_request(&id);
                        }
                        Ok(UiResponse::Shares(ls_response)) => {
                            debug!("Got shares response");
                            match ls_response {
                                LsResponse::Success(entries) => {
                                    debug!("processing entrys");
                                    let entries_clone = entries.clone();
                                    set_shares.update(|shares_option| {
                                        if let Some(shares) = shares_option {
                                            for entry in entries_clone {
                                                shares.files.insert(
                                                    entry.name.clone(),
                                                    // File::new(entry),
                                                );
                                            }
                                        }
                                    });
                                    if let Some(peer) = shares.get_untracked() {
                                        let peer_name = peer.name.clone();
                                        set_files.update(|files| {
                                            for entry in entries {
                                                let peer_path = PeerPath {
                                                    peer_name: peer_name.clone(),
                                                    path: entry.name.clone(),
                                                };
                                                if !files.contains_key(&peer_path) {
                                                    files.insert(
                                                        peer_path,
                                                        File::from_entry(entry, peer_name.clone()),
                                                    );
                                                }
                                            }
                                        });
                                    }
                                }
                                LsResponse::Err(err) => {
                                    warn!("Responded to shares request with err {:?}", err);
                                    remove_request(&id);
                                }
                            }
                        }
                        Ok(UiResponse::AddShare(number_of_shares)) => {
                            debug!("Got add share response");
                            set_add_or_remove_share_message.update(|message| {
                                *message = Some(Ok(format!("Added {} shares", number_of_shares)))
                            });

                            // Re-query shares to reflect changes
                            let share_query_request = Command::Shares(IndexQuery {
                                path: Default::default(),
                                searchterm: None,
                                recursive: true,
                            });
                            set_requester
                                .update(|requester| requester.make_request(share_query_request));
                        }
                        Ok(UiResponse::RemoveShare) => {
                            debug!("Got remove share response");
                            set_add_or_remove_share_message.update(|message| {
                                *message = Some(Ok("No longer sharing".to_string()))
                            });

                            // Re-query shares to reflect changes
                            let share_query_request = Command::Shares(IndexQuery {
                                path: Default::default(),
                                searchterm: None,
                                recursive: true,
                            });
                            set_requester
                                .update(|requester| requester.make_request(share_query_request));
                        }
                        Ok(UiResponse::Requests(new_requests)) => {
                            set_requests.update(|requests| {
                                for request in new_requests.iter() {
                                    requests.insert(&request);
                                }
                            });
                            let new_requests_clone = new_requests.clone();
                            set_files.update(|files| {
                                for request in new_requests_clone {
                                    let download_status = if request.progress == request.total_size
                                    {
                                        DownloadStatus::Downloaded(request.request_id)
                                    } else {
                                        DownloadStatus::Requested(request.request_id)
                                    };
                                    let peer_path = PeerPath {
                                        peer_name: request.peer_name.clone(),
                                        path: request.path.clone(),
                                    };
                                    files
                                        .entry(peer_path.clone())
                                        .and_modify(|file| {
                                            file.request.set(Some(request.clone()));
                                        })
                                        .or_insert(File {
                                            name: request.path.clone(),
                                            peer_name: request.peer_name.clone(),
                                            size: Some(request.total_size),
                                            download_status: RwSignal::new(download_status.clone()),
                                            request: RwSignal::new(Some(request.clone())),
                                            is_dir: None,
                                        });

                                    let mut upper_bound = peer_path.path.clone();
                                    upper_bound.push_str("~");
                                    for (_, file) in files.range_mut(
                                        peer_path.clone()..PeerPath {
                                            peer_name: peer_path.peer_name.clone(),
                                            path: upper_bound,
                                        },
                                    ) {
                                        // TODO only set this to requested if...
                                        file.download_status.set(download_status.clone());
                                    }
                                }
                            });
                            for request in new_requests {
                                set_requester.update(|requester| {
                                    requester
                                        .make_request(Command::RequestedFiles(request.request_id))
                                });
                            }
                        }
                        Ok(UiResponse::RequestedFiles(requested_files)) => {
                            if let Some(Command::RequestedFiles(request_id)) = request {
                                // Now find the request and get peer_name
                                if let Some(peer_path) = requests.get().get_by_id(*request_id) {
                                    set_files.update(|files| {
                                        for requested_file in requested_files {
                                            let download_status = if requested_file.downloaded {
                                                DownloadStatus::Downloaded(*request_id)
                                            } else {
                                                DownloadStatus::Requested(*request_id)
                                            };
                                            files
                                                .entry(PeerPath {
                                                    peer_name: peer_path.peer_name.clone(),
                                                    path: requested_file.path.clone(),
                                                })
                                                .and_modify(|file| {
                                                    // TODO this should not clobber if file is in
                                                    // downloading state
                                                    file.download_status
                                                        .set(download_status.clone());
                                                    file.size = Some(requested_file.size);
                                                })
                                                .or_insert(File {
                                                    name: requested_file.path,
                                                    peer_name: peer_path.peer_name.clone(),
                                                    size: Some(requested_file.size),
                                                    download_status: RwSignal::new(download_status),
                                                    request: RwSignal::new(None),
                                                    is_dir: Some(false),
                                                });
                                        }
                                    });
                                }
                            }
                        }
                        Err(server_error) => {
                            warn!("Got error from server {:?}", server_error);
                            remove_request(&id);
                            match server_error {
                                UiServerError::ShareError(error_message) => {
                                    set_add_or_remove_share_message
                                        .update(|message| *message = Some(Err(error_message)));
                                }
                                _ => {
                                    warn!("Not handling error");
                                }
                            }
                        }
                    }
                }
                UiServerMessage::Event(UiEvent::PeerConnected { name, peer_type }) => {
                    // If a new peer connects check their files
                    // or check our own files
                    debug!("Connected to {}", name);
                    let request = if let PeerRemoteOrSelf::Me { os_home_dir } = peer_type {
                        set_shares.update(|shares| match shares {
                            Some(_) => {}
                            None => *shares = Some(Peer::new(name, true)),
                        });
                        set_home_dir.update(|home_dir| *home_dir = os_home_dir);
                        Command::Shares(IndexQuery {
                            path: Default::default(),
                            searchterm: None,
                            recursive: true,
                        })
                    } else {
                        Command::Ls(
                            IndexQuery {
                                path: Default::default(),
                                searchterm: None,
                                recursive: true,
                            },
                            Default::default(),
                        )
                    };
                    set_requester.update(|requester| requester.make_request(request));
                }
                UiServerMessage::Event(UiEvent::PeerDisconnected { name }) => {
                    debug!("{} disconnected", name);
                    set_peers.update(|peers| {
                        peers.remove(&name);
                    });
                }
                UiServerMessage::Event(UiEvent::Uploaded(upload_info)) => {
                    debug!("Uploading {:?}", upload_info);
                }
                UiServerMessage::Event(UiEvent::Topics(topics)) => {
                    debug!("Connected topics {:?}", topics);
                    set_topics.update(|existing_topics| {
                        existing_topics.clear();
                        for topic in topics {
                            existing_topics.push(topic);
                        }
                    });
                } // UiServerMessage::Event(UiEvent::Wishlist {
                  //     requested,
                  //     downloaded,
                  // }) => {
                  //     debug!("Requested {:?} Downloaded {:?}", requested, downloaded);
                  //     let requested_clone = requested.clone();
                  //     let downloaded_clone = downloaded.clone();
                  //     set_files.update(|files| {
                  //         for request in requested_clone {
                  //             files
                  //                 .entry(PeerPath {
                  //                     peer_name: request.peer_name.clone(),
                  //                     path: request.path.clone(),
                  //                 })
                  //                 .and_modify(|file| {
                  //                     file.download_status.set(DownloadStatus::Requested);
                  //                     file.request.set(Some(request.clone()));
                  //                 })
                  //                 .or_insert(File::from_download_request(
                  //                     request,
                  //                     DownloadStatus::Requested,
                  //                 ));
                  //         }
                  //
                  //         for request in downloaded_clone {
                  //             files
                  //                 .entry(PeerPath {
                  //                     peer_name: request.peer_name.clone(),
                  //                     path: request.path.clone(),
                  //                 })
                  //                 .and_modify(|file| {
                  //                     file.download_status.set(DownloadStatus::Downloaded);
                  //                     file.request.set(Some(request.clone()));
                  //                 })
                  //                 .or_insert(File::from_download_request(
                  //                     request,
                  //                     DownloadStatus::Downloaded,
                  //                 ));
                  //         }
                  //     });
                  //     set_requested.update(|existing_requested| {
                  //         existing_requested.clear();
                  //         for request in requested {
                  //             existing_requested.insert(PeerPath {
                  //                 peer_name: request.peer_name.clone(),
                  //                 path: request.path.clone(),
                  //             });
                  //         }
                  //     });
                  //     set_downloaded.update(|existing_downloaded| {
                  //         existing_downloaded.clear();
                  //         for request in downloaded {
                  //             existing_downloaded.insert(PeerPath {
                  //                 peer_name: request.peer_name.clone(),
                  //                 path: request.path.clone(),
                  //             });
                  //         }
                  //     });
                  // }
            }
        }
        debug!("ws closed");
    });

    // Get current requests
    set_requester.update(|requester| requester.make_request(Command::Requests));

    let error_message_display = move || {
        view! {
            <For
                each=move || error_message.get()
                key=|error_message| format!("{:?}", error_message)
                children=move |error_message| {
                    view! { <ErrorMessage message=format!("{}", error_message)/> }
                }
            />
        }
    };
    let selected_value = RwSignal::new("peers".to_string());

    view! {
        <Style id="hdp-header">
            "
                .hdp-header {
                    height: 64px;
                    display: flex;
                    align-items: center;
                    justify-content: space-between;
                    padding: 0 20px;
                    z-index: 1000;
                    position: relative;
                }
                .hdp-name {
                    cursor: pointer;
                    display: flex;
                    align-items: center;
                    height: 100%;
                    font-weight: 600;
                    font-size: 20px;
                }
                .hdp-header__menu-mobile {
                    display: none !important;
                }
                .hdp-header__right-btn .thaw-select {
                    width: 60px;
                }
                @media screen and (max-width: 600px) {
                    .hdp-header {
                        padding: 0 8px;
                    }
                    .hdp-name {
                        display: none;
                    }
                }
                @media screen and (max-width: 1200px) {
                    .hdp-header__right-btn {
                        display: none !important;
                    }
                    .hdp-header__menu-mobile {
                        display: inline-block !important;
                    }
                }
            "
        </Style>
        <Layout>
            <div id="root" class="main">
                <nav>
                    <HdpHeader topics peers shares/>
                    {error_message_display}
                </nav>
                <main>
                    <Layout>
                        <Routes fallback=|| "Not found">
                            <Route path=path!("") view=move || view! { <Peers peers/> }/>
                            <Route path=path!("topics") view=move || view! { <Topics topics/> }/>
                            <Route
                                path=path!("shares")
                                view=move || {
                                    view! { <Shares shares add_or_remove_share_message home_dir/> }
                                }
                            />

                            <Route path=path!("peers") view=move || view! { <Peers peers/> }/>
                            <Route
                                path=path!("transfers")
                                view=move || view! { <Transfers requests files/> }
                            />
                        </Routes>
                    </Layout>
                </main>
            </div>
        </Layout>
    }
}

pub fn display_bytes(bytes: u64) -> String {
    match bytes {
        0 => "0".to_string(),
        _ => pretty_bytes(
            bytes,
            Some(pretty_bytes_rust::PrettyBytesOptions {
                use_1024_instead_of_1000: Some(true),
                number_of_decimal: None,
                remove_zero_decimal: Some(true),
            }),
        ),
    }
}

#[component]
pub fn ErrorMessage(message: String) -> impl IntoView {
    view! {
        <MessageBar intent=MessageBarIntent::Error>
            <MessageBarBody>
                <MessageBarTitle>"Error"</MessageBarTitle>
                {message}
            </MessageBarBody>
        </MessageBar>
    }
}

#[component]
pub fn SuccessMessage(message: String) -> impl IntoView {
    view! {
        <div
            class="flex p-4 my-4 text-sm text-green-800 border border-green-300 rounded-lg bg-green-50 dark:bg-gray-800 dark:text-green-400 dark:border-green-800"
            role="alert"
        >
            <div>
                <span class="font-medium">" ✅ " {message}</span>
            </div>
        </div>
    }
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub enum AppError {
    WsConnection,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let err_msg = match self {
            AppError::WsConnection => {
                "Cannot connect to harddrive-party over websocket. Is harddrive party runnng?"
            }
        };

        write!(f, "{}", err_msg)
    }
}
