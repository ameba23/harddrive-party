use crate::{
    components::announce_address::AnnounceAddressView,
    display_bytes,
    file::{File, FileDisplayContext},
    AppContext, PeerPath,
};
use harddrive_party_shared::wire_messages::{AnnounceAddress, PeerConnectionDetails};
use leptos::{either::Either, prelude::*};
use qrcode::{render::svg, QrCode};
use std::collections::HashSet;
use std::ops::Bound::Included;
use thaw::*;

fn is_visible_in_peer_tree(
    files: &std::collections::BTreeMap<PeerPath, File>,
    peer_path: &PeerPath,
) -> bool {
    if peer_path.path.is_empty() {
        return true;
    }

    let mut current = String::new();
    for component in peer_path
        .path
        .split('/')
        .take_while(|component| !component.is_empty())
    {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(component);

        if current == peer_path.path {
            break;
        }

        let Some(ancestor) = files.get(&PeerPath {
            peer_name: peer_path.peer_name.clone(),
            path: current.clone(),
        }) else {
            return false;
        };

        if !ancestor.is_expanded.get() {
            return false;
        }
    }

    true
}

#[component]
pub fn Peer(name: String, is_self: bool) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let files = app_context.get_files;

    // This signal is used below to provide context to File
    let (peer_signal, _set_peer) = signal((name.clone(), is_self));

    let root_size = move || {
        display_bytes(
            match files.get().get(&PeerPath {
                peer_name: peer_signal.get().0,
                path: "".to_string(),
            }) {
                Some(file) => file.size.unwrap_or_default(),
                None => 0,
            },
        )
    };
    let disconnect_peer = move |_| {
        app_context.disconnect(peer_signal.get_untracked().0);
    };

    let files_iter = move || {
        // Calling .get() clones - we should ideally use .with(|files| files.range...)
        let files = files.get();
        // Get only files from this peer using a range of the BTreeMap
        files
            .range((
                Included(PeerPath {
                    peer_name: peer_signal.get().0,
                    path: "".to_string(),
                }),
                Included(PeerPath {
                    peer_name: format!("{}~", peer_signal.get().0),
                    path: "".to_string(), // TODO
                }),
            ))
            .filter(|(peer_path, _)| is_visible_in_peer_tree(&files, peer_path))
            .map(|(_, file)| file.clone()) // TODO ideally dont clone
            .collect::<Vec<File>>()
    };

    view! {
        <div class="peer-card">
            <Flex vertical=true>
                <Flex class="peer-card__header" justify=FlexJustify::SpaceBetween align=FlexAlign::Center>
                    <div>
                        <Icon icon=icondata::AiUserOutlined />
                        {move || peer_signal.get().0}
                        " "
                        {root_size}
                        " shared"
                    </div>
                    {(!is_self).then(|| {
                        view! {
                            <div class="disconnect-wrap">
                                <Button size=ButtonSize::Small on:click=disconnect_peer>
                                    "Disconnect"
                                </Button>
                            </div>
                        }
                    })}
                </Flex>
                <div class="table-scroll">
                    <Table class="file-table">
                        <TableBody>
                            <For
                                each=files_iter
                                key=|file| file.name.clone()
                                children=move |file: File| {
                                    view! {
                                        <File
                                            file
                                            is_shared=is_self
                                            context=FileDisplayContext::Peer
                                        />
                                    }
                                }
                            />
                        </TableBody>
                    </Table>
                </div>
            </Flex>
        </div>
    }
}

