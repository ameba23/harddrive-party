//! Display a file - either from a remote peer or one of our own shared files
use crate::{
    hdp::{display_bytes, Entry},
    ui_messages::{FilesQuery, UiDownloadRequest, UploadInfo},
    AppContext, PeerPath,
};
use harddrive_party_shared::wire_messages::IndexQuery;
use leptos::{
    either::{Either, EitherOf4, EitherOf6},
    prelude::*,
};
use log::debug;
use std::ops::Bound::Excluded;
use thaw::*;

/// Ui representation of a file
#[derive(Clone, Debug)]
pub struct File {
    /// Path of file
    pub name: String,
    /// Name of peer who holds this file
    pub peer_name: String,
    /// Size, if known
    pub size: Option<u64>,
    pub is_dir: Option<bool>,
    pub is_expanded: RwSignal<bool>,
    /// Is this item in an expanded directory - only relevant to peers context
    pub is_visible: RwSignal<bool>,
    pub download_status: RwSignal<DownloadStatus>,
    pub request: RwSignal<Option<UiDownloadRequest>>,
}

impl File {
    pub fn from_entry(entry: Entry, peer_name: String) -> Self {
        Self {
            name: entry.name,
            peer_name,
            size: Some(entry.size),
            is_dir: Some(entry.is_dir),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Nothing),
            request: RwSignal::new(None),
        }
    }

    pub fn from_downloading_file(
        name: String,
        peer_name: String,
        download_status: DownloadStatus,
    ) -> Self {
        Self {
            name,
            peer_name,
            size: None,
            is_dir: Some(false),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(download_status),
            request: RwSignal::new(None),
        }
    }
}

impl From<UploadInfo> for File {
    fn from(upload: UploadInfo) -> Self {
        Self {
            name: upload.path,
            peer_name: upload.peer_name,
            size: Some(upload.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Uploading {
                bytes_read: upload.bytes_read,
                total_size: upload.total_size,
                speed: upload.speed as u64,
            }),
            request: RwSignal::new(None),
        }
    }
}

/// The context in which we are displaying this file
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum FileDisplayContext {
    /// List of a peer's files
    Peer,
    /// List of downloading / uploading files
    Transfer,
    /// List of files matching a searchterm
    SearchResult,
}

