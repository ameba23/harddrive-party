use crate::{display_bytes, AppContext, PeerPath};
use harddrive_party_shared::ui_messages::PeerInfo;
use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate};
use std::collections::HashSet;
use thaw::*;

#[component]
pub fn HdpHeader(
    peers: ReadSignal<HashSet<PeerInfo>>,
    own_peer: ReadSignal<Option<PeerInfo>>,
) -> impl IntoView {
    let location = use_location();
    let pathname = location.pathname;
    let selected_value = RwSignal::new(pathname.get_untracked());

    Effect::new(move || {
        selected_value.set(pathname.get());
    });

    let files = use_context::<AppContext>().unwrap().get_files;

    let shared_files_size = move || match own_peer.get() {
        Some(me) => {
            match files.get().get(&PeerPath {
                peer: me,
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
        <LayoutHeader class="hdp-header">
            <Flex class="header-shell">
                <div class="brand-lockup" title="harddrive-party">
                    <img class="hover-invert" src="hdd.png" alt="" width="54" />
                    <span class="brand-title">"harddrive party"</span>
                </div>
                <TabList class="tab-list" selected_value>
                    <Flex>
                        <Tab
                            value="/shares"
                            on:click=move |_| {
                                navigate1("/shares", Default::default());
                            }
                        >

                            <Flex>
                                <Icon icon=icondata::AiHeartFilled />
                                " Shares"
                                <Badge>{shared_files_size}</Badge>
                            </Flex>
                        </Tab>
                        <Tab
                            value="/"
                            on:click=move |_| {
                                navigate2("/", Default::default());
                            }
                        >

                            <Flex>
                                <Icon icon=icondata::FaUsersSolid />
                                " Peers"
                                <Badge>{move || { peers.get().len() }}</Badge>
                            </Flex>
                        </Tab>
                        <Tab
                            value="/search"
                            on:click=move |_| {
                                navigate3("/search", Default::default());
                            }
                        >

                            <Flex>
                                <Icon icon=icondata::AiSearchOutlined />
                                " Search"
                            </Flex>
                        </Tab>
                        <Tab
                            value="/transfers"
                            on:click=move |_| {
                                navigate4("/transfers", Default::default());
                            }
                        >

                            <Flex>
                                <Icon icon=icondata::LuArrowUpDown />
                                " Transfers"
                            </Flex>
                        </Tab>
                    </Flex>
                </TabList>
            </Flex>
        </LayoutHeader>
    }
}
