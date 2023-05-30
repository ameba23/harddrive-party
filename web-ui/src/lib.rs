pub mod file;
pub mod peer;
pub mod topics;
pub mod transfers;
pub mod ui_messages;
pub mod wire_messages;
pub mod ws;

use crate::{
    file::{DownloadStatus, File},
    peer::{Peer, Peers},
    topics::Topics,
    transfers::Transfers,
    ui_messages::{Command, UiEvent},
    wire_messages::IndexQuery,
    ws::{Requester, WebsocketService},
};
use futures::StreamExt;
use leptos::*;
use leptos_router::*;
use log::{debug, info, warn};
use pretty_bytes_rust::pretty_bytes;
use std::collections::{HashMap, HashSet};
use ui_messages::{DownloadResponse, UiResponse, UiServerMessage};
use wasm_bindgen_futures::spawn_local;
use wire_messages::{Entry, LsResponse};

const ITEM_STYLE: &str = "inline-block p-4 border-b-2 border-transparent rounded-t-lg hover:text-gray-600 hover:border-gray-300 dark:hover:text-gray-300 focus:text-blue-600 focus:border-blue-600";
const NUMBER_LABEL: &str = "inline-flex items-center justify-center px-1 ml-2 text-xs font-semibold text-gray-800 bg-gray-200 rounded-full";
pub const BUTTON_STYLE: &str = "bg-black hover:bg-gray-700 p-1 text-white rounded";

#[derive(Copy, Clone)]
struct RequesterSetter(WriteSignal<Requester>);

#[derive(Clone)]
struct PeerName(ReadSignal<(String, bool)>);

#[derive(Clone)]
struct Requested(ReadSignal<HashSet<PeerPath>>);

#[derive(Clone)]
struct FilesReadSignal(ReadSignal<HashMap<PeerPath, File>>);

#[derive(Clone, Hash, PartialEq, Eq)]
pub struct PeerPath {
    peer_name: String,
    path: String,
}

