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
    use gloo_timers::future::sleep;
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
        let app_context_for_mount = app_context.clone();

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert_eq!(text.matches("Downloading").count(), 2);
        assert!(text.contains("asphericKingCrab"));
        assert!(text.contains("single.mp3"));

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
        let app_context_for_mount = app_context.clone();

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert_eq!(text.matches("Downloading").count(), 3);
        assert!(text.contains("asphericKingCrab"));
        assert!(text.contains("music"));
        assert!(text.contains("single.mp3"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    async fn updates_directory_request_when_more_children_arrive() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "music".to_string(),
            progress: 512,
            total_size: 2048,
            request_id: 2003,
            timestamp: Duration::from_secs(1_710_000_003),
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
            size: Some(1024),
            is_dir: Some(false),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloading {
                bytes_read: 512,
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
        let app_context_for_mount = app_context.clone();

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        assert!(host.text_content().unwrap_or_default().contains("single.mp3"));
        assert!(!host.text_content().unwrap_or_default().contains("bonus.flac"));

        app_context.set_files.update(|files| {
            files.insert(
                PeerPath {
                    peer_name: request.peer_name.clone(),
                    path: "music/bonus.flac".to_string(),
                },
                File {
                    name: "music/bonus.flac".to_string(),
                    peer_name: request.peer_name.clone(),
                    size: Some(1024),
                    is_dir: Some(false),
                    is_expanded: RwSignal::new(false),
                    is_visible: RwSignal::new(true),
                    download_status: RwSignal::new(DownloadStatus::Requested(request.request_id)),
                    request: RwSignal::new(Some(request.clone())),
                },
            );
        });
        sleep(Duration::from_millis(1)).await;

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("music"));
        assert!(text.contains("single.mp3"));
        assert!(text.contains("bonus.flac"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn shows_completed_directory_summary_with_peer_and_path() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "music".to_string(),
            progress: 1024,
            total_size: 1024,
            request_id: 2004,
            timestamp: Duration::from_secs(1_710_000_004),
            peer_name: "asphericKingCrab".to_string(),
        };
        let request_root = File {
            name: request.path.clone(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(true),
            is_expanded: RwSignal::new(true),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloaded(request.request_id)),
            request: RwSignal::new(Some(request.clone())),
        };
        let child_file = File {
            name: "music/single.mp3".to_string(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(false),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloaded(request.request_id)),
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
        let app_context_for_mount = app_context.clone();

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("asphericKingCrab"));
        assert!(text.contains("music"));
        assert!(text.contains("Downloaded"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn shows_view_button_for_completed_top_level_file_request() {
        let host = mount_host();
        let request = UiDownloadRequest {
            path: "movie.mp4".to_string(),
            progress: 1024,
            total_size: 1024,
            request_id: 2005,
            timestamp: Duration::from_secs(1_710_000_005),
            peer_name: "asphericKingCrab".to_string(),
        };
        let request_file = File {
            name: request.path.clone(),
            peer_name: request.peer_name.clone(),
            size: Some(request.total_size),
            is_dir: Some(false),
            is_expanded: RwSignal::new(true),
            is_visible: RwSignal::new(true),
            download_status: RwSignal::new(DownloadStatus::Downloaded(request.request_id)),
            request: RwSignal::new(Some(request.clone())),
        };
        let mut files = BTreeMap::new();
        files.insert(
            PeerPath {
                peer_name: request.peer_name.clone(),
                path: request.path.clone(),
            },
            request_file,
        );

        let app_context = AppContext::for_tests();
        app_context.set_files.set(files.clone());

        let mut requests = Requests::new();
        requests.insert(&request);
        let (requests, _set_requests) = signal(requests);
        let (files, _set_files) = signal(files);
        let (uploads, _set_uploads) = signal(Uploads::new());
        let app_context_for_mount = app_context.clone();

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Transfers requests files uploads />
                </ConfigProvider>
            }
        });

        let text = host.text_content().unwrap_or_default();
        assert!(text.contains("movie.mp4"));
        assert!(text.contains("Downloaded"));
        assert!(text.contains("View"));

        drop(handle);
        host.remove();
    }
}
