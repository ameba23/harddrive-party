//! Display a file - either from a remote peer or one of our own shared files
use crate::{
    hdp::{display_bytes, Entry},
    ui_messages::{FilesQuery, UiDownloadRequest},
    AppContext, FilesSignal, PeerPath,
};
use harddrive_party_shared::wire_messages::IndexQuery;
use leptos::{
    either::{Either, EitherOf4, EitherOf5},
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

/// The context in which we are displaying this file
#[derive(Eq, PartialEq)]
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
    let set_files = use_context::<FilesSignal>().unwrap().1;
    let (file_name, _set_file_name) = signal(file.name);
    let peer_name = file.peer_name.clone();

    let download_request = move |_| {
        // let download = Command::Download {
        //     path: file_name.get().to_string(),
        //     peer_name: peer_name.clone(),
        // };
        // set_requester.update(|requester| requester.make_request(download));
    };

    // Only display download button if we dont have it requested, and it is not our share
    let download_button = move || {
        if file.download_status.get() == DownloadStatus::Nothing
            && !is_shared
            && file.request.get() == None
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
                    app_context.shares_query(query, app_context.own_name.get(), set_files);
                } else {
                    let peer_name = Some(peer_name.clone());
                    app_context.files(FilesQuery { query, peer_name }, set_files);
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
        let file_name = file_name.get();
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
        let (name, indentation) = match file_name.rsplit_once('/') {
            Some((path, name)) => {
                let indent = path.split('/').count();
                let indent_str = "  ".repeat(indent);
                (name.to_string(), indent_str)
            }
            None => (file_name, Default::default()),
        };
        view! { <pre>{indentation} {icon} " " <span class="text-sm font-medium">{name}</span></pre> }
    };

    let is_dir = file.is_dir == Some(true);

    view! {
        <TableRow on:click=expand_dir>
            <TableCell>{file_name_and_indentation}</TableCell>
            <TableCell>
                " " {display_bytes(file.size.unwrap_or_default())} " " {download_button()}
                {move || {
                    match file.download_status.get() {
                        DownloadStatus::Nothing => EitherOf5::A(view! { <span></span> }),
                        DownloadStatus::Downloaded(_) => {
                            if is_dir {
                                EitherOf5::B(view! { <span>"Downloaded"</span> })
                            } else {
                                let file_path = file_name.get();
                                EitherOf5::C(
                                    view! {
                                        "Downloaded"
                                        <Preview file_path=&file_path shared=is_shared />
                                    },
                                )
                            }
                        }
                        DownloadStatus::Requested(_) => {
                            EitherOf5::D(view! { <span>"Requested"</span> })
                        }
                        DownloadStatus::Downloading { bytes_read, .. } => {
                            EitherOf5::E(
                                view! {
                                    <span>
                                        <DownloadingFile bytes_read size=file.size />
                                    </span>
                                },
                            )
                        }
                    }
                }} // TODO fix this
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

            </TableCell>
        </TableRow>
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadStatus {
    Nothing,
    Requested(u32),
    Downloading { bytes_read: u64, request_id: u32 },
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
        <span>
            {format!("Downloading {} of {} bytes...", bytes_read, size.unwrap_or_default())}
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
