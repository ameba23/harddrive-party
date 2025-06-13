pub mod components;
pub mod file;
pub mod hdp;
pub mod peer;
mod requests;
pub mod shares;
pub mod transfers;
pub mod ws;

use crate::{file::File, peer::Peer, ui_messages::FilesQuery};
use futures::StreamExt;
use harddrive_party_shared::{client::Client, ui_messages::PeerPath, wire_messages::IndexQuery};
pub use hdp::*;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::Router;
use log::{debug, warn};
use requests::Requests;
use std::collections::{BTreeMap, HashSet};
use thaw::*;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <ConfigProvider>
            <Router>
                <HdpUi />
            </Router>
        </ConfigProvider>
    }
}

#[derive(Clone)]
pub struct AppContext {
    pub client: ReadSignal<Client>,
    pub own_name: ReadSignal<Option<String>>,
    pub set_peers: WriteSignal<HashSet<String>>,
}

impl AppContext {
    pub fn new(
        ui_url: url::Url,
        own_name: ReadSignal<Option<String>>,
        set_peers: WriteSignal<HashSet<String>>,
    ) -> Self {
        let (client, set_client) = signal(Client::new(ui_url));
        Self {
            client,
            own_name,
            set_peers,
        }
    }

    pub fn shares_query(
        &self,
        query: IndexQuery,
        own_name: Option<String>,
        set_files: WriteSignal<BTreeMap<PeerPath, File>>,
    ) {
        let client = self.client.get_untracked();
        spawn_local(async move {
            let mut shares_stream = client.shares(query).await.unwrap();
            debug!("Making shares query {own_name:?}");
            while let Some(response) = shares_stream.next().await {
                match response {
                    Ok(ls_response) => match ls_response {
                        LsResponse::Success(entries) => {
                            debug!("processing entries");
                            if let Some(ref own_name) = own_name {
                                set_files.update(|files| {
                                    for entry in entries {
                                        let peer_path = PeerPath {
                                            peer_name: own_name.clone(),
                                            path: entry.name.clone(),
                                        };
                                        if !files.contains_key(&peer_path) {
                                            files.insert(
                                                peer_path,
                                                File::from_entry(entry, own_name.clone()),
                                            );
                                        }
                                    }
                                });
                            }
                        }
                        LsResponse::Err(err) => {
                            warn!("Responded to shares request with err {:?}", err);
                        }
                    },
                    Err(e) => {
                        println!("Error from server {:?}", e);
                        break;
                    }
                }
            }
        });
    }

    pub fn download(&self, peer_path: PeerPath) {
        let client = self.client.get_untracked();
        spawn_local(async move {
            match client.download(&peer_path).await {
                Ok(id) => {
                    debug!("Download requested with id: {}", id);
                }
                Err(e) => {
                    warn!("Download request failed {:?}", e);
                }
            };
        });
    }

    pub fn connect(&self, announce_address: String) {
        let client = self.client.get_untracked();
        spawn_local(async move {
            match client.connect(announce_address).await {
                Ok(()) => {
                    debug!("Connecting to peer...");
                }
                Err(e) => {
                    warn!("Failed to connect to peer {:?}", e);
                }
            };
        });
    }