#[component]
pub fn HdpUi(cx: Scope) -> impl IntoView {
    let location = match document().location() {
        Some(loc) => loc.hostname().unwrap(),
        None => "No location".to_string(),
    };
    info!("location {}", location);

    let ws_url = format!("ws://{}:4001", location);
    let (ws_service, mut ws_rx) = WebsocketService::new(&ws_url);
    let (requester, set_requester) = create_signal(cx, Requester::new(ws_service));
    let (peers, set_peers) = create_signal(cx, HashMap::<String, Peer>::new());
    let (shares, set_shares) = create_signal(cx, Option::<Peer>::None);
    let (topics, set_topics) = create_signal(cx, Vec::<String>::new());
    let (requested, set_requested) = create_signal(cx, HashSet::<PeerPath>::new());
    let (downloaded, set_downloaded) = create_signal(cx, HashSet::<PeerPath>::new());
    let (files, set_files) = create_signal(cx, HashMap::<PeerPath, File>::new());

    provide_context(cx, RequesterSetter(set_requester));
    provide_context(cx, Requested(requested));
    provide_context(cx, FilesReadSignal(files));

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
                    match requester.get_untracked().get_request(&id) {
                        Some(request) => {
                            // if this is an endresponse or error, remove the associated request
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
                                                    // File::new(cx, entry),
                                                );
                                            }
                                            debug!("peers {:?}", peers);
                                        });
                                        set_files.update(|files| {
                                            for entry in entries {
                                                files.insert(
                                                    PeerPath {
                                                        peer_name: peer_name.clone(),
                                                        path: entry.name.clone(),
                                                    },
                                                    File::from_entry(cx, entry),
                                                );
                                            }
                                        });
                                    }
                                    LsResponse::Err(err) => {
                                        warn!("Peer responded to ls request with err {:?}", err);
                                        remove_request(&id);
                                    }
                                },
                                Ok(UiResponse::Read(read_response)) => {
                                    debug!("Got read response {:?}", read_response);
                                    // path: String,
                                    // bytes_read: u64,
                                    // total_bytes_read: u64,
                                    // speed: usize,
                                }
                                Ok(UiResponse::Download(download_response)) => {
                                    debug!("Got download response {:?}", download_response);
                                    if let Command::Download { peer_name, path } = request {
                                        let files = files.get();
                                        if let Some(file) = files.get(&PeerPath {
                                            peer_name: peer_name.clone(),
                                            path: path.clone(),
                                        }) {
                                            if download_response.bytes_read == file.size {
                                                file.download_status
                                                    .set(DownloadStatus::Downloaded);
                                            } else {
                                                file.download_status.set(
                                                    DownloadStatus::Downloading(download_response),
                                                );
                                            }
                                        };
                                    }
                                }
                                Ok(UiResponse::Connect) => {
                                    debug!("Successfully connected to peer");
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
                                                            // File::new(cx, entry),
                                                        );
                                                    }
                                                }
                                            });
                                            if let Some(peer) = shares.get() {
                                                let peer_name = peer.name.clone();
                                                set_files.update(|files| {
                                                    for entry in entries {
                                                        files.insert(
                                                            PeerPath {
                                                                peer_name: peer_name.clone(),
                                                                path: entry.name.clone(),
                                                            },
                                                            File::from_entry(cx, entry),
                                                        );
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
                                Err(server_error) => {
                                    warn!("Got error from server {:?}", server_error);
                                    remove_request(&id);
                                }
                            }
                        }
                        None => {
                            warn!("Found response with unknown ID - probably from another client");
                        }
                    }
                }
                UiServerMessage::Event(UiEvent::PeerConnected { name, is_self }) => {
                    // If a new peer connects check their files
                    // or check our own files
                    debug!("Connected to {} {}", name, is_self);
                    let request = if is_self {
                        set_shares.update(|shares| match shares {
                            Some(_) => {}
                            None => *shares = Some(Peer::new(name, true)),
                        });
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
                }
                UiServerMessage::Event(UiEvent::Uploaded(upload_info)) => {
                    debug!("Uploading {:?}", upload_info);
                }
                UiServerMessage::Event(UiEvent::ConnectedTopics(topics)) => {
                    debug!("Connected topics {:?}", topics);
                    set_topics.update(|existing_topics| {
                        existing_topics.clear();
                        for topic in topics {
                            existing_topics.push(topic);
                        }
                    });
                }
                UiServerMessage::Event(UiEvent::Wishlist {
                    requested,
                    downloaded,
                }) => {
                    debug!("Requested {:?} Downloaded {:?}", requested, downloaded);
                    let requested_clone = requested.clone();
                    let downloaded_clone = downloaded.clone();
                    set_files.update(|files| {
                        for request in requested_clone {
                            files
                                .entry(PeerPath {
                                    peer_name: request.peer_name.clone(),
                                    path: request.path.clone(),
                                })
                                .and_modify(|file| {
                                    file.download_status.set(DownloadStatus::Requested);
                                    file.request.set(Some(request.clone()));
                                })
                                .or_insert(File::from_download_request(cx, request));
                        }

                        for request in downloaded_clone {
                            files
                                .entry(PeerPath {
                                    peer_name: request.peer_name.clone(),
                                    path: request.path.clone(),
                                })
                                .and_modify(|file| {
                                    file.download_status.set(DownloadStatus::Downloaded);
                                    file.request.set(Some(request.clone()));
                                })
                                .or_insert(File::from_download_request(cx, request));
                            // TODO
                        }
                    });
                    set_requested.update(|existing_requested| {
                        existing_requested.clear();
                        for request in requested {
                            existing_requested.insert(PeerPath {
                                peer_name: request.peer_name.clone(),
                                path: request.path.clone(),
                            });
                        }
                    });
                    set_downloaded.update(|existing_downloaded| {
                        existing_downloaded.clear();
                        for request in downloaded {
                            existing_downloaded.insert(PeerPath {
                                peer_name: request.peer_name.clone(),
                                path: request.path.clone(),
                            });
                        }
                    });
                }
            }
        }
        debug!("ws closed");
    });

    let shared_files_size = move || match shares.get() {
        Some(me) => {
            match files.get().get(&PeerPath {
                peer_name: me.name,
                path: "".to_string(),
            }) {
                Some(file) => display_bytes(file.size),
                None => display_bytes(0),
            }
        }
        None => display_bytes(0),
    };

    view! { cx,
        <div id="root" class="container mx-auto font-serif">
            <Router>
                <nav>
                    <div class="text-sm font-medium text-center text-gray-500 border-b border-gray-200 dark:text-gray-400 dark:border-gray-700">
                        <ul class="flex flex-wrap -mb-px">
                            <li class="mr-2">
                                <img class="hover:invert" src="hdd.png" alt="hard drive" width="60" title="harddrive-party"/>
                            </li>
                            <li class="mr-2" title={ move || { format!("{} connected topics", topics.get().len()) } }>
                                <A href="topics" class={ ITEM_STYLE }>
                                    { "Topics" }
                                    <span class={ NUMBER_LABEL}>
                                        {move || { topics.get().len() } }
                                    </span>
                                </A>
                            </li>
                            <li class="mr-2">
                                <A href="shares" class={ ITEM_STYLE }>
                                    "Shares"
                                    <span class={ NUMBER_LABEL }>
                                        { shared_files_size }
                                    </span>
                                </A>
                            </li>
                            <li class="mr-2" title={ move || { format!("{} connected peers", peers.get().len() ) } }>
                                <A href="peers" class={ ITEM_STYLE }>
                                    "Peers"
                                    <span class={ NUMBER_LABEL }>
                                        {move || { peers.get().len() } }
                                    </span>
                                </A>
                            </li>
                            <li class="mr-2">
                                <A href="transfers" class={ ITEM_STYLE}>
                                    {"Transfers"}
                                </A>
                            </li>
                        </ul>
                    </div>
                </nav>
                <main>
                    <Routes>
                        <Route
                            path=""
                            view=move |cx| view! { cx,  <Peers peers /> }
                        />
                        <Route
                            path="topics"
                            view=move |cx| view! { cx,  <Topics topics /> }
                        />
                        <Route
                            path="shares"
                            view=move |cx| view! { cx,  <Shares shares /> }
                        />
                        <Route
                            path="peers"
                            view=move |cx| view! { cx,  <Peers peers /> }
                        />
                        <Route
                            path="transfers"
                            view=move |cx| view! { cx,  <Transfers requested downloaded files /> }
                        />
                    </Routes>
                </main>
            </Router>
        </div>
    }
}

#[component]
fn Shares(cx: Scope, shares: ReadSignal<Option<Peer>>) -> impl IntoView {
    let selves = move || match shares.get() {
        Some(shares) => vec![shares],
        None => Vec::new(),
    };

    // TODO A form offerring to share another directory

    view! { cx,
            <h2 class="text-xl">"Shared files"</h2>
            <ul class="list-disc list-inside">
                <For
                    each=selves
                    key=|peer| format!("{}{}", peer.name, peer.files.len())
                    view=move |cx, peer| view! { cx,  <Peer peer /> }
                />
            </ul>
    }
}

fn display_bytes(bytes: u64) -> String {
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
