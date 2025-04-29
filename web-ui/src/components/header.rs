use crate::{display_bytes, peer::Peer, FilesSignal, PeerPath};
use leptos::prelude::*;
use leptos_meta::Style;
use leptos_router::hooks::use_navigate;
use std::collections::HashMap;
use thaw::*;

#[component]
pub fn HdpHeader(
    topics: ReadSignal<Vec<(String, bool)>>,
    peers: ReadSignal<HashMap<String, Peer>>,
    shares: ReadSignal<Option<Peer>>,
) -> impl IntoView {
    let selected_value = RwSignal::new("peers".to_string());

    let files = use_context::<FilesSignal>().unwrap().0;

    let shared_files_size = move || match shares.get() {
        Some(me) => {
            match files.get().get(&PeerPath {
                peer_name: me.name,
                path: "".to_string(),
            }) {
                Some(file) => display_bytes(file.size.unwrap_or_default()),
                None => display_bytes(0),
            }
        }
        None => display_bytes(0),
    };

    let navigate1 = use_navigate();
    let navigate2 = use_navigate();
    let navigate3 = use_navigate();
    let navigate4 = use_navigate();

    view! {
        <Style id="hdp-header">
            "
                .hdp-header {
                    height: 64px;
                    display: flex;
                    align-items: center;
                    justify-content: space-between;
                    padding: 0 20px;
                    z-index: 1000;
                    position: relative;
                }
                .hover-invert {
                    transition: filter 0.3s ease-in-out;
                }
                .hover-invert:hover {
                    filter: invert(1);
                }
                .hdp-header__menu-mobile {
                    display: none !important;
                }
                .hdp-header__right-btn .thaw-select {
                    width: 60px;
                }
                @media screen and (max-width: 600px) {
                    .hdp-header {
                        padding: 0 8px;
                    }
                    .hdp-name {
                        display: none;
                    }
                }
                @media screen and (max-width: 1200px) {
                    .hdp-header__right-btn {
                        display: none !important;
                    }
                    .hdp-header__menu-mobile {
                        display: inline-block !important;
                    }
                }
            "
        </Style>
        <LayoutHeader class="hdp-header">
            <Space>
                <img
                    class="hover-invert"
                    src="hdd.png"
                    alt="hard drive"
                    width="60"
                    title="harddrive-party"
                />
                <TabList selected_value>
                    <Tab
                        value="topics"
                        on:click=move |_| {
                            navigate1("/topics", Default::default());
                        }
                    >

                        <Flex>
                            <Icon icon=icondata::FaNetworkWiredSolid/>
                            {" Topics"}
                            <Badge>{move || { topics.get().len() }}</Badge>
                        </Flex>
                    </Tab>
                    <Tab
                        value="shares"
                        on:click=move |_| {
                            navigate2("/shares", Default::default());
                        }
                    >

                        <Flex>
                            <Icon icon=icondata::AiHeartFilled/>
                            " Shares"
                            <Badge>{shared_files_size}</Badge>
                        </Flex>
                    </Tab>
                    <Tab
                        value="peers"
                        on:click=move |_| {
                            navigate3("/peers", Default::default());
                        }
                    >

                        <Flex>
                            <Icon icon=icondata::FaUsersSolid/>
                            " Peers"
                            <Badge>{move || { peers.get().len() }}</Badge>
                        </Flex>
                    </Tab>
                    <Tab
                        value="transfers"
                        on:click=move |_| {
                            navigate4("/transfers", Default::default());
                        }
                    >

                        <Flex>
                            <Icon icon=icondata::LuArrowUpDown/>
                            " Transfers"
                        </Flex>
                    </Tab>
                </TabList>
            </Space>
        </LayoutHeader>
    }
}
