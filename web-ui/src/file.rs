//! Display a file - either from a remote peer or one of our own shared files
use crate::{
    display_bytes,
    ui_messages::{Command, UiDownloadRequest},
    Entry, RequesterSetter, BUTTON_STYLE,
};
use leptos::*;

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
            download_status: create_rw_signal(DownloadStatus::Nothing),
            request: create_rw_signal(None),
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
            download_status: create_rw_signal(download_status),
            request: create_rw_signal(None),
        }
    }
}

#[derive(Eq, PartialEq)]
pub enum FileDisplayContext {
    Peer,
    Transfer,
    SearchResult,
}

#[component]
pub fn File(file: File, is_shared: bool, context: FileDisplayContext) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>().unwrap().0;
    // let peer_details = use_context::<PeerName>().unwrap().0;
    let (file_name, _set_file_name) = create_signal(file.name);

    let download_request = move |_| {
        let download = Command::Download {
            path: file_name.get().to_string(),
            peer_name: file.peer_name.clone(),
        };
        set_requester.update(|requester| requester.make_request(download));
    };

    // Only display download button if we dont have it requested, and it is not our share
    let download_button_style = move || {
        if file.download_status.get() == DownloadStatus::Nothing
            && !is_shared
            && file.request.get() == None
            && context != FileDisplayContext::Transfer
        {
            ""
        } else {
            "display:none"
        }
    };

    let file_name_and_indentation = move || {
        let file_name = file_name.get();
        let icon = match file.is_dir {
            Some(true) => "🗀 ",
            Some(false) => "🗎 ",
            None => " ",
        };
        let (name, indentation) = match file_name.rsplit_once('/') {
            Some((path, name)) => {
                let indent = path.split('/').count();
                let indent_str = "  ".repeat(indent);
                (name.to_string(), indent_str)
            }
            None => (file_name, Default::default()),
        };
        view! {
            <pre>
                {indentation} <strong>{icon}</strong>
                <span class="text-sm font-medium">{name}</span>
            </pre>
        }
    };

    let is_dir = file.is_dir == Some(true);

    view! {
        <tr class="hover:bg-gray-200">
            <td>{file_name_and_indentation}</td>
            <td>
                " " {display_bytes(file.size.unwrap_or_default())} " "
                <button
                    class=BUTTON_STYLE
                    style=download_button_style
                    on:click=download_request
                    title="Download"
                >
                    "🠫"
                </button>
                {move || {
                    match file.download_status.get() {
                        DownloadStatus::Nothing => {
                            view! { <span></span> }
                        }
                        DownloadStatus::Downloaded(_) => {
                            if is_dir {
                                view! { <span> "Downloaded" </span> }
                            } else {

                            view! { <span>
                                "Downloaded"
                                        <Preview file_path=&file_name.get() shared=is_shared />
                                    </span> }
                            }
                        }
                        DownloadStatus::Requested(_) => {
                            view! { <span>"Requested"</span> }
                        }
                        DownloadStatus::Downloading{ bytes_read, .. } => {
                            view! {
                                <span>
                                    <DownloadingFile bytes_read size=file.size/>
                                </span>
                            }
                        }
                    }
                }}
                {// TODO fix this
                move || {
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

            </td>
        </tr>
    }
}

// enum LocalStorage {
//     Downloads,
//     Shared(String),
// }

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
    // This will be a progress bar
    view! {
        <span>{format!("Downloading {} of {} bytes...", bytes_read, size.unwrap_or_default())}</span>
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
            view! {
                <span>
                    <button class=BUTTON_STYLE>
                        <a
                            href=format!("{}//{}/{}/{}", protocol, host, sub_path, escaped_path)
                            target="_blank"
                        >
                            "View"
                        </a>
                    </button>
                </span>
            }
        }
        None => {
            view! { <span>"Cannot get URL"</span> }
        }
    }
}
