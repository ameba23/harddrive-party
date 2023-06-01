//! Display a file - either from a remote peer or one of our own shared files
use crate::{
    display_bytes,
    ui_messages::{Command, UiDownloadRequest},
    DownloadResponse, Entry, PeerName, RequesterSetter, BUTTON_STYLE,
};
use leptos::*;

#[derive(Clone, Debug)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub download_status: RwSignal<DownloadStatus>,
    pub request: RwSignal<Option<UiDownloadRequest>>,
}

impl File {
    pub fn from_entry(cx: Scope, entry: Entry) -> Self {
        Self {
            name: entry.name,
            size: entry.size,
            is_dir: entry.is_dir,
            download_status: create_rw_signal(cx, DownloadStatus::Nothing),
            request: create_rw_signal(cx, None),
        }
    }

    pub fn from_download_request(
        cx: Scope,
        download_request: UiDownloadRequest,
        download_status: DownloadStatus,
    ) -> Self {
        Self {
            name: download_request.path.clone(),
            size: download_request.size,
            is_dir: false,
            download_status: create_rw_signal(cx, download_status),
            request: create_rw_signal(cx, Some(download_request)),
        }
    }
}

#[component]
pub fn File(cx: Scope, file: File, is_shared: bool) -> impl IntoView {
    let set_requester = use_context::<RequesterSetter>(cx).unwrap().0;
    let peer_details = use_context::<PeerName>(cx).unwrap().0;
    let (file_name, _set_file_name) = create_signal(cx, file.name);

    let download_request = move |_| {
        let download = Command::Download {
            path: file_name.get().to_string(),
            peer_name: peer_details.get().0,
        };
        set_requester.update(|requester| requester.make_request(download));
    };

    let download_button_style = move || {
        if file.download_status.get() == DownloadStatus::Nothing && !peer_details.get().1 {
            ""
        } else {
            "display:none"
        }
    };

    view! { cx,
        <li>
          <code>
            <strong>{ if file.is_dir { "🗀 " } else { "🗎 " } }</strong>
            <span class="text-sm font-medium">
            { file_name.get() }
        </span>
          </code>
          " "{ display_bytes(file.size) } " "
          <button class={ BUTTON_STYLE } style={ download_button_style } on:click=download_request title="Download">"🠫"</button>
          {
                move || {
                match file.download_status.get() {
                        DownloadStatus::Nothing => {
                            view! { cx, <span /> }
                        }
                        DownloadStatus::Downloaded => {
                            view! { cx, <span> "Downloaded" </span> }
                        }
                        DownloadStatus::Requested => {
                            view! { cx, <span> "Requested" </span> }
                        }
                        DownloadStatus::Downloading(download_response) => {
                            view! { cx, <span><DownloadingFile download_response size=file.size /></span> }
                        }
                }
                }
        }
        {
            // TODO fix this
            move || {
                if is_shared {
                    view! { cx, <span><Preview file_path=&file_name.get() /></span> }
                } else {
                    view! { cx, <span /> }
                }
            }
        }
        </li>
    }
}

// enum LocalStorage {
//     Downloads,
//     Shared(String),
// }

#[derive(Clone, Debug, PartialEq)]
pub enum DownloadStatus {
    Nothing,
    Requested,
    Downloading(DownloadResponse),
    Downloaded,
}

/// Show progress when currently downloading
#[component]
pub fn DownloadingFile(cx: Scope, download_response: DownloadResponse, size: u64) -> impl IntoView {
    view! { cx,
        <span>
            { format!("Downloading {} of {} bytes...", download_response.bytes_read, size) }
        </span>
    }
}

/// A file which has been requested / downloaded
#[component]
pub fn Request(cx: Scope, file: File) -> impl IntoView {
    let request_option = file.request.get();
    match request_option {
        Some(request) => {
            view! { cx,
                    <li>
                        { request.peer_name } " "
                        <code>{ &request.path }</code>
                        " " { display_bytes(request.size) } " "
                        {
                            move || {
                                match file.download_status.get() {
                                    DownloadStatus::Downloading(download_response) => {
                                        view! { cx, <span><DownloadingFile download_response size=file.size /></span> }
                                    }
                                    DownloadStatus::Downloaded => {
                                        view! { cx, <span><Preview file_path=&request.path /></span> }
                                    }
                                    _ => {
                                        view! { cx, <span /> }
                                    }
                                }
                            }
                        }
                    </li>
            }
        }
        None => {
            view! { cx,
              <li>"Never happens"</li>
            }
        }
    }
}

/// Allow a locally stored file to be opened / downloaded
#[component]
fn Preview<'a>(cx: Scope, file_path: &'a str) -> impl IntoView {
    let escaped_path = urlencoding::encode(&file_path);
    view! { cx, <span><a href={ format!("http://localhost:3030/downloads/{}", escaped_path)}>"view"</a></span> }
}
