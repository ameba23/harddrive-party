pub mod components;
pub mod file;
pub mod hdp;
#[cfg(feature = "mock-ui")]
pub mod mock;
pub mod peer;
mod requests;
pub mod search;
pub mod shares;
pub mod transfers;
pub mod uploads;
pub mod ws;

use crate::{file::File, peer::Peer, ui_messages::FilesQuery};
use file::DownloadStatus;
use futures::{stream::LocalBoxStream, StreamExt};
use harddrive_party_shared::{
    client::Client,
    ui_messages::{PeerPath, UiRequestedFile, UiServerError},
    wire_messages::{AnnounceAddress, IndexQuery},
};
pub use hdp::*;
use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::Router;
use log::{debug, warn};
use requests::Requests;
use uploads::Uploads;
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

type SharesStream = LocalBoxStream<'static, Result<LsResponse, UiServerError>>;
type FilesStream = LocalBoxStream<'static, Result<(LsResponse, String), UiServerError>>;
type RequestsStream = LocalBoxStream<'static, Result<Vec<UiDownloadRequest>, UiServerError>>;
type RequestedFilesStream = LocalBoxStream<'static, Result<Vec<UiRequestedFile>, UiServerError>>;

#[derive(Clone)]
pub enum UiClient {
    Real(Client),
    #[cfg(feature = "mock-ui")]
    Mock(crate::mock::MockClient),
}

impl UiClient {
    pub fn real(ui_url: url::Url) -> Self {
        Self::Real(Client::new(ui_url))
    }

    pub async fn info(&self) -> Result<ui_messages::Info, AppError> {
        match self {
            UiClient::Real(client) => client.info().await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.info().await.map_err(AppError::from),
        }
    }

