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

fn request_child_files(files: &BTreeMap<PeerPath, File>, peer_path: &PeerPath) -> Vec<File> {
    let mut upper_bound = peer_path.path.clone();
    upper_bound.push_str("~");
    files
        .range(
            peer_path.clone()..PeerPath {
                peer_name: peer_path.peer_name.clone(),
                path: upper_bound,
            },
        )
        .filter(|(_, file)| file.name != peer_path.path)
        .map(|(_, file)| file.clone())
        .collect::<Vec<File>>()
}

#[component]
fn RequestFilesTable(peer_path: PeerPath) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let get_files = app_context.get_files;
    view! {
        <Table class="transfer-table">
            <TableBody>
                <For
                    each=move || request_child_files(&get_files.get(), &peer_path)
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
    let app_context = use_context::<AppContext>().unwrap();
    let request_option = file.request.get_untracked();
    match request_option {
        Some(request) => {
            let request_path = request.path.clone();
            let request_peer_name = request.peer_name.clone();
            let peer_path = PeerPath {
                peer_name: request.peer_name.clone(),
                path: request.path.clone(),
            };
            let child_files_peer_path = peer_path.clone();
            let get_files_for_snapshot = app_context.get_files;

            let child_files =
                move || request_child_files(&get_files_for_snapshot.get(), &child_files_peer_path);

            if file.is_dir != Some(true) {
                return view! {
                    <Table class="transfer-table">
                        <TableBody>
                            <File file is_shared=false context=FileDisplayContext::Transfer />
                        </TableBody>
                    </Table>
                }
                    .into_any();
            }

            view! {
                {move || {
                    let child_files_snapshot = child_files();
                    let show_single_child = child_files_snapshot.len() == 1
                        && child_files_snapshot[0].is_dir != Some(true)
                        && !matches!(file.download_status.get(), DownloadStatus::Downloaded(_));
                    if show_single_child {
                        Either::Left(view! {
                            <RequestFilesTable peer_path=peer_path.clone() />
                        })
                    } else {
                        Either::Right(view! {
                            <div class="transfer-request-group">
                                <div class="transfer-request-status">
                                    <span class="file-peer-label" title=request_peer_name.clone()>
                                        {request_peer_name.clone()}
                                        " "
                                    </span>
                                    <span class="transfer-request-path text-sm font-medium" title=request_path.clone()>
                                        {request_path.clone()}
                                    </span>
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
                                </div>
                                <div class="table-scroll transfer-request-files">
                                    <RequestFilesTable peer_path=peer_path.clone() />
                                </div>
                            </div>
                        })
                    }
                }}
            }
                .into_any()
        }
        None => view! { <li>"Never happens"</li> }.into_any(),
    }
}
