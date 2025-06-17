pub mod components;
pub mod file;
pub mod hdp;
pub mod peer;
mod requests;
pub mod shares;
pub mod transfers;
pub mod ws;

use crate::{file::File, peer::Peer, ui_messages::FilesQuery};
use file::DownloadStatus;
use futures::StreamExt;
use harddrive_party_shared::{client::Client, ui_messages::PeerPath, wire_messages::IndexQuery};
pub use hdp::*;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::Router;
use log::{debug, warn};
use requests::Requests;
use std::collections::{BTreeMap, HashSet};
use thaw::*;
use ui_messages::UiDownloadRequest;

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
    pub get_files: ReadSignal<BTreeMap<PeerPath, File>>,
    pub set_files: WriteSignal<BTreeMap<PeerPath, File>>,
    pub set_requests: WriteSignal<Requests>,
}

impl AppContext {
    pub fn new(
        ui_url: url::Url,
        own_name: ReadSignal<Option<String>>,
        set_peers: WriteSignal<HashSet<String>>,
        get_files: ReadSignal<BTreeMap<PeerPath, File>>,
        set_files: WriteSignal<BTreeMap<PeerPath, File>>,
        set_requests: WriteSignal<Requests>,
    ) -> Self {
        let (client, _set_client) = signal(Client::new(ui_url));
        Self {
            client,
            own_name,
            set_peers,
            get_files,
            set_files,
            set_requests,
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
        let set_requests = self.set_requests.clone();
        let set_files = self.set_files.clone();
        let files = self.get_files.clone();
        spawn_local(async move {
            match client.download(&peer_path).await {
                Ok(id) => {
                    debug!("Download requested with id: {}", id);
                    let total_size = files
                        .get()
                        .get(&peer_path)
                        .map_or(0, |file| file.size.unwrap_or_default());
                    let request = UiDownloadRequest {
                        path: peer_path.path.clone(),
                        peer_name: peer_path.peer_name.clone(),
                        progress: 0,
                        total_size,
                        request_id: id,
                        timestamp: std::time::Duration::from_secs(0), // TODO
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
                                download_status: RwSignal::new(DownloadStatus::Requested(id)),
                                request: RwSignal::new(Some(request.clone())),
                                is_dir: None,
                                is_expanded: RwSignal::new(true),
                                is_visible: RwSignal::new(true),
                            });
                        // Mark all files below this one in the dir heirarchy as
                        // requested
                        let mut upper_bound = peer_path.path.clone();
                        upper_bound.push_str("~");
                        for (_, file) in files.range_mut(
                            peer_path.clone()..PeerPath {
                                peer_name: peer_path.peer_name.clone(),
                                path: upper_bound,
                            },
                        ) {
                            file.download_status.set(DownloadStatus::Requested(id));
                        }
                    })
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

    pub fn files(&self, query: FilesQuery) {
        let client = self.client.get_untracked();
        let set_peers = self.set_peers.clone();
        let set_files = self.set_files.clone();
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
        let set_files = self.set_files.clone();
        let self_clone = self.clone();
        debug!("Requests");
        spawn_local(async move {
            let mut requests_stream = client.requests().await.unwrap();
            while let Some(Ok(new_requests)) = requests_stream.next().await {
                set_requests.update(|requests| {
                    for request in new_requests.iter() {
                        requests.insert(request);
                    }
                });
                let new_requests_clone = new_requests.clone();
                set_files.update(|files| {
                    for request in new_requests_clone {
                        let download_status = if request.progress == request.total_size {
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
                                is_dir: None, // We don't know whether it is a dir or a file
                                is_expanded: RwSignal::new(true),
                                is_visible: RwSignal::new(true),
                            });

                        // Now set all child files to the same download status
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

                // For each request, get the requested files
                for request in new_requests {
                    self_clone.requested_files(request);
                    // set_requester.update(|requester| {
                    //     requester.make_request(Command::RequestedFiles(request.request_id))
                    // });
                }
            }
        });
    }

    pub fn requested_files(&self, request: UiDownloadRequest) {
        let client = self.client.get_untracked();
        let set_files = self.set_files.clone();
        spawn_local(async move {
            let mut stream = client.requested_files(request.request_id).await.unwrap();
            while let Some(Ok(requested_files)) = stream.next().await {
                set_files.update(|files| {
                    let is_dir_request = requested_files.len() > 0;
                    for requested_file in requested_files {
                        let download_status = if requested_file.downloaded {
                            DownloadStatus::Downloaded(request.request_id)
                        } else {
                            DownloadStatus::Requested(request.request_id)
                        };
                        files
                            .entry(PeerPath {
                                peer_name: request.peer_name.clone(),
                                path: requested_file.path.clone(),
                            })
                            .and_modify(|file| {
                                // TODO this should not clobber if file is in
                                // downloading state
                                file.download_status.set(download_status.clone());
                                file.size = Some(requested_file.size);
                            })
                            .or_insert(File {
                                name: requested_file.path,
                                peer_name: request.peer_name.clone(),
                                size: Some(requested_file.size),
                                download_status: RwSignal::new(download_status),
                                request: RwSignal::new(None),
                                is_dir: Some(false),
                                is_expanded: RwSignal::new(true),
                                is_visible: RwSignal::new(true),
                            });
                    }
                    // TODO here we should set the state of the parent request
                    // - if request_files > 1 (or 0?) is_dir = Some(true) else
                    // Some(false)
                    let peer_path = PeerPath {
                        peer_name: request.peer_name.clone(),
                        path: request.path.clone(),
                    };
                    files.entry(peer_path).and_modify(|file| {
                        if file.is_dir.is_none() {
                            file.is_dir = Some(is_dir_request);
                        }
                    });
                });
            }
        });
    }
    //
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
}