#[component]
pub fn Peers(
    announce_address: ReadSignal<Option<String>>,
    pending_peers: ReadSignal<HashSet<String>>,
    known_peers: ReadSignal<Vec<AnnounceAddress>>,
) -> impl IntoView {
    let app_context = use_context::<AppContext>().unwrap();
    let qr_svg = move || {
        announce_address.get().and_then(|announce_address| {
            let announce_address = announce_address.trim().to_string();
            if announce_address.is_empty() {
                return None;
            }

            QrCode::new(announce_address).ok().map(|code| {
                code.render::<svg::Color<'_>>()
                    .min_dimensions(50, 50)
                    .dark_color(svg::Color("#111111"))
                    .light_color(svg::Color("#ffffff"))
                    .build()
            })
        })
    };

    let show_peers = move || {
        if app_context.get_peers.get().is_empty() {
            Either::Left(view! {
                <div class="empty-state">
                    <strong>"No peers connected"</strong>
                    <p>"Paste an announce address above or connect to a known peer below."</p>
                </div>
            })
        } else {
            Either::Right(view! {
                <div>
                    <For
                        each=move || app_context.get_peers.get()
                        key=|name| name.clone()
                        children=move |name| view! { <Peer name is_self=false /> }
                    />
                </div>
            })
        }
    };

    let known_peers_iter = move || {
        let connected = app_context.get_peers.get();
        let pending = pending_peers.get();
        known_peers
            .get()
            .into_iter()
            .filter(|announce_address| {
                !connected.contains(&announce_address.name)
                    && !pending.contains(&announce_address.to_string())
            })
            .collect::<Vec<_>>()
    };

    let show_pending_peers = move || {
        view! {
            <For
                each=move || pending_peers.get()
                key=|announce_address| announce_address.clone()
                children=move |announce_address| {
                    view! {
                        <Flex class="pending-peer">
                            <Spinner label=announce_address size=SpinnerSize::Small />
                        </Flex>
                    }
                }
            />
        }
    };

    let input_value = RwSignal::new(String::new());

    let app_context_for_add_peer = app_context.clone();
    let add_peer = move |e: leptos::ev::SubmitEvent| {
        e.prevent_default();
        let announce_payload = input_value.get();
        let announce_payload = announce_payload.trim();
        if !announce_payload.is_empty() {
            app_context_for_add_peer.connect(announce_payload.to_string());
            app_context_for_add_peer
                .set_pending_peers
                .update(|pending_peers| {
                    pending_peers.insert(announce_payload.to_string());
                });
        }

        input_value.set(String::new());
    };

    let show_known_peers = move || {
        let known_peers = known_peers_iter();
        if known_peers.is_empty() {
            view! {
                <div class="empty-state empty-state--quiet">
                    <strong>"No known peers"</strong>
                    <p>"Peers you have seen before will appear here for quick reconnects."</p>
                </div>
            }
            .into_any()
        } else {
            let app_context = app_context.clone();
            view! {
                <ul class="known-peers-list">
                    <For
                        each=move || known_peers.clone()
                        key=|announce_address: &AnnounceAddress| announce_address.to_string()
                        children=move |announce_address| {
                            let announce_payload = announce_address.to_string();
                            let connect_payload = announce_payload.clone();
                            let connect_label = known_peer_connect_label(&announce_address);
                            let app_context = app_context.clone();
                            view! {
                                <li class="known-peer-item">
                                    <AnnounceAddressView announce_address />
                                    <Button
                                        size=ButtonSize::Small
                                        on:click=move |_| {
                                            app_context.connect(connect_payload.clone());
                                            app_context
                                                .set_pending_peers
                                                .update(|pending_peers| {
                                                    pending_peers.insert(connect_payload.clone());
                                            });
                                        }
                                    >
                                        {connect_label}
                                    </Button>
                                </li>
                            }
                        }
                    />
                </ul>
            }
            .into_any()
        }
    };

    let announce = move || {
        announce_address
            .get()
            .unwrap_or("No announce address".to_string())
    };

    let copy_to_clipboard = move |_| {
        wasm_bindgen_futures::spawn_local(async move {
            let Some(window) = web_sys::window() else {
                log::warn!("Cannot copy announce address: window is unavailable");
                return;
            };
            let clipboard = window.navigator().clipboard();
            let promise = clipboard.write_text(
                &announce_address
                    .get_untracked()
                    .unwrap_or("Cannot get signal".to_string()),
            );
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(_) => log::info!("Copied to clipboard"),
                Err(err) => log::warn!("Cannot copy announce address: {:?}", err),
            }
        });
    };

    view! {
        <div class="announce-card">
            {move || {
                qr_svg()
                    .map(|qr_svg| {
                        view! { <div class="announce-card__qr" inner_html=qr_svg /> }
                    })
            }} <div class="announce-card__body">
                <span class="announce-card__label">"Announce address"</span>
                <div class="announce-card__value-row">
                    <code class="announce-card__value">{announce}</code>
                    <Popover trigger_type=PopoverTriggerType::Click>
                        <PopoverTrigger slot>
                            <span title="Copy to clipboard">
                                <Button
                                    icon=icondata::ChCopy
                                    on:click=copy_to_clipboard
                                    size=ButtonSize::Small
                                />
                            </span>
                        </PopoverTrigger>
                        "Copied"
                    </Popover>
                </div>
            </div>
        </div>
        <form class="peer-connect-form" on:submit=add_peer>
            <Flex class="form-row form-row--peer-connect">
                <Input value=input_value placeholder="Paste an announce address">
                    <InputPrefix slot>
                        <Icon icon=icondata::AiUserOutlined />
                    </InputPrefix>
                </Input>
                <Button button_type=ButtonType::Submit>"Add peer"</Button>
            </Flex>
        </form>
        {show_pending_peers}
        <h2 class="text-xl">"Connected peers"</h2>
        {show_peers}
        <h2 class="text-xl">"Known peers"</h2>
        {show_known_peers}
    }
}

