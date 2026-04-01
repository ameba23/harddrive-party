use crate::{
    file::{DownloadStatus, DownloadingFile, File, FileDisplayContext},
    ui_messages::UiDownloadRequest,
    AppContext, PeerPath,
};
use leptos::{
    either::{Either, EitherOf3},
    prelude::*,
};
use std::collections::BTreeMap;
use thaw::*;

/// For requests (requested or downloaded items)
/// Map timestamp, request id to peer name and path
#[derive(Clone)]
pub struct Requests(BTreeMap<(u64, u32), PeerPath>);

impl Requests {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn insert(&mut self, request: &UiDownloadRequest) -> Option<PeerPath> {
        let peer_path = PeerPath {
            peer_name: request.peer_name.clone(),
            path: request.path.clone(),
        };
        // To make them be ordered newest first, invert the timestamp
        self.0.insert(
            (u64::MAX - request.timestamp.as_secs(), request.request_id),
            peer_path,
        )
    }

    pub fn get_by_id(&self, id: u32) -> Option<&PeerPath> {
        self.0.iter().find(|(k, _v)| k.1 == id).map(|(_k, v)| v)
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, (u64, u32), PeerPath> {
        self.0.iter()
    }
}

fn request_files(files: &BTreeMap<PeerPath, File>, peer_path: &PeerPath) -> Vec<File> {
    let mut upper_bound = peer_path.path.clone();
    upper_bound.push_str("~");
    let mut request_files = Vec::new();
    if let Some(file) = files.get(peer_path) {
        request_files.push(file.clone());
    }
    request_files.extend(
        files
            .range(
                peer_path.clone()..PeerPath {
                    peer_name: peer_path.peer_name.clone(),
                    path: upper_bound,
                },
            )
            .filter(|(_, file)| file.name != peer_path.path)
            .map(|(_, file)| file.clone()),
    );
    request_files
}

#[component]
fn RequestFilesTable(peer_path: PeerPath) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let get_files = app_context.get_files;
    view! {
        <Table class="transfer-table">
            <TableBody>
                <For
                    each=move || request_files(&get_files.get(), &peer_path)
                    key=|file| format!("{}{:?}", file.name, file.size)
                    children=move |file: File| {
                        view! {
                            <File
                                file
                                is_shared=false
                                context=FileDisplayContext::Transfer
                            />
                        }
                    }
                />
            </TableBody>
        </Table>
    }
}

/// A file which has been requested / downloaded
#[component]
pub fn Request(file: File) -> impl IntoView {
    let request_option = file.request.get_untracked();
    match request_option {
        Some(request) => {
            let request_peer_name = request.peer_name.clone();
            let peer_path = PeerPath {
                peer_name: request.peer_name.clone(),
                path: request.path.clone(),
            };

            view! {
                <div class=move || {
                    if matches!(file.download_status.get(), DownloadStatus::Downloaded(_)) {
                        "transfer-request-group transfer-request-group--complete"
                    } else {
                        "transfer-request-group"
                    }
                }>
                    <div class="transfer-request-status">
                        <span class="file-peer-label" title=request_peer_name.clone()>
                            {request_peer_name.clone()}
                        </span>
                        {move || {
                            if file.is_dir == Some(true) {
                                Either::Left(view! {
                                    <span class="transfer-request-status-detail">
                                        {move || {
                                            match file.download_status.get() {
                                                DownloadStatus::Downloading { bytes_read, request_id: _ } => {
                                                    EitherOf3::A(
                                                        view! {
                                                            <span>
                                                                <DownloadingFile bytes_read size=file.size />
                                                            </span>
                                                        },
                                                    )
                                                }
                                                DownloadStatus::Downloaded(_) => {
                                                    EitherOf3::B(
                                                        view! {
                                                            <span title="Download complete">
                                                                <Icon icon=icondata::AiCheckCircleTwotone />
                                                                " Downloaded"
                                                            </span>
                                                        },
                                                    )
                                                }
                                                _ => EitherOf3::C(view! { <span></span> }),
                                            }
                                        }}
                                    </span>
                                })
                            } else {
                                Either::Right(view! { <span></span> })
                            }
                        }}
                    </div>
                    <div class="table-scroll transfer-request-files">
                        <RequestFilesTable peer_path=peer_path.clone() />
                    </div>
                </div>
            }
                .into_any()
        }
        None => view! { <li>"Never happens"</li> }.into_any(),
    }
}
