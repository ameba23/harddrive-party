use crate::{
    display_bytes,
    file::{DownloadStatus, DownloadingFile, File, FileDisplayContext},
    ui_messages::UiDownloadRequest,
    FilesSignal, PeerPath,
};
use leptos::{
    either::{Either, EitherOf3, EitherOf4},
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

/// A file which has been requested / downloaded
#[component]
pub fn Request(file: File) -> impl IntoView {
    let request_option = file.request.get();
    match request_option {
        Some(request) => {
            let files = use_context::<FilesSignal>().unwrap().0;
            let peer_path = PeerPath {
                peer_name: request.peer_name.clone(),
                path: request.path.clone(),
            };

            let child_files = move || {
                // Calling .get() clones - we should ideally use .with(|files| files.range...)
                let files = files.get();

                let mut upper_bound = peer_path.path.clone();
                upper_bound.push_str("~");
                files
                    .range(
                        peer_path.clone()..PeerPath {
                            peer_name: peer_path.peer_name.clone(),
                            path: upper_bound,
                        },
                    )
                    .map(|(_, file)| file.clone()) // TODO ideally dont clone
                    .collect::<Vec<File>>()
            };
            let icon = move || match file.is_dir {
                Some(true) => {
                    if file.is_expanded.get() {
                        EitherOf4::A(view! { <Icon icon=icondata::AiFolderOpenOutlined /> })
                    } else {
                        EitherOf4::B(view! { <Icon icon=icondata::AiFolderOutlined /> })
                    }
                }

                Some(false) => EitherOf4::C(view! { <Icon icon=icondata::AiFileOutlined /> }),
                None => EitherOf4::D(view! {}),
            };
            Either::Left(view! {
                <div>
                    {icon}{request.peer_name} " " {display_bytes(request.total_size)} " "
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
                                        </span>
                                    },
                                )
                            }
                            _ => EitherOf3::C(view! { <span></span> }),
                        }
                    }} <Table>
                        <TableBody>
                            <For
                                each=child_files
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
                </div>
            })
        }
        None => Either::Right(view! { <li>"Never happens"</li> }),
    }
}