#[component]
pub fn File(file: File, is_shared: bool, context: FileDisplayContext) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let set_files = app_context.set_files;
    let (file_name, _set_file_name) = signal(file.name);
    let peer_name = file.peer_name.clone();
    let peer_name_for_uploader = file.peer_name.clone();

    let app_context_1 = app_context.clone();
    let download_request = move |_| {
        app_context_1.download(PeerPath {
            path: file_name.get().to_string(),
            peer_name: peer_name.clone(),
        });
    };

    // Only display download button if we dont have it requested, and it is not our share
    let download_button = move || {
        if file.download_status.get_untracked() == DownloadStatus::Nothing
            && !is_shared
            && file.request.get_untracked() == None
            && context != FileDisplayContext::Transfer
        {
            Either::Left(view! {
                <span title="Download">
                    <Button
                        icon=icondata::FiDownload
                        on:click=download_request
                        size=ButtonSize::Small
                    />
                </span>
            })
        } else {
            Either::Right(view! {})
        }
    };

    let peer_name = file.peer_name.clone();
    let expand_dir = move |_| {
        if file.is_dir.unwrap_or_default() {
            if file.is_expanded.get() {
                // Collapse dir by either setting all children to insible
                log::info!("Collapse dir");
                let peer_name = file.peer_name.clone();
                set_files.update(|files| {
                    let mut upper_bound = file_name.get();
                    upper_bound.push_str("~");
                    for (_, file) in files.range_mut((
                        Excluded(PeerPath {
                            peer_name: peer_name.clone(),
                            path: file_name.get(),
                        }),
                        Excluded(PeerPath {
                            peer_name: file.peer_name.clone(),
                            path: upper_bound,
                        }),
                    )) {
                        log::info!("{}", file.name);
                        file.is_visible.set(false);
                    }
                });
                file.is_expanded.set(false);
            } else {
                let query = IndexQuery {
                    path: Some(file_name.get()),
                    searchterm: None,
                    recursive: false,
                };
                debug!("is_shared {}", is_shared);

                if is_shared {
                    app_context.shares_query(query);
                } else {
                    let peer_name = Some(peer_name.clone());
                    app_context.files(FilesQuery { query, peer_name });
                }

                // Issue here is that if this is repeatedly clicked before file is loaded we lose
                // state
                file.is_expanded.set(true);

                let peer_name = file.peer_name.clone();
                set_files.update(|files| {
                    let mut upper_bound = file_name.get();
                    upper_bound.push_str("~");
                    for (_, file) in files.range_mut((
                        Excluded(PeerPath {
                            peer_name: peer_name.clone(),
                            path: file_name.get(),
                        }),
                        Excluded(PeerPath {
                            peer_name: file.peer_name.clone(),
                            path: upper_bound,
                        }),
                    )) {
                        log::info!("{}", file.name);
                        file.is_visible.set(true);
                    }
                })
            }
        }
    };

    let file_name_and_indentation = move || {
        let full_path = file_name.get();
        let uploader_label = match context {
            FileDisplayContext::Transfer => match file.download_status.get() {
                DownloadStatus::Uploading { .. } => Some(peer_name_for_uploader.clone()),
                _ => None,
            },
            _ => None,
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
        let (display_name, indentation) = match full_path.rsplit_once('/') {
            Some((path, name)) => {
                let indent = path.split('/').count();
                let indent_str = "  ".repeat(indent);
                (name.to_string(), indent_str)
            }
            None => (full_path.clone(), Default::default()),
        };
        view! {
            <pre title=full_path
                .clone()>
                {match uploader_label {
                    Some(peer_name) => {
                        let peer_name_title = peer_name.clone();
                        view! {
                            <span class="file-peer-label" title=peer_name_title>
                                {peer_name.clone()}
                                " "
                            </span>
                        }
                            .into_any()
                    }
                    None => view! { <span></span> }.into_any(),
                }} {indentation} {icon} " "
                <span class="text-sm font-medium" title=display_name.clone()>
                    {display_name.clone()}
                </span>
            </pre>
        }
    };

    let is_dir = file.is_dir == Some(true);

    view! {
        <TableRow on:click=expand_dir>
            <TableCell>{file_name_and_indentation}</TableCell>
            <TableCell class="file-meta-cell">
                <div class="file-meta-top">
                    <span>
                        {match file.size {
                            Some(size) => display_bytes(size),
                            None => "-".to_string(),
                        }}
                    </span>
                    {download_button()}
                </div>
                <div class="file-meta-status">
                    {move || {
                        match file.download_status.get() {
                            DownloadStatus::Nothing => EitherOf6::A(view! { <span></span> }),
                            DownloadStatus::Downloaded(_) => {
                                if is_dir {
                                    EitherOf6::B(view! { <span>"Downloaded"</span> })
                                } else {
                                    let file_path = file_name.get();
                                    EitherOf6::C(
                                        view! {
                                            "Downloaded"
                                            <Preview file_path=&file_path shared=is_shared />
                                        },
                                    )
                                }
                            }
                            DownloadStatus::Requested(_) => {
                                EitherOf6::D(view! { <span>"Requested"</span> })
                            }
                            DownloadStatus::Downloading { bytes_read, .. } => {
                                EitherOf6::E(
                                    view! {
                                        <span>
                                            <DownloadingFile bytes_read size=file.size />
                                        </span>
                                    },
                                )
                            }
                            DownloadStatus::Uploading { bytes_read, total_size, speed } => {
                                EitherOf6::F(
                                    view! {
                                        <span>
                                            <UploadingFile bytes_read total_size speed />
                                        </span>
                                    },
                                )
                            }
                        }
                    }}
                    {move || {
                        if is_shared {
                            view! {
                                // view! { <span><Preview file_path=&file_name.get() shared=true /></span> }
                                <span></span>
                            }
                        } else {
                            view! {
                                // view! { <span><Preview file_path=&file_name.get() shared=true /></span> }
                                <span></span>
                            }
                        }
                    }}
                </div>
            </TableCell>
        </TableRow>
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadStatus {
    Nothing,
    Requested(u32),
    Downloading { bytes_read: u64, request_id: u32 },
    Uploading {
        bytes_read: u64,
        total_size: u64,
        speed: u64,
    },
    Downloaded(u32),
}

/// Show progress when currently downloading
#[component]
pub fn DownloadingFile(bytes_read: u64, size: Option<u64>) -> impl IntoView {
    let progress: f64 = match size {
        Some(0) => 0.0,
        Some(size) => bytes_read as f64 / size as f64,
        None => 0.0,
    };
    let value = RwSignal::new(progress);
    view! {
        <ProgressBar value />
        <span class="download-progress-text">
            {match size {
                Some(size) if size > 0 => {
                    format!(
                        "Downloading {} / {}...",
                        display_bytes(bytes_read),
                        display_bytes(size),
                    )
                }
                _ => format!("Downloading {}...", display_bytes(bytes_read)),
            }}
        </span>
    }
}

/// Show progress when currently uploading
#[component]
pub fn UploadingFile(bytes_read: u64, total_size: u64, speed: u64) -> impl IntoView {
    let progress = if total_size == 0 {
        0.0
    } else {
        bytes_read as f64 / total_size as f64
    };
    let value = RwSignal::new(progress);
    view! {
        <ProgressBar value />
        <span class="download-progress-text">
            {format!(
                "Uploading {} / {} ({}/s)",
                display_bytes(bytes_read),
                display_bytes(total_size),
                display_bytes(speed),
            )}
        </span>
    }
}

/// Allow a locally stored file to be opened / downloaded
#[component]
fn Preview<'a>(file_path: &'a str, shared: bool) -> impl IntoView {
    let sub_path = if shared { "shared" } else { "downloads" };

    match document().location() {
        Some(location) => {
            let protocol = location.protocol().unwrap_or("http:".to_string());
            let host = location.host().unwrap_or("localhost:3030".to_string());
            let escaped_path = urlencoding::encode(&file_path);
            Either::Left(view! {
                <span>
                    <a
                        href=format!("{}//{}/{}/{}", protocol, host, sub_path, escaped_path)
                        target="_blank"
                    >
                        <Button size=ButtonSize::Small>"View"</Button>
                    </a>
                </span>
            })
        }
        None => Either::Right(view! { <span>"Cannot get URL"</span> }),
    }
}
