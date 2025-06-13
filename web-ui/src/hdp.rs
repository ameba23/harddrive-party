pub use harddrive_party_shared::ui_messages;
use harddrive_party_shared::ui_messages::PeerPath;
pub use harddrive_party_shared::wire_messages;

use crate::{
    components::header::HdpHeader,
    file::{DownloadStatus, File},
    peer::Peers,
    requests::Requests,
    shares::Shares,
    transfers::Transfers,
    ui_messages::{DownloadInfo, FilesQuery, UiDownloadRequest, UiEvent, UiServerError},
    wire_messages::IndexQuery,
    ws::WebsocketService,
    AppContext,
};
use futures::StreamExt;
use harddrive_party_shared::client::Client;
use leptos::prelude::*;
use leptos_router::{
    components::{Redirect, Route, Routes},
    path,
};
use log::{debug, info};
use pretty_bytes_rust::pretty_bytes;
use std::collections::{BTreeMap, HashSet};
use thaw::*;
use wasm_bindgen_futures::spawn_local;
pub use wire_messages::{Entry, LsResponse};

#[derive(Clone)]
pub struct FilesSignal(
    pub ReadSignal<BTreeMap<PeerPath, File>>,
    pub WriteSignal<BTreeMap<PeerPath, File>>,
);

#[component]
pub fn HdpUi() -> impl IntoView {
    // Use document.location as hostname for api server server to connect to - unless 'dev' feature
    let ui_url: url::Url = if cfg!(feature = "dev") {
        "http://127.0.0.1:3030".parse().unwrap()
    } else {
        let origin = match document().location() {
            Some(loc) => loc.origin().unwrap(),
            None => "http://127.0.0.1:3030".to_string(),
        };
        origin.parse().unwrap()
    };
    // let (ui_url, _) = signal(ui_url);

    let (error_message, set_error_message) = signal(HashSet::<AppError>::new());

    let (_ws_service, mut ws_rx) =
        WebsocketService::new(ui_url.clone(), set_error_message).unwrap();

    // Setup signals
    let (peers, set_peers) = signal(HashSet::<String>::new());
    let (pending_peers, set_pending_peers) = signal(HashSet::<String>::new());
    let (add_or_remove_share_message, set_add_or_remove_share_message) =
        signal(Option::<Result<String, String>>::None);

    // let (own_name, set_own_name) = signal(Option::<String>::None);
    let (requests, set_requests) = signal(Requests::new());

    let (files, set_files) = signal(BTreeMap::<PeerPath, File>::new());

    let (home_dir, set_home_dir) = signal(Option::<String>::None);
    let (announce_address, set_announce_address) = signal(Option::<String>::None);
    let (own_name, set_own_name) = signal(Option::<String>::None);
    let app_context = AppContext::new(ui_url, own_name, set_peers.clone());

    // Get initial info
    let client = app_context.client.get_untracked();
    spawn_local(async move {
        let set_announce_address = set_announce_address.clone();
        let set_home_dir = set_home_dir.clone();
        let set_own_name = set_own_name.clone();
        let info = client.info().await.unwrap();
        set_announce_address.update(|address| *address = Some(info.announce_address));
        set_home_dir.update(|home_dir| *home_dir = info.os_home_dir);
        set_own_name.update(|own_name| *own_name = Some(info.name.clone()));
    });
    {
        let app_context = app_context.clone();
        Effect::new(move || {
            let index_query = IndexQuery {
                path: Default::default(),
                searchterm: None,
                recursive: false,
            };
            app_context.shares_query(index_query.clone(), own_name.get(), set_files);

            // On startup do a files query to see what peers are connected
            app_context.files(
                FilesQuery {
                    query: index_query,
                    peer_name: None,
                },
                set_files,
            );
        });
    }
    app_context.requests(set_requests);

    provide_context(app_context.clone());
    provide_context(FilesSignal(files, set_files));

    spawn_local(async move {
        // Loop over messages from server
        while let Some(msg) = ws_rx.next().await {
            match msg {
                //         Ok(UiResponse::Download(download_response)) => {
                //             debug!("Got download response {:?}", download_response);
                //             // TODO check if we already have the associated request
                //             match download_response.download_info {
                //                 DownloadInfo::Requested(timestamp) => {
                //                     let peer_path = PeerPath {
                //                         peer_name: download_response.peer_name.clone(),
                //                         path: download_response.path.clone(),
                //                     };
                //                     let total_size = files
                //                         .get()
                //                         .get(&peer_path)
                //                         .map_or(0, |file| file.size.unwrap_or_default());
                //                     let request = UiDownloadRequest {
                //                         path: download_response.path.clone(),
                //                         peer_name: download_response.peer_name.clone(),
                //                         progress: 0,
                //                         total_size,
                //                         request_id: id,
                //                         timestamp,
                //                     };
                //                     set_requests.update(|requests| {
                //                         if requests.get_by_id(id).is_none() {
                //                             requests.insert(&request);
                //                         }
                //                     });
                //                     set_files.update(|files| {
                //                         files
                //                             .entry(peer_path.clone())
                //                             .and_modify(|file| {
                //                                 file.request.set(Some(request.clone()));
                //                             })
                //                             .or_insert(File {
                //                                 name: request.path.clone(),
                //                                 peer_name: request.peer_name.clone(),
                //                                 size: None,
                //                                 download_status: RwSignal::new(
                //                                     DownloadStatus::Requested(id),
                //                                 ),
                //                                 request: RwSignal::new(Some(request.clone())),
                //                                 is_dir: None,
                //                                 is_expanded: RwSignal::new(true),
                //                                 is_visible: RwSignal::new(true),
                //                             });
                //                         // Mark all files below this one in the dir heirarchy as
                //                         // requested
                //                         let mut upper_bound = download_response.path.clone();
                //                         upper_bound.push_str("~");
                //                         for (_, file) in files.range_mut(
                //                             PeerPath {
                //                                 peer_name: download_response.peer_name.clone(),
                //                                 path: download_response.path.clone(),
                //                             }
                //                                 ..PeerPath {
                //                                     peer_name: download_response.peer_name.clone(),
                //                                     path: upper_bound,
                //                                 },
                //                         ) {
                //                             file.download_status.set(DownloadStatus::Requested(id));
                //                         }
                //                     })
                //                 }
                //                 DownloadInfo::Downloading {
                //                     path,
                //                     bytes_read,
                //                     total_bytes_read: _,
                //                     speed: _,
                //                 } => {
                //                     set_files.update(|files| {
                //                         files
                //                             .entry(PeerPath {
                //                                 peer_name: download_response.peer_name.clone(),
                //                                 path: path.clone(),
                //                             })
                //                             .and_modify(|file| {
                //                                 let download_status = if bytes_read
                //                                     == file.size.unwrap_or_default()
                //                                 {
                //                                     DownloadStatus::Downloaded(id)
                //                                 } else {
                //                                     DownloadStatus::Downloading {
                //                                         bytes_read,
                //                                         request_id: id,
                //                                     }
                //                                 };
                //                                 file.download_status.set(download_status);
                //                             })
                //                             .or_insert(File::from_downloading_file(
                //                                 path,
                //                                 download_response.peer_name.clone(),
                //                                 DownloadStatus::Downloading {
                //                                     bytes_read,
                //                                     request_id: id,
                //                                 },
                //                             ));
                //                     });
                //
                //                     set_requests.update(|_requests| {
                //                         // TODO Find request with this request id
                //                         // update the total_bytes_read
                //                     });
                //                 }
                //                 DownloadInfo::Completed(_timestamp) => {
                //                     // TODO Mark all files below this one in the dir heirarchy as
                //                     // completed
                //                     // TODO update requests to have progress = total_size
                //                     set_files.update(|files| {
                //                         files
                //                             .entry(PeerPath {
                //                                 peer_name: download_response.peer_name.clone(),
                //                                 path: download_response.path.clone(),
                //                             })
                //                             .and_modify(|file| {
                //                                 file.download_status
                //                                     .set(DownloadStatus::Downloaded(id));
                //                             });
                //                         // TODO do we need or_insert?
                //                     })
                //                 }
                //             }
                //         }
                //         Ok(UiResponse::EndResponse) => {
                //             if let Some(Command::ConnectDirect(announce_address)) = request {
                //             }
                //         }
                //         Ok(UiResponse::AddShare(number_of_shares)) => {
                //             debug!("Got add share response");
                //             set_add_or_remove_share_message.update(|message| {
                //                 *message = Some(Ok(format!("Added {} shares", number_of_shares)))
                //             });
                //
                //             // Re-query shares to reflect changes
                //             let share_query_request = Command::Shares(IndexQuery {
                //                 path: Default::default(),
                //                 searchterm: None,
                //                 recursive: true,
                //             });
                //             set_requester
                //                 .update(|requester| requester.make_request(share_query_request));
                //         }
                //         Ok(UiResponse::RemoveShare) => {
                //             debug!("Got remove share response");
                //             set_add_or_remove_share_message.update(|message| {
                //                 *message = Some(Ok("No longer sharing".to_string()))
                //             });
                //
                //             // Re-query shares to reflect changes
                //             let share_query_request = Command::Shares(IndexQuery {
                //                 path: Default::default(),
                //                 searchterm: None,
                //                 recursive: true,
                //             });
                //             set_requester
                //                 .update(|requester| requester.make_request(share_query_request));
                //         }
                // }
                UiEvent::PeerConnected { name } => {
                    debug!("Connected to {}", name);
                    set_peers.update(|peers| {
                        peers.insert(name.clone());
                    });
                    app_context.files(
                        FilesQuery {
                            query: IndexQuery {
                                path: Default::default(),
                                searchterm: None,
                                recursive: false,
                            },
                            peer_name: Some(name),
                        },
                        set_files,
                    )
                    //TODO remove from pending_peers
                    // set_pending_peers.update(|pending_peers| {
                    //     pending_peers.remove(announce_address);
                    // })
                }
                UiEvent::PeerDisconnected { name } => {
                    debug!("{} disconnected", name);
                    set_peers.update(|peers| {
                        peers.remove(&name);
                    });
                }
                UiEvent::Uploaded(upload_info) => {
                    debug!("Uploading {:?}", upload_info);
                }
                UiEvent::PeerConnectionFailed { name, error } => {
                    debug!("Peer connection failed {} {}", name, error);
                    // set_pending_peers.update(|pending_peers| {
                    //     pending_peers.remove(announce_address);
                    // })
                    //
                    set_peers.update(|peers| {
                        peers.remove(&name);
                    });
                }
                UiEvent::Download(_download_info) => {}
            }
        }
        debug!("ws closed");
    });

    // let client = client.read();
    // // On startup GET /info
    // let info = client.info();
    //
    // // On startup do a shares query to get our own files
    // client.shares(index_query);

    let error_message_display = move || {
        view! {
            <For
                each=move || error_message.get()
                key=|error_message| format!("{:?}", error_message)
                children=move |error_message| {
                    match error_message {
                        AppError::WsConnection => {
                            view! {
                                <ErrorMessage message=format!("{}", error_message)>
                                    <span />
                                </ErrorMessage>
                            }
                        }
                        _ => {
                            view! {
                                <ErrorMessage message=format!("{}", error_message)>

                                    <MessageBarActions>
                                        <Button
                                            appearance=ButtonAppearance::Transparent
                                            icon=icondata::AiCloseOutlined
                                            on:click=move |_| {
                                                set_error_message
                                                    .update(|error_messages| {
                                                        error_messages.remove(&error_message);
                                                    })
                                            }
                                        />
                                    </MessageBarActions>
                                </ErrorMessage>
                            }
                        }
                    }
                }
            />
        }
    };

    view! {
        <Layout>
            <div id="root" class="main">
                <nav>
                    <HdpHeader peers own_name />
                    {error_message_display}
                </nav>
                <main>
                    <Layout>
                        <Routes fallback=|| "Not found">
                            <Route
                                path=path!("")
                                view=move || {
                                    view! { <Redirect path="/peers" /> }
                                }
                            />
                            <Route
                                path=path!("shares")
                                view=move || {
                                    view! { <Shares add_or_remove_share_message home_dir /> }
                                }
                            />

                            <Route
                                path=path!("peers")
                                view=move || {
                                    view! {
                                        <Peers
                                            peers
                                            announce_address
                                            pending_peers
                                            set_pending_peers
                                        />
                                    }
                                }
                            />
                            <Route
                                path=path!("transfers")
                                view=move || view! { <Transfers requests files /> }
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
pub fn ErrorMessage(message: String, children: Children) -> impl IntoView {
    view! {
        <MessageBar intent=MessageBarIntent::Error>
            <MessageBarBody>
                <MessageBarTitle>"Error"</MessageBarTitle>
                {message}
            </MessageBarBody>
            {children()}
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
    PeerConnection(String, String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AppError::WsConnection => {
                write!(
                    f,
                    "Cannot connect to harddrive-party over websocket. Is harddrive party runnng?"
                )
            }
            AppError::PeerConnection(announce_address, message) => {
                write!(f, "Cannot connect to peer {announce_address}: {message}")
            }
        }
    }
}