fn known_peer_connect_label(announce_address: &AnnounceAddress) -> &'static str {
    match announce_address.connection_details {
        PeerConnectionDetails::NoNat(_) => "Connect",
        PeerConnectionDetails::Asymmetric(_) | PeerConnectionDetails::Symmetric(_) => {
            "Attempt connect"
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::{
        file::{DownloadStatus, File},
        AppContext,
    };
    use gloo_timers::future::sleep;
    use leptos::mount::mount_to;
    use leptos::wasm_bindgen::JsCast;
    use std::time::Duration;
    use thaw::ConfigProvider;
    use wasm_bindgen_test::wasm_bindgen_test;
    use web_sys::HtmlElement;

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
    fn filters_connected_peers_from_known_peers_list() {
        let host = mount_host();
        let mut connected = HashSet::new();
        connected.insert("asphericKingCrab".to_string());
        connected.insert("bob".to_string());
        let app_context = AppContext::for_tests();
        app_context.set_peers.set(connected);
        let (announce_address, _set_announce_address) = signal(None::<String>);
        let (pending_peers, _set_pending_peers) = signal(HashSet::<String>::new());
        let (known_peers, _set_known_peers) = signal(vec![
            AnnounceAddress::from_string("asphericKingCrabEJLLAHEK2".to_string()).unwrap(),
            AnnounceAddress::from_string("amberCloudYakG1/LAHFY0".to_string()).unwrap(),
            AnnounceAddress::from_string("bobbyxjNkTQ1".to_string()).unwrap(),
        ]);

        let handle = mount_to(host.clone(), move || {
            provide_context(app_context.clone());
            view! {
                <ConfigProvider>
                    <Peers announce_address pending_peers known_peers />
                </ConfigProvider>
            }
        });

        let known_list = host
            .query_selector(".known-peers-list")
            .expect("query should succeed")
            .expect("known peers list should exist");
        let known_text = known_list.text_content().unwrap_or_default();
        let all_text = host.text_content().unwrap_or_default();

        assert!(known_text.contains("amberCloudYak"));
        assert!(known_text.contains("bobby"));
        assert!(!known_text.contains("asphericKingCrab"));
        assert!(all_text.contains("asphericKingCrab"));

        drop(handle);
        host.remove();
    }

    #[test]
    fn labels_nat_peers_as_attempt_connect() {
        let direct_peer = AnnounceAddress {
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:1234".parse().unwrap()),
            name: "DirectPeer".to_string(),
        };
        let nat_peer = AnnounceAddress {
            connection_details: PeerConnectionDetails::Symmetric("8.8.8.8".parse().unwrap()),
            name: "NatPeer".to_string(),
        };

        assert_eq!(known_peer_connect_label(&direct_peer), "Connect");
        assert_eq!(known_peer_connect_label(&nat_peer), "Attempt connect");
    }

    #[wasm_bindgen_test]
    async fn hides_downloaded_children_until_parent_directory_is_expanded() {
        let host = mount_host();
        let app_context = AppContext::for_tests();
        let peer_name = "asphericKingCrab".to_string();
        let parent_path = PeerPath {
            peer_name: peer_name.clone(),
            path: "albums".to_string(),
        };
        let child_path = PeerPath {
            peer_name: peer_name.clone(),
            path: "albums/song.mp3".to_string(),
        };

        app_context.set_files.update(|files| {
            files.insert(
                parent_path.clone(),
                File {
                    name: parent_path.path.clone(),
                    peer_name: peer_name.clone(),
                    size: Some(1024),
                    is_dir: Some(true),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
            );
        });

        let app_context_for_mount = app_context.clone();
        let peer_name_for_mount = peer_name.clone();
        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Peer name=peer_name_for_mount.clone() is_self=false />
                </ConfigProvider>
            }
        });

        let initial_text = host.text_content().unwrap_or_default();
        assert!(initial_text.contains("albums"));
        assert!(!initial_text.contains("song.mp3"));

        app_context.set_files.update(|files| {
            files.insert(
                child_path.clone(),
                File {
                    name: child_path.path.clone(),
                    peer_name: child_path.peer_name.clone(),
                    size: Some(512),
                    is_dir: Some(false),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Downloaded(2000)),
                    request: RwSignal::new(None),
                },
            );
        });

        sleep(Duration::from_millis(0)).await;

        let collapsed_text = host.text_content().unwrap_or_default();
        assert!(collapsed_text.contains("albums"));
        assert!(!collapsed_text.contains("song.mp3"));

        let parent_row = host
            .query_selector("tr")
            .expect("query should succeed")
            .expect("parent row should exist");
        parent_row
            .dyn_into::<web_sys::HtmlElement>()
            .expect("row should be an HtmlElement")
            .click();

        sleep(Duration::from_millis(0)).await;

        let expanded_text = host.text_content().unwrap_or_default();
        assert!(expanded_text.contains("song.mp3"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    async fn nested_subdirectories_stay_collapsed_when_parent_expands() {
        let host = mount_host();
        let app_context = AppContext::for_tests();
        let peer_name = "asphericKingCrab".to_string();

        app_context.set_files.update(|files| {
            for file in [
                File {
                    name: "albums".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(1024),
                    is_dir: Some(true),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
                File {
                    name: "albums/live".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(512),
                    is_dir: Some(true),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
                File {
                    name: "albums/live/song.mp3".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(256),
                    is_dir: Some(false),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Downloaded(2001)),
                    request: RwSignal::new(None),
                },
            ] {
                files.insert(
                    PeerPath {
                        peer_name: file.peer_name.clone(),
                        path: file.name.clone(),
                    },
                    file,
                );
            }
        });

        let app_context_for_mount = app_context.clone();
        let peer_name_for_mount = peer_name.clone();
        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Peer name=peer_name_for_mount.clone() is_self=false />
                </ConfigProvider>
            }
        });

        let initial_text = host.text_content().unwrap_or_default();
        assert!(initial_text.contains("albums"));
        assert!(!initial_text.contains("live"));
        assert!(!initial_text.contains("song.mp3"));

        host.query_selector("tr")
            .expect("query should succeed")
            .expect("top-level row should exist")
            .dyn_into::<web_sys::HtmlElement>()
            .expect("row should be an HtmlElement")
            .click();

        sleep(Duration::from_millis(0)).await;

        let expanded_parent_text = host.text_content().unwrap_or_default();
        assert!(expanded_parent_text.contains("albums"));
        assert!(expanded_parent_text.contains("live"));
        assert!(!expanded_parent_text.contains("song.mp3"));

        drop(handle);
        host.remove();
    }

    #[wasm_bindgen_test]
    async fn missing_intermediate_directory_keeps_downloaded_file_hidden_until_loaded() {
        let host = mount_host();
        let app_context = AppContext::for_tests();
        let peer_name = "asphericKingCrab".to_string();

        app_context.set_files.update(|files| {
            files.insert(
                PeerPath {
                    peer_name: peer_name.clone(),
                    path: "albums".to_string(),
                },
                File {
                    name: "albums".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(1024),
                    is_dir: Some(true),
                    is_expanded: RwSignal::new(true),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
            );
            files.insert(
                PeerPath {
                    peer_name: peer_name.clone(),
                    path: "albums/live/song.mp3".to_string(),
                },
                File {
                    name: "albums/live/song.mp3".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(256),
                    is_dir: Some(false),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Downloaded(2002)),
                    request: RwSignal::new(None),
                },
            );
        });

        let app_context_for_mount = app_context.clone();
        let peer_name_for_mount = peer_name.clone();
        let handle = mount_to(host.clone(), move || {
            provide_context(app_context_for_mount.clone());
            view! {
                <ConfigProvider>
                    <Peer name=peer_name_for_mount.clone() is_self=false />
                </ConfigProvider>
            }
        });

        let text_before_intermediate = host.text_content().unwrap_or_default();
        assert!(text_before_intermediate.contains("albums"));
        assert!(!text_before_intermediate.contains("song.mp3"));

        app_context.set_files.update(|files| {
            files.insert(
                PeerPath {
                    peer_name: peer_name.clone(),
                    path: "albums/live".to_string(),
                },
                File {
                    name: "albums/live".to_string(),
                    peer_name: peer_name.clone(),
                    size: Some(512),
                    is_dir: Some(true),
                    is_expanded: RwSignal::new(false),
                    download_status: RwSignal::new(DownloadStatus::Nothing),
                    request: RwSignal::new(None),
                },
            );
        });

        sleep(Duration::from_millis(0)).await;

        let text_with_collapsed_intermediate = host.text_content().unwrap_or_default();
        assert!(text_with_collapsed_intermediate.contains("live"));
        assert!(!text_with_collapsed_intermediate.contains("song.mp3"));

        host.query_selector("tr:nth-of-type(2)")
            .expect("query should succeed")
            .expect("intermediate row should exist")
            .dyn_into::<web_sys::HtmlElement>()
            .expect("row should be an HtmlElement")
            .click();

        sleep(Duration::from_millis(0)).await;

        let text_after_intermediate_expand = host.text_content().unwrap_or_default();
        assert!(text_after_intermediate_expand.contains("song.mp3"));

        drop(handle);
        host.remove();
    }
}