    pub fn files(&self, query: FilesQuery, set_files: WriteSignal<BTreeMap<PeerPath, File>>) {
        let client = self.client.get_untracked();
        let set_peers = self.set_peers.clone();
        spawn_local(async move {
            let mut files_stream = client.files(query).await.unwrap();

            while let Some(response) = files_stream.next().await {
                match response {
                    Ok((ls_response, peer_name)) => match ls_response {
                        LsResponse::Success(entries) => {
                            debug!("Processing entrys");

                            set_peers.update(|peers| {
                                peers.insert(peer_name.clone());
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
                        }
                    },
                    Err(server_error) => {
                        warn!("Got error from server {:?}", server_error);
                    }
                }
            }
        });
    }
    pub fn requests(&self, set_requests: WriteSignal<Requests>) {
        let client = self.client.get_untracked();
        debug!("Requests");
        spawn_local(async move {
            let mut requests_stream = client.requests().await.unwrap();
            while let Some(Ok(new_requests)) = requests_stream.next().await {
                set_requests.update(|requests| {
                    for request in new_requests.iter() {
                        requests.insert(request);
                    }
                });
            }
            //             let new_requests_clone = new_requests.clone();
            //             set_files.update(|files| {
            //                 for request in new_requests_clone {
            //                     let download_status = if request.progress == request.total_size
            //                     {
            //                         DownloadStatus::Downloaded(request.request_id)
            //                     } else {
            //                         DownloadStatus::Requested(request.request_id)
            //                     };
            //                     let peer_path = PeerPath {
            //                         peer_name: request.peer_name.clone(),
            //                         path: request.path.clone(),
            //                     };
            //                     files
            //                         .entry(peer_path.clone())
            //                         .and_modify(|file| {
            //                             file.request.set(Some(request.clone()));
            //                         })
            //                         .or_insert(File {
            //                             name: request.path.clone(),
            //                             peer_name: request.peer_name.clone(),
            //                             size: Some(request.total_size),
            //                             download_status: RwSignal::new(download_status.clone()),
            //                             request: RwSignal::new(Some(request.clone())),
            //                             is_dir: None,
            //                             is_expanded: RwSignal::new(true),
            //                             is_visible: RwSignal::new(true),
            //                         });
            //
            //                     let mut upper_bound = peer_path.path.clone();
            //                     upper_bound.push_str("~");
            //                     for (_, file) in files.range_mut(
            //                         peer_path.clone()..PeerPath {
            //                             peer_name: peer_path.peer_name.clone(),
            //                             path: upper_bound,
            //                         },
            //                     ) {
            //                         // TODO only set this to requested if...
            //                         file.download_status.set(download_status.clone());
            //                     }
            //                 }
            //             });
            //             for request in new_requests {
            //                 set_requester.update(|requester| {
            //                     requester
            //                         .make_request(Command::RequestedFiles(request.request_id))
            //                 });
            //             }
            //         }
        });
    }

    //         Ok(UiResponse::RequestedFiles(requested_files)) => {
    //             if let Some(Command::RequestedFiles(request_id)) = request {
    //                 // Now find the request and get peer_name
    //                 if let Some(peer_path) = requests.get().get_by_id(*request_id) {
    //                     set_files.update(|files| {
    //                         let is_dir_request = requested_files.len() > 0;
    //                         for requested_file in requested_files {
    //                             let download_status = if requested_file.downloaded {
    //                                 DownloadStatus::Downloaded(*request_id)
    //                             } else {
    //                                 DownloadStatus::Requested(*request_id)
    //                             };
    //                             files
    //                                 .entry(PeerPath {
    //                                     peer_name: peer_path.peer_name.clone(),
    //                                     path: requested_file.path.clone(),
    //                                 })
    //                                 .and_modify(|file| {
    //                                     // TODO this should not clobber if file is in
    //                                     // downloading state
    //                                     file.download_status
    //                                         .set(download_status.clone());
    //                                     file.size = Some(requested_file.size);
    //                                 })
    //                                 .or_insert(File {
    //                                     name: requested_file.path,
    //                                     peer_name: peer_path.peer_name.clone(),
    //                                     size: Some(requested_file.size),
    //                                     download_status: RwSignal::new(download_status),
    //                                     request: RwSignal::new(None),
    //                                     is_dir: Some(false),
    //                                     is_expanded: RwSignal::new(true),
    //                                     is_visible: RwSignal::new(true),
    //                                 });
    //                         }
    //                         // TODO here we should set the state of the parent request
    //                         // - if request_files > 1 (or 0?) is_dir = Some(true) else
    //                         // Some(false)
    //                         files.entry(peer_path.clone()).and_modify(|file| {
    //                             if file.is_dir.is_none() {
    //                                 file.is_dir = Some(is_dir_request);
    //                             }
    //                         });
    //                     });
    //                 }
    //             }
    //         }
    //     }
}

// pub struct UiClient {
//     pub inner: Client,
//     pub set_peers: WriteSignal<HashMap<String, Peer>>,
//     pub shares: ReadSignal<Option<Peer>>,
//     pub set_shares: WriteSignal<Option<Peer>>,
//     pub set_home_dir: WriteSignal<Option<String>>,
//     pub set_announce_address: WriteSignal<Option<String>>,
//     pub set_files: WriteSignal<BTreeMap<PeerPath, File>>,
//     // pending peers? // HashSet<String>
// }
//
