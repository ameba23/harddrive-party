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
    let (file_name, _set_file_name) = signal(file.name);
    let peer_name_for_uploader = file.peer_name.clone();

    let app_context_1 = app_context.clone();
    let peer_name_for_download = file.peer_name.clone();

    let is_dir = file.is_dir == Some(true);

    // Only display download button if we dont have it requested, and it is not our share
    let download_button = move || {
        if file.download_status.get() == DownloadStatus::Nothing
            && !is_shared
            && file.request.get() == None
            && context != FileDisplayContext::Transfer
        {
            let app_ctx = app_context_1.clone();
            let p_name = peer_name_for_download.clone();
            Either::Left(view! {
                <span title="Download">
                    <Button
                        icon=icondata::FiDownload
                        on:click=move |event: leptos::ev::MouseEvent| {
                            event.stop_propagation();
                            app_ctx
                                .download(PeerPath {
                                    path: file_name.get().to_string(),
                                    peer_name: p_name.clone(),
                                });
                        }
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
                <span class:font-medium=is_dir title=display_name.clone()>
                    {display_name.clone()}
                </span>
            </pre>
        }
    };

    view! {
        <TableRow
            class:file-row--downloaded=move || {
                !is_dir && matches!(file.download_status.get(), DownloadStatus::Downloaded(_))
            }
            class:file-row--directory=move || is_dir
            on:click=expand_dir
        >
            <TableCell>{file_name_and_indentation}</TableCell>
            <TableCell class="file-meta-cell">
                <div class="file-meta-top">
                    <span>
                        {match file.size {
                            Some(size) => display_bytes(size),
                            None => "-".to_string(),
                        }}
                    </span>
                    {move || download_button()}
                </div>
                <div class="file-meta-status">
                    {move || {
                        match file.download_status.get() {
                            DownloadStatus::Nothing => EitherOf6::A(view! { <span></span> }),
                            DownloadStatus::Downloaded(_) => {
                                if is_dir {
                                    EitherOf6::B(
                                        view! { <span class="status-pill status-pill--complete">"Downloaded"</span> },
                                    )
                                } else {
                                    let file_path = file_name.get();
                                    EitherOf6::C(
                                        view! {
                                            <span class="status-pill status-pill--complete">"Downloaded"</span>
                                            <Preview file_path=&file_path shared=is_shared />
                                        },
                                    )
                                }
                            }
                            DownloadStatus::Requested(_) => {
                                EitherOf6::D(
                                    view! { <span class="status-pill status-pill--pending">"Requested"</span> },
                                )
                            }
                            DownloadStatus::Downloading { bytes_read, .. } => {
                                EitherOf6::E(
                                    view! {
                                        <span class="transfer-state">
                                            <DownloadingFile bytes_read size=file.size />
                                        </span>
                                    },
                                )
                            }
                            DownloadStatus::Uploading { bytes_read, total_size, speed } => {
                                EitherOf6::F(
                                    view! {
                                        <span class="transfer-state">
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
    Downloading {
        bytes_read: u64,
        request_id: u32,
    },
    Uploading {
        bytes_read: u64,
        total_size: u64,
        speed: u64,
    },
    Downloaded(u32),
}

impl DownloadStatus {
    pub fn merge_request_snapshot(&self, incoming: Self) -> Self {
        match (self, &incoming) {
            (
                DownloadStatus::Downloading {
                    request_id: current_id,
                    ..
                },
                DownloadStatus::Requested(incoming_id),
            ) if current_id == incoming_id => self.clone(),
            (DownloadStatus::Downloaded(current_id), DownloadStatus::Requested(incoming_id))
            | (
                DownloadStatus::Downloaded(current_id),
                DownloadStatus::Downloading {
                    request_id: incoming_id,
                    ..
                },
            ) if current_id == incoming_id => self.clone(),
            _ => incoming,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::DownloadStatus;

    #[test]
    fn request_snapshot_does_not_downgrade_active_download() {
        let current = DownloadStatus::Downloading {
            bytes_read: 1024,
            request_id: 42,
        };

        assert_eq!(
            current.merge_request_snapshot(DownloadStatus::Requested(42)),
            current
        );
    }

    #[test]
    fn request_snapshot_can_complete_active_download() {
        let current = DownloadStatus::Downloading {
            bytes_read: 1024,
            request_id: 42,
        };

        assert_eq!(
            current.merge_request_snapshot(DownloadStatus::Downloaded(42)),
            DownloadStatus::Downloaded(42)
        );
    }

    #[test]
    fn request_snapshot_for_new_request_can_replace_old_status() {
        let current = DownloadStatus::Downloading {
            bytes_read: 1024,
            request_id: 42,
        };

        assert_eq!(
            current.merge_request_snapshot(DownloadStatus::Requested(43)),
            DownloadStatus::Requested(43)
        );
    }

    #[test]
    fn stale_progress_does_not_downgrade_completed_download() {
        let current = DownloadStatus::Downloaded(42);

        assert_eq!(
            current.merge_request_snapshot(DownloadStatus::Downloading {
                bytes_read: 1024,
                request_id: 42,
            }),
            current
        );
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
                        rel="noopener noreferrer"
                        on:click=move |event| event.stop_propagation()
                    >
                        <Button size=ButtonSize::Small>"View"</Button>
                    </a>
                </span>
            })
        }
        None => Either::Right(view! { <span>"Cannot get URL"</span> }),
    }
}