    pub async fn shares(&self, query: IndexQuery) -> Result<SharesStream, AppError> {
        match self {
            UiClient::Real(client) => Ok(client
                .shares(query)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => Ok(client
                .shares(query)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
        }
    }

    pub async fn files(&self, query: FilesQuery) -> Result<FilesStream, AppError> {
        match self {
            UiClient::Real(client) => Ok(client
                .files(query)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => Ok(client
                .files(query)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
        }
    }

    pub async fn requests(&self) -> Result<RequestsStream, AppError> {
        match self {
            UiClient::Real(client) => Ok(client
                .requests()
                .await
                .map_err(AppError::from)?
                .boxed_local()),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => Ok(client
                .requests()
                .await
                .map_err(AppError::from)?
                .boxed_local()),
        }
    }

    pub async fn requested_files(&self, id: u32) -> Result<RequestedFilesStream, AppError> {
        match self {
            UiClient::Real(client) => Ok(client
                .requested_files(id)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => Ok(client
                .requested_files(id)
                .await
                .map_err(AppError::from)?
                .boxed_local()),
        }
    }

    pub async fn download(&self, peer_path: &PeerPath) -> Result<u32, AppError> {
        match self {
            UiClient::Real(client) => client.download(peer_path).await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.download(peer_path).await.map_err(AppError::from),
        }
    }

    pub async fn connect(&self, announce_address: String) -> Result<(), AppError> {
        match self {
            UiClient::Real(client) => client.connect(announce_address).await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.connect(announce_address).await.map_err(AppError::from),
        }
    }

    pub async fn known_peers(&self) -> Result<Vec<AnnounceAddress>, AppError> {
        match self {
            UiClient::Real(client) => client.known_peers().await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.known_peers().await.map_err(AppError::from),
        }
    }

    pub async fn add_share(&self, share_dir: String) -> Result<u32, AppError> {
        match self {
            UiClient::Real(client) => client.add_share(share_dir).await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.add_share(share_dir).await.map_err(AppError::from),
        }
    }

    pub async fn remove_share(&self, share_dir: String) -> Result<(), AppError> {
        match self {
            UiClient::Real(client) => client.remove_share(share_dir).await.map_err(AppError::from),
            #[cfg(feature = "mock-ui")]
            UiClient::Mock(client) => client.remove_share(share_dir).await.map_err(AppError::from),
        }
    }
}

#[derive(Clone)]
pub struct AppContext {
    pub client: ReadSignal<UiClient>,
    pub own_name: ReadSignal<Option<String>>,
    pub get_peers: ReadSignal<HashSet<String>>,
    pub set_peers: WriteSignal<HashSet<String>>,
    pub get_files: ReadSignal<BTreeMap<PeerPath, File>>,
    pub set_files: WriteSignal<BTreeMap<PeerPath, File>>,
    pub set_requests: WriteSignal<Requests>,
    pub uploads: ReadSignal<Uploads>,
    pub set_uploads: WriteSignal<Uploads>,
    pub set_add_or_remove_share_message: WriteSignal<Option<Result<String, String>>>,
    pub set_error_message: WriteSignal<HashSet<AppError>>,
    pub set_search_results: WriteSignal<Vec<PeerPath>>,
    pub set_pending_peers: WriteSignal<HashSet<String>>,
    pub get_known_peers: ReadSignal<Vec<AnnounceAddress>>,
    pub set_known_peers: WriteSignal<Vec<AnnounceAddress>>,
}

impl AppContext {
    pub fn new(
        client: UiClient,
        own_name: ReadSignal<Option<String>>,
        get_peers: ReadSignal<HashSet<String>>,
        set_peers: WriteSignal<HashSet<String>>,
        get_files: ReadSignal<BTreeMap<PeerPath, File>>,
        set_files: WriteSignal<BTreeMap<PeerPath, File>>,
        set_requests: WriteSignal<Requests>,
        uploads: ReadSignal<Uploads>,
        set_uploads: WriteSignal<Uploads>,
        set_add_or_remove_share_message: WriteSignal<Option<Result<String, String>>>,
        set_error_message: WriteSignal<HashSet<AppError>>,
        set_search_results: WriteSignal<Vec<PeerPath>>,
        set_pending_peers: WriteSignal<HashSet<String>>,
        get_known_peers: ReadSignal<Vec<AnnounceAddress>>,
        set_known_peers: WriteSignal<Vec<AnnounceAddress>>,
    ) -> Self {
        let (client, _set_client) = signal(client);
        Self {
            client,
            own_name,
            get_peers,
            set_peers,
            get_files,
            set_files,
            set_requests,
            uploads,
            set_uploads,
            set_add_or_remove_share_message,
            set_error_message,
            set_search_results,
            set_pending_peers,
            get_known_peers,
            set_known_peers,
        }
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        let (own_name, _) = signal(Some("mock-ui".to_string()));
        let (get_peers, set_peers) = signal(HashSet::<String>::new());
        let (get_files, set_files) = signal(BTreeMap::<PeerPath, File>::new());
        let (_requests, set_requests) = signal(Requests::new());
        let (uploads, set_uploads) = signal(Uploads::new());
        let (_share_message, set_share_message) = signal(None::<Result<String, String>>);
        let (_errors, set_errors) = signal(HashSet::new());
        let (_search_results, set_search_results) = signal(Vec::<PeerPath>::new());
        let (_pending_peers, set_pending_peers) = signal(HashSet::<String>::new());
        let (known_peers, set_known_peers) = signal(Vec::<AnnounceAddress>::new());

        Self::new(
            UiClient::real("http://127.0.0.1:3030".parse().expect("url should parse")),
            own_name,
            get_peers,
            set_peers,
            get_files,
            set_files,
            set_requests,
            uploads,
            set_uploads,
            set_share_message,
            set_errors,
            set_search_results,
            set_pending_peers,
            known_peers,
            set_known_peers,
        )
    }

    pub fn shares_query(&self, query: IndexQuery) {
        let client = self.client.get_untracked();
        let set_files = self.set_files.clone();
        let own_name = self.own_name.get_untracked();
        let set_error_message = self.set_error_message.clone();
        spawn_local(async move {
            match client.shares(query).await {
                Ok(mut shares_stream) => {
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
                                    } else {
                                        debug!("No name");
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
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }

    pub fn download(&self, peer_path: PeerPath) {
        let client = self.client.get_untracked();
        let set_requests = self.set_requests.clone();
        let set_files = self.set_files.clone();
        let files = self.get_files.clone();
        let set_error_message = self.set_error_message.clone();
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
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            };
        });
    }

    pub fn connect(&self, announce_address: String) {
        let client = self.client.get_untracked();
        let set_error_message = self.set_error_message.clone();
        let set_pending_peers = self.set_pending_peers.clone();
        spawn_local(async move {
            match client.connect(announce_address.clone()).await {
                Ok(()) => {
                    debug!("Connecting to peer...");
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                    set_pending_peers.update(|pending_peers| {
                        pending_peers.remove(&announce_address);
                    });
                }),
            };
        });
    }

    pub fn files(&self, query: FilesQuery) {
        let client = self.client.get_untracked();
        let set_peers = self.set_peers.clone();
        let set_files = self.set_files.clone();
        let set_error_message = self.set_error_message.clone();
        spawn_local(async move {
            match client.files(query).await {
                Ok(mut files_stream) => {
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
                                                .or_insert(File::from_entry(
                                                    entry,
                                                    peer_name.clone(),
                                                ));
                                        }
                                    });
                                }
                                LsResponse::Err(err) => {
                                    warn!("Peer responded to ls request with err {:?}", err);
                                }
                            },
                            Err(err) => set_error_message.update(|error_messages| {
                                error_messages.insert(err.into());
                            }),
                        }
                    }
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }

    pub fn requests(&self, set_requests: WriteSignal<Requests>) {
        let client = self.client.get_untracked();
        let set_files = self.set_files.clone();
        let self_clone = self.clone();
        let set_error_message = self.set_error_message.clone();
        debug!("Requests");
        spawn_local(async move {
            match client.requests().await {
                Ok(mut requests_stream) => {
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
                        }
                    }
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }

    pub fn requested_files(&self, request: UiDownloadRequest) {
        let client = self.client.get_untracked();
        let set_files = self.set_files.clone();
        let set_error_message = self.set_error_message.clone();
        spawn_local(async move {
            match client.requested_files(request.request_id).await {
                Ok(mut stream) => {
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
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }

    pub fn add_share(&self, share_dir: String) {
        let client = self.client.get_untracked();
        let self_clone = self.clone();
        let set_error_message = self.set_error_message.clone();
        spawn_local(async move {
            match client.add_share(share_dir).await {
                Ok(num_files_added) => {
                    self_clone
                        .set_add_or_remove_share_message
                        .update(|message| {
                            *message = Some(Ok(format!("Added {} files", num_files_added)))
                        });

                    // Re-query shares to reflect changes
                    self_clone.shares_query(IndexQuery {
                        path: Default::default(),
                        searchterm: None,
                        recursive: false,
                    });
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }

    pub fn remove_share(&self, share_dir: String) {
        let client = self.client.get_untracked();
        let self_clone = self.clone();
        spawn_local(async move {
            match client.remove_share(share_dir).await {
                Ok(()) => {
                    self_clone
                        .set_add_or_remove_share_message
                        .update(|message| *message = Some(Ok("No longer sharing".to_string())));

                    // Re-query shares to reflect changes
                    self_clone.shares_query(IndexQuery {
                        path: Default::default(),
                        searchterm: None,
                        recursive: false,
                    });
                }
                Err(err) => self_clone.set_error_message.update(|error_messages| {
                    error_messages.insert(err.into());
                }),
            }
        });
    }

    pub fn search(&self, searchterm: String) {
        let query = FilesQuery {
            query: IndexQuery {
                path: Default::default(),
                searchterm: Some(searchterm),
                recursive: true,
            },
            peer_name: None,
        };
        let client = self.client.get_untracked();
        let set_search_results = self.set_search_results.clone();
        let set_files = self.set_files.clone();
        let set_error_message = self.set_error_message.clone();
        spawn_local(async move {
            match client.files(query).await {
                Ok(mut files_stream) => {
                    // Remove existing search results
                    set_search_results.update(|search_results| {
                        *search_results = Vec::new();
                    });
                    while let Some(response) = files_stream.next().await {
                        match response {
                            Ok((ls_response, peer_name)) => match ls_response {
                                LsResponse::Success(entries) => {
                                    set_search_results.update(|search_results| {
                                        for entry in entries.clone() {
                                            let peer_path = PeerPath {
                                                path: entry.name.clone(),
                                                peer_name: peer_name.clone(),
                                            };
                                            search_results.push(peer_path);
                                        }
                                    });

                                    set_files.update(|files| {
                                        for entry in entries {
                                            let peer_path = PeerPath {
                                                path: entry.name.clone(),
                                                peer_name: peer_name.clone(),
                                            };
                                            files
                                                .entry(peer_path)
                                                .and_modify(|file| {
                                                    file.size = Some(entry.size);
                                                    file.is_dir = Some(entry.is_dir);
                                                })
                                                .or_insert(File::from_entry(
                                                    entry,
                                                    peer_name.clone(),
                                                ));
                                        }
                                    });
                                }
                                LsResponse::Err(err) => {
                                    warn!("Peer responded to ls request with err {:?}", err);
                                }
                            },
                            Err(err) => set_error_message.update(|error_messages| {
                                error_messages.insert(err.into());
                            }),
                        }
                    }
                }
                Err(err) => set_error_message.update(|error_messages| {
                    error_messages.insert(err);
                }),
            }
        });
    }
}
