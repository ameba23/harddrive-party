use crate::{
    file::File,
    requests::{Request, Requests},
    uploads::{UploadRow, Uploads},
    PeerPath,
};
use leptos::prelude::*;
use std::collections::BTreeMap;
use thaw::*;

#[component]
pub fn Transfers(
    requests: ReadSignal<Requests>,
    files: ReadSignal<BTreeMap<PeerPath, File>>,
    uploads: ReadSignal<Uploads>,
) -> impl IntoView {
    let wishlist = move || {
        requests
            .get()
            .iter()
            .filter_map(|((_timestamp, _id), peer_path)| {
                let files = files.get();
                match files.get(peer_path) {
                    Some(file) => Some(file.clone()),
                    None => None,
                }
            })
            .collect::<Vec<File>>()
    };
    let uploads_list = move || uploads.get().iter().cloned().collect::<Vec<_>>();

    view! {
        <h2 class="text-xl">"Transfers"</h2>
        <h3 class="text-lg">"Requested"</h3>
        <Flex vertical=true class="transfer-list">
            <For
                each=wishlist
                key=|file| format!("{}{:?}", file.name, file.size)
                children=move |file| view! { <Request file /> }
            />
        </Flex>
        <h3 class="text-lg">"Uploads"</h3>
        <Table class="transfer-table file-table">
            <TableBody>
                <For
                    each=uploads_list
                    key=|upload| {
                        let data = upload.get_untracked();
                        format!("{}:{}", data.peer_name, data.name)
                    }
                    children=move |upload| view! { <UploadRow upload=upload /> }
                />
            </TableBody>
        </Table>
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::{
        file::DownloadStatus,
        requests::Requests,
        ui_messages::UiDownloadRequest,
        uploads::Uploads,
        AppContext,
    };
    use harddrive_party_shared::ui_messages::UploadInfo;
    use leptos::mount::mount_to;
    use leptos::wasm_bindgen::JsCast;
    use std::time::Duration;
    use thaw::ConfigProvider;
    use web_sys::HtmlElement;
    use wasm_bindgen_test::wasm_bindgen_test;

    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn mount_host() -> HtmlElement {
        let document = document();
        let host = document
            .create_element("div")
            .expect("host element should be created")
            .dyn_into::<HtmlElement>()
            .expect("host should be an HtmlElement");
        document
            .body()
            .expect("document body should exist")
            .append_child(&host)
            .expect("host should be appended");
        host
    }

    #[wasm_bindgen_test]
    fn renders_requested_downloads_and_uploads() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "film/trailer.mov".to_string(),
            progress: 512,
            total_size: 1024,
            request_id: 2000,
            timestamp: Duration::from_secs(1_710_000_000),
            peer_name: "asphericKingCrab".to_string(),
        };
        let request_file = File {
            name: request.path.clone(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(true),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloading {
                bytes_read: request.progress,
                request_id: request.request_id,
            }),
            request: RwSignal::new(Some(request.clone())),
        };
        let request_peer_path = PeerPath {
            peer_name: request.peer_name.clone(),
            path: request.path.clone(),
        };
        let mut files = BTreeMap::new();
        files.insert(request_peer_path, request_file);

        let app_context = AppContext::for_tests();
        app_context.set_files.set(files.clone());

        let mut requests = Requests::new();
        requests.insert(&request);
        let (requests, _set_requests) = signal(requests);
        let (files, _set_files) = signal(files);

        let mut uploads = Uploads::new();
        uploads.upsert(UploadInfo {
            path: "/home/pumkin/video-archive.mkv".to_string(),
            bytes_read: 2048,
            total_size: 4096,
            speed: 512,
            peer_name: "lunarTulipOx".to_string(),
        });
        let (uploads, _set_uploads) = signal(uploads);

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("Requested"));
        assert!(text.contains("Uploads"));
        assert!(text.contains("asphericKingCrab"));
        assert!(text.contains("trailer.mov"));
        assert!(text.contains("lunarTulipOx"));
        assert!(text.contains("video-archive.mkv"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn renders_single_file_download_with_one_progress_bar() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "music/single.mp3".to_string(),
            progress: 512,
            total_size: 1024,
            request_id: 2001,
            timestamp: Duration::from_secs(1_710_000_001),
            peer_name: "asphericKingCrab".to_string(),
        };
        let request_file = File {
            name: request.path.clone(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(true),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloading {
                bytes_read: request.progress,
                request_id: request.request_id,
            }),
            request: RwSignal::new(Some(request.clone())),
        };
        let request_peer_path = PeerPath {
            peer_name: request.peer_name.clone(),
            path: request.path.clone(),
        };
        let mut files = BTreeMap::new();
        files.insert(request_peer_path, request_file);

        let app_context = AppContext::for_tests();
        app_context.set_files.set(files.clone());

        let mut requests = Requests::new();
        requests.insert(&request);
        let (requests, _set_requests) = signal(requests);
        let (files, _set_files) = signal(files);
        let (uploads, _set_uploads) = signal(Uploads::new());

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert_eq!(text.matches("Downloading").count(), 1);

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn renders_single_child_request_with_one_progress_bar() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "music".to_string(),
            progress: 512,
            total_size: 1024,
            request_id: 2002,
            timestamp: Duration::from_secs(1_710_000_002),
            peer_name: "asphericKingCrab".to_string(),
        };
        let request_root = File {
            name: request.path.clone(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(true),
            is_expanded: RwSignal::new(true),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloading {
                bytes_read: request.progress,
                request_id: request.request_id,
            }),
            request: RwSignal::new(Some(request.clone())),
        };
        let child_file = File {
            name: "music/single.mp3".to_string(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloading {
                bytes_read: request.progress,
                request_id: request.request_id,
            }),
            request: RwSignal::new(Some(request.clone())),
        };
        let mut files = BTreeMap::new();
        files.insert(
            PeerPath {
                peer_name: request.peer_name.clone(),
                path: request.path.clone(),
            },
            request_root,
        );
        files.insert(
            PeerPath {
                peer_name: request.peer_name.clone(),
                path: "music/single.mp3".to_string(),
            },
            child_file,
        );

        let app_context = AppContext::for_tests();
        app_context.set_files.set(files.clone());

        let mut requests = Requests::new();
        requests.insert(&request);
        let (requests, _set_requests) = signal(requests);
        let (files, _set_files) = signal(files);
        let (uploads, _set_uploads) = signal(Uploads::new());

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert_eq!(text.matches("Downloading").count(), 1);

        drop(handle);
        host.remove();
    }
}
