use crate::{display_bytes, FilesSignal, PeerPath};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use std::collections::HashSet;
use thaw::*;

#[component]
pub fn HdpHeader(
    peers: ReadSignal<HashSet<String>>,
    own_name: ReadSignal<Option<String>>,
) -> impl IntoView {
    let selected_value = RwSignal::new("peers".to_string());

    let files = use_context::<FilesSignal>().unwrap().0;

    let shared_files_size = move || match own_name.get() {
        Some(me) => {
            match files.get().get(&PeerPath {
                peer_name: me,
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

    view! {
        <LayoutHeader class="hdp-header">
            <Flex>
                <img
                    class="hover-invert"
                    src="hdd.png"
                    alt="hard drive"
                    width="60"
                    title="harddrive-party"
                />
                <TabList class="tab-list" selected_value>
                    <Flex>
                        <Tab
                            value="shares"
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
                            value="peers"
                            on:click=move |_| {
                                navigate2("/peers", Default::default());
                            }
                        >

                            <Flex>
                                <Icon icon=icondata::FaUsersSolid />
                                " Peers"
                                <Badge>{move || { peers.get().len() }}</Badge>
                            </Flex>
                        </Tab>
                        <Tab
                            value="transfers"
                            on:click=move |_| {
                                navigate3("/transfers", Default::default());
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
